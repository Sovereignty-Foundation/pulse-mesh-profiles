#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""The architecture, executable: the rules the tree keeps, checked from `cargo metadata` and the sources.

The rules, each with the reason it exists:

1. tiers: a crate depends only on tiers below its own (`pulse/` 0, `core/` 1, `mesh/` 2, `rings/` 3); the peripheral
   surface depends on all of them and is depended on by none. Normal and build dependencies count; dev-dependencies
   do not, since a test may reach up (a client's tests run an in-process daemon).
2. rings: a ring's `core`, `functions` and `stages` crates are depended on only from inside their ring; the hub is
   the ring's one import point (normal and build dependencies).
3. consumers: every library crate has a dependent, or is a binary, an example, a tool or a plugin (an optional
   backend, a leaf by design), or declares `package.metadata.pulse-mesh.status` (ADR-0014), or is named in the
   workspace manifest's `workspace.metadata.pulse-mesh.exported` (a library this workspace holds for the
   repositories built on it: the standard's registry macro, which only the ring hubs use); a library nothing uses
   and nothing accounts for is a mistake.
4. manifests: the module table (`dev_functions::core_ops::modules`) and the four hubs' `register_stages!`
   manifests agree: every table entry of the form `crate::module` in a functions or stages layer is a manifest
   entry, or is excluded here with its reason (a core-layer entry is re-exported by its hub as `core`, not
   registered); every manifest path names a workspace crate and a module it has, as a module file or a `pub use`
   or `pub mod` in its `lib.rs`.
5. growth: every public error enum and configuration struct outside tier 0 is `#[non_exhaustive]` (ARC-5), or is
   excluded here with its reason.
6. pins: no member restates a dependency the workspace table pins; it takes it with `workspace = true`.
7. partial: a crate whose sources return a deferred error (an `Err` of a variant named `IntegrationDeferred` or
   `NotYetImplemented`) declares `package.metadata.pulse-mesh.status` `partial`, or `pre-integration`, and its crate
   docs open with the matching `**Status: ...**` line; a crate that declares `partial` returns such an error (ADR-0017).
   A reader, an evaluator or a tool can then tell an entry point that does not work yet from a finished one, and the
   label cannot outlive the gap it names. Examples are left out: they demonstrate a crate, and its status is its own.

Run from the workspace root. `--self-test` plants one violation per rule in a synthetic tree and expects the rule
to refuse it, so the check checks itself.

The check reads the integrated tree and each of the repositories it splits into (the standard, the daemon, the
profiles) alike: a tier prefix that is absent is no tier, a hub source that is absent is no hub, and the module table
is checked only where it lives (the daemon's dev ring), against the crates the workspace has.
"""
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib

ROOT = pathlib.Path(os.environ.get('PMS_ROOT') or pathlib.Path(__file__).resolve().parents[1])

# path prefix -> tier, for the integrated tree and for the split repositories (`dev_ring/`, `tools/`, `examples/`,
# `clients/`, `plugins/`, `bindings/` are the daemon's, the standard's and the profiles' own top levels)
TIERS = [('pulse/', 0), ('core/', 1), ('mesh/', 2), ('rings/', 3), ('dev_ring/', 3), ('peripheral/', 4), ('tools/', 4),
         ('examples/', 4), ('clients/', 4), ('plugins/', 4), ('bindings/', 4), ('fuzz/', 4)]
LEAF_PREFIXES = ('peripheral/tools/', 'peripheral/examples/', 'peripheral/plugins/', 'pulse/pulse-spec/tools/',
                 'tools/', 'examples/', 'plugins/', 'clients/', 'bindings/', 'fuzz/')
RING_LAYERS = ('core', 'functions', 'stages')
MODULE_TABLE = 'rings/dev_ring/dev-functions/src/core_ops/modules.rs'
HUBS = {
    'dev_ring': ('rings/dev_ring/dev-hub/src/lib.rs', 'dev_ring/dev-hub/src/lib.rs'),
    'enterprise_ring': ('rings/enterprise_ring/enterprise-hub/src/lib.rs',),
    'hft_ring': ('rings/hft_ring/hft-hub/src/lib.rs',),
    'robotics_ring': ('rings/robotics_ring/robotics-hub/src/lib.rs',),
}
MODULE_TABLES = (MODULE_TABLE, 'dev_ring/dev-functions/src/core_ops/modules.rs')
# Module-table entries that are not ring stages or functions, with the reason each is not in a manifest.
TABLE_EXCLUSIONS = {
    'hft_stages::numa': 'stage-layer support for multilane and spmc, not registered as a stage (hft-hub says so)',
    'pms-telemetry-export': 'a peripheral tool, not a ring module',
    'pms-crc32c': 'a core crate, not a ring module',
    'mesh-broadcast': 'a mesh crate, not a ring module',
}
# Public error enums and configuration structs outside tier 0 that stay exhaustive, with the reason.
GROWTH_EXCLUSIONS = {
    'Error': 'core_region::shm::Error is exhaustive on purpose: the crates that map it must fail to compile when a variant is added (ADR-0013)',
}
ERROR_ENUM = re.compile(r'^pub enum (\w*Error\w*)\s*(<[^>]*>)?\s*\{', re.M)
DEFERRED = re.compile(r'Err\(\s*(?:[A-Za-z_][\w:]*::)?(?:IntegrationDeferred|NotYetImplemented)\b')
STATUS_LINES = {'partial': '**Status: partial.**', 'pre-integration': '**Status: pre-integration'}
EXAMPLE_PREFIXES = ('peripheral/examples/', 'examples/')
CONFIG_STRUCT = re.compile(r'^pub struct (\w*(?:Config|Policy))\s*(<[^>]*>)?\s*\{', re.M)


class Tree:
    """What the rules read: the workspace's packages and the files the rules look into."""

    def __init__(self, packages, files):
        # packages: name -> {path, deps: [(name, kind)], targets: [kind...], status}
        self.packages = packages
        # files: relative path -> text, for the module table, the hub sources, the crate sources and manifests
        self.files = files

    @classmethod
    def from_workspace(cls, root):
        meta = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=root))
        members = {}
        for pkg in meta['packages']:
            manifest = pathlib.Path(pkg['manifest_path'])
            rel = manifest.parent.relative_to(root).as_posix()
            status = ((pkg.get('metadata') or {}).get('pulse-mesh') or {}).get('status', '')
            deps = [(d['name'], d['kind'] or 'normal') for d in pkg['dependencies']]
            targets = [k for t in pkg['targets'] for k in t['kind']]
            members[pkg['name']] = {'path': rel, 'deps': deps, 'targets': targets, 'status': status}
        files = {}
        for rel in [*MODULE_TABLES, *(h for hs in HUBS.values() for h in hs), 'Cargo.toml']:
            if (root / rel).exists():
                files[rel] = (root / rel).read_text()
        for name, pkg in members.items():
            files[f"{pkg['path']}/Cargo.toml"] = (root / pkg['path'] / 'Cargo.toml').read_text()
            for src in sorted((root / pkg['path'] / 'src').rglob('*.rs')):
                files[src.relative_to(root).as_posix()] = src.read_text()
        return cls(members, files)

    def tier(self, name):
        path = self.packages[name]['path'] + '/'
        for prefix, tier in TIERS:
            if path.startswith(prefix):
                return tier
        raise ValueError(f'{name} at {path} is in no tier')

    def ring(self, name):
        """The ring a crate belongs to, `rings/<ring>/` or the daemon's `dev_ring/`, or None."""
        path = self.packages[name]['path']
        parts = path.split('/')
        if parts[0] == 'rings' and len(parts) > 1:
            return parts[1]
        return parts[0] if parts[0].endswith('_ring') else None

    def is_member(self, name):
        return name in self.packages


def check_tiers(tree):
    for name, pkg in tree.packages.items():
        for dep, kind in pkg['deps']:
            if kind == 'dev' or not tree.is_member(dep):
                continue
            if tree.tier(dep) > tree.tier(name):
                yield f'tiers: {name} (tier {tree.tier(name)}) depends on {dep} (tier {tree.tier(dep)}) as a {kind} dependency'


def check_rings(tree):
    for name, pkg in tree.packages.items():
        for dep, kind in pkg['deps']:
            if kind == 'dev' or not tree.is_member(dep):
                continue
            ring = tree.ring(dep)
            if ring is None or not dep.split('-')[-1] in RING_LAYERS:
                continue
            if tree.ring(name) != ring:
                yield f'rings: {name} depends on {dep}, a {dep.split("-")[-1]} layer of {ring}; the hub is the import point'


def exported(tree):
    """The libraries the workspace manifest declares it holds for the repositories built on it."""
    meta = tomllib.loads(tree.files['Cargo.toml']).get('workspace', {}).get('metadata', {}).get('pulse-mesh', {})
    return set(meta.get('exported', []))


def check_consumers(tree):
    dependents = {name: set() for name in tree.packages}
    for name, pkg in tree.packages.items():
        for dep, _ in pkg['deps']:
            if dep in dependents:
                dependents[dep].add(name)
    held = exported(tree)
    for name in sorted(held):
        if name not in tree.packages:
            yield f'consumers: the workspace manifest exports {name}, which is no crate of this workspace'
        elif dependents[name]:
            yield f'consumers: the workspace manifest exports {name}, which {", ".join(sorted(dependents[name]))} depends on here; an export is a library nothing here uses'
    for name, pkg in tree.packages.items():
        if dependents[name] or pkg['status'] == 'pre-integration' or name in held:
            continue
        if 'bin' in pkg['targets'] or 'example' in pkg['targets']:
            continue
        if pkg['path'].startswith(LEAF_PREFIXES):
            continue
        yield f'consumers: {name} is a library nothing depends on and no status accounts for (ADR-0014)'


def manifest_paths(tree):
    """The `crate::module` paths each hub present in this tree registers."""
    paths = {}
    for ring, rels in HUBS.items():
        for rel in rels:
            text = tree.files.get(rel)
            if text is None:
                continue
            m = re.search(r'register_stages!\s*\{(.*?)\n\}', text, re.S)
            paths[ring] = re.findall(r'(\w+::\w+)', m.group(1)) if m else []
    return paths


def check_manifests(tree):
    crate_of = {name.replace('-', '_'): pkg for name, pkg in tree.packages.items()}
    table = []
    for rel in MODULE_TABLES:
        if rel in tree.files:
            table = re.findall(r'crate_name:\s*"([^"]+)"', tree.files[rel])
    registered = {p for ps in manifest_paths(tree).values() for p in ps}
    for entry in table:
        if entry in TABLE_EXCLUSIONS or entry.split('::')[0].endswith('_core'):
            continue
        # a table entry for a crate this workspace does not hold is another repository's to register
        if '::' in entry and entry.split('::')[0] not in crate_of:
            continue
        if entry not in registered:
            yield f'manifests: module-table entry {entry} is registered by no hub and excluded by no reason'
    for ring, paths in manifest_paths(tree).items():
        for path in paths:
            krate, module = path.split('::')
            pkg = crate_of.get(krate)
            if pkg is None:
                yield f'manifests: {ring} registers {path}, and {krate} is no workspace crate'
                continue
            files = (f"{pkg['path']}/src/{module}.rs", f"{pkg['path']}/src/{module}/mod.rs")
            lib = tree.files.get(f"{pkg['path']}/src/lib.rs", '')
            declared = re.search(rf'^pub (?:use \S+ as {module}|mod {module});', lib, re.M)
            if not any(f in tree.files for f in files) and not declared:
                yield f'manifests: {ring} registers {path}, and {krate} has no module {module}'


def check_growth(tree):
    for name, pkg in tree.packages.items():
        if tree.tier(name) == 0 or pkg['path'].startswith('peripheral/'):
            continue
        for rel, text in tree.files.items():
            if not rel.startswith(pkg['path'] + '/src/'):
                continue
            for pattern in (ERROR_ENUM, CONFIG_STRUCT):
                for m in pattern.finditer(text):
                    item = m.group(1)
                    if item in GROWTH_EXCLUSIONS:
                        continue
                    above = text[:m.start()].split('\n')[-12:]
                    if not any(line.strip() == '#[non_exhaustive]' for line in above):
                        yield f'growth: {rel} declares pub {item} without #[non_exhaustive]'


def check_pins(tree):
    pinned = set(tomllib.loads(tree.files['Cargo.toml']).get('workspace', {}).get('dependencies', {}))
    for name, pkg in tree.packages.items():
        manifest = tomllib.loads(tree.files[f"{pkg['path']}/Cargo.toml"])
        for table in ('dependencies', 'dev-dependencies', 'build-dependencies'):
            for dep, spec in manifest.get(table, {}).items():
                if dep not in pinned:
                    continue
                if isinstance(spec, dict) and spec.get('workspace') is True:
                    continue
                yield f'pins: {name} restates {dep} in [{table}]; the workspace table pins it'


def crate_docs(tree, pkg):
    """The crate's `//!` documentation, from its `lib.rs` or else its `main.rs`."""
    for root in ('lib.rs', 'main.rs'):
        text = tree.files.get(f"{pkg['path']}/src/{root}")
        if text is not None:
            return '\n'.join(line for line in text.splitlines() if line.lstrip().startswith('//!'))
    return ''


def check_partial(tree):
    for name, pkg in sorted(tree.packages.items()):
        if pkg['path'].startswith(EXAMPLE_PREFIXES):
            continue
        returns = sorted(rel for rel, text in tree.files.items()
                         if rel.startswith(pkg['path'] + '/src/') and DEFERRED.search(text))
        status = pkg['status']
        if returns and status not in STATUS_LINES:
            yield f'partial: {name} returns a deferred error ({returns[0]}) and declares no status partial (ADR-0017)'
        elif returns and STATUS_LINES[status] not in crate_docs(tree, pkg):
            yield f'partial: {name} declares status {status}, and its crate docs do not open with {STATUS_LINES[status]!r} (ADR-0017)'
        elif status == 'partial' and not returns:
            yield f'partial: {name} declares status partial and returns no deferred error; drop the status (ADR-0017)'


RULES = [check_tiers, check_rings, check_consumers, check_manifests, check_growth, check_pins, check_partial]


def violations(tree):
    return [v for rule in RULES for v in rule(tree)]


def synthetic():
    """A small tree that keeps every rule, to plant violations in."""
    packages = {
        'pulse-canon': {'path': 'pulse/pulse-canon', 'deps': [], 'targets': ['lib'], 'status': ''},
        'core-ring': {'path': 'core/core-ring', 'deps': [('pulse-canon', 'normal'), ('libc', 'normal')], 'targets': ['lib'], 'status': ''},
        'mesh-hub': {'path': 'mesh/mesh-hub', 'deps': [('core-ring', 'normal')], 'targets': ['lib', 'bin'], 'status': ''},
        'mesh-spare': {'path': 'mesh/mesh-spare', 'deps': [], 'targets': ['lib'], 'status': 'pre-integration'},
        'dev-stages': {'path': 'rings/dev_ring/dev-stages', 'deps': [('core-ring', 'normal')], 'targets': ['lib'], 'status': ''},
        'dev-hub': {'path': 'rings/dev_ring/dev-hub', 'deps': [('dev-stages', 'normal')], 'targets': ['lib'], 'status': ''},
        'pms-cli': {'path': 'peripheral/tools/dev-cli', 'deps': [('dev-hub', 'normal'), ('mesh-hub', 'normal')], 'targets': ['bin'], 'status': ''},
        'pulse-wire-plug': {'path': 'peripheral/plugins/plug', 'deps': [('core-ring', 'normal')], 'targets': ['lib'], 'status': ''},
    }
    files = {
        'Cargo.toml': '[workspace.dependencies]\nlibc = "0.2"\n',
        MODULE_TABLE: 'crate_name: "dev_stages::conflation",\ncrate_name: "pms-crc32c",\ncrate_name: "dev_core::prefetch",\n',
        HUBS['dev_ring'][0]: 'register_stages! {\n    functions: [\n        dev_stages::basket,\n    ],\n    stages: [\n        dev_stages::conflation,\n    ],\n}\n',
        'rings/dev_ring/dev-stages/src/lib.rs': 'pub use other_basket as basket;\npub mod conflation;\n',
        HUBS['enterprise_ring'][0]: 'register_stages! {\n    functions: [],\n    stages: [],\n}\n',
        HUBS['hft_ring'][0]: 'register_stages! {\n    functions: [],\n    stages: [],\n}\n',
        HUBS['robotics_ring'][0]: 'register_stages! {\n    functions: [],\n    stages: [],\n}\n',
        'rings/dev_ring/dev-stages/src/conflation/mod.rs': '#[non_exhaustive]\npub enum ConflationError {\n}\n',
        'mesh/mesh-hub/src/lib.rs': '#[derive(Debug)]\n#[non_exhaustive]\npub struct ConnectionPolicy {\n}\n',
    }
    for name, pkg in packages.items():
        files[f"{pkg['path']}/Cargo.toml"] = '[dependencies]\n' + ('libc.workspace = true\n' if name == 'core-ring' else '')
    return Tree(packages, files)


def self_test():
    base = synthetic()
    assert not violations(base), violations(base)
    plants = []
    t = synthetic(); t.packages['core-ring']['deps'].append(('mesh-hub', 'normal')); plants.append(('tiers', t))
    t = synthetic(); t.packages['pms-cli']['deps'].append(('dev-stages', 'normal')); plants.append(('rings', t))
    t = synthetic(); t.packages['mesh-spare']['status'] = ''; plants.append(('consumers', t))
    t = synthetic(); t.packages['mesh-spare']['status'] = ''; t.files['Cargo.toml'] += '[workspace.metadata.pulse-mesh]\nexported = ["mesh-spare"]\n'; assert not violations(t), violations(t)
    t = synthetic(); t.files['Cargo.toml'] += '[workspace.metadata.pulse-mesh]\nexported = ["mesh-nowhere"]\n'; plants.append(('consumers', t))
    t = synthetic(); t.files['Cargo.toml'] += '[workspace.metadata.pulse-mesh]\nexported = ["core-ring"]\n'; plants.append(('consumers', t))
    t = synthetic(); t.files[MODULE_TABLE] += 'crate_name: "dev_stages::dedupe",\n'; plants.append(('manifests', t))
    t = synthetic(); t.files[HUBS['dev_ring'][0]] = t.files[HUBS['dev_ring'][0]].replace('conflation', 'missing'); plants.append(('manifests', t))
    t = synthetic(); t.files['mesh/mesh-hub/src/lib.rs'] = 'pub struct ConnectionPolicy {\n}\n'; plants.append(('growth', t))
    t = synthetic(); t.files['core/core-ring/Cargo.toml'] = '[dependencies]\nlibc = "0.2"\n'; plants.append(('pins', t))
    t = synthetic(); t.files['core/core-ring/src/lib.rs'] = 'fn go() -> Result<(), E> {\n    Err(E::IntegrationDeferred)\n}\n'; plants.append(('partial', t))
    t = synthetic(); t.packages['core-ring']['status'] = 'partial'; t.files['core/core-ring/src/lib.rs'] = '//! Ring.\nfn go() -> Result<(), E> {\n    Err(E::NotYetImplemented("go"))\n}\n'; plants.append(('partial', t))
    t = synthetic(); t.packages['core-ring']['status'] = 'partial'; t.files['core/core-ring/src/lib.rs'] = '//! Ring.\n//!\n//! **Status: partial.** `go` is deferred.\nfn go() -> Result<(), E> {\n    Err(E::NotYetImplemented("go"))\n}\n'; assert not violations(t), violations(t)
    t = synthetic(); t.packages['core-ring']['status'] = 'partial'; t.files['core/core-ring/src/lib.rs'] = '//! **Status: partial.**\n'; plants.append(('partial', t))
    t = synthetic(); t.packages['mesh-spare']['status'] = 'partial'; t.files['mesh/mesh-spare/src/lib.rs'] = '//! **Status: partial.**\nfn go() -> Result<(), E> { Err(E::IntegrationDeferred) }\n'; plants.append(('consumers', t))
    for rule, tree in plants:
        found = violations(tree)
        assert found and all(v.startswith(rule + ':') for v in found), (rule, found)
    print(f'arch-check self-test: {len(plants)} planted violations refused')


def main():
    if '--self-test' in sys.argv[1:]:
        self_test()
        return 0
    tree = Tree.from_workspace(ROOT)
    found = violations(tree)
    for v in found:
        print(v)
    if found:
        print(f'arch-check: {len(found)} violation(s)')
        return 1
    print(f'arch-check: ok ({len(RULES)} rules, {len(tree.packages)} crates)')
    return 0


if __name__ == '__main__':
    sys.exit(main())
