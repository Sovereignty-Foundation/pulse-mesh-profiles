#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""One version for the whole tree.

Checks every version string in the tree against the workspace version,
`[workspace.package] version` in Cargo.toml, and exits 1 naming each one
that differs:

- the package version of every Cargo manifest (a workspace member may
  inherit it with `version.workspace = true`);
- in every Cargo.lock, the version of each package built from this tree,
  that is, each entry without a `source`;
- the version of every package.json, and of the root package in every
  package-lock.json;
- the `[project]` version of every pyproject.toml, in its PEP 440 form
  (1.0.0 stays 1.0.0; 1.1.0-alpha.1 is 1.1.0a1);
- the version the book displays, in book/theme/head.hbs.

It also checks every `rust-version` of a Cargo manifest, stated or
inherited from the workspace, against the toolchain rust-toolchain.toml
pins, and exits 1 naming each that disagrees: a rust-version agrees when
it is the toolchain's version to as many places as it gives, so 1.95 and
1.95.0 agree with 1.95.0, and 1.94 does not. The toolchain is the Rust
version the project supports and tests; `--set` changes neither.

Usage:
  scripts/check-versions.py              check
  scripts/check-versions.py --set V      write V into every place above,
                                         the workspace version first, then check

scripts/set-version.sh runs the second form, so the places are listed once,
here. LICENSE also names a release version; this script neither reads nor
writes it, so a version change fails the license gate until a person
updates LICENSE.

The files are the ones `git ls-files` lists (tracked, and untracked but
not ignored), so ignored build output such as target/ and node_modules/ is
never read. A tree without .git, such as an export, is walked instead:
every .gitignore on the way down is read as git reads it, and .git and
nested repositories are not entered. Both ways leave out a virtual
environment (a directory holding pyvenv.cfg), which git lists when no
.gitignore names it. In a git checkout the script does both and stops
with status 1, naming the files, if the walk finds other version files
than git lists, so every check in a checkout shows that the check
without .git reads the same files. (The one file the walk cannot find is
a tracked one that a .gitignore pattern matches, added with `git add
-f`; the comparison names any such version file.) Standard library
only; Python 3.9 or later.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SEMVER = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?(?:\+([0-9A-Za-z.-]+))?\Z")
PEP440_PRE = {"alpha": "a", "a": "a", "beta": "b", "b": "b", "rc": "rc", "c": "rc"}
HEAD_HBS = "book/theme/head.hbs"
TOOLCHAIN = "rust-toolchain.toml"
RUST_VERSION = re.compile(r"\d+\.\d+(?:\.\d+)?\Z")
# The names of the files that hold a version string, besides HEAD_HBS.
KINDS = ("Cargo.toml", "Cargo.lock", "package.json", "package-lock.json", "pyproject.toml")
HBS_VERSION = re.compile(r'(<meta name="pm-version" content=")([^"]*)(">)')


def pep440(version):
    """The PEP 440 form of a semantic version, or ValueError."""
    m = SEMVER.match(version)
    if not m:
        raise ValueError(f"{version!r} is not MAJOR.MINOR.PATCH[-PRE][+BUILD]")
    major, minor, patch, pre, build = m.groups()
    out = f"{int(major)}.{int(minor)}.{int(patch)}"
    if pre:
        parts = pre.split(".")
        kind = parts[0].lower()
        num = parts[1] if len(parts) > 1 else "0"
        if len(parts) > 2 or not num.isdigit() or (kind not in PEP440_PRE and kind != "dev"):
            raise ValueError(f"the pre-release {pre!r} of {version} has no PEP 440 form")
        out += f".dev{int(num)}" if kind == "dev" else f"{PEP440_PRE[kind]}{int(num)}"
    if build:
        out += "+" + build.replace("-", ".").lower()
    return out


def git_files():
    """The paths `git ls-files` lists, or None when ROOT has no .git."""
    if not (ROOT / ".git").exists():
        return None
    try:
        out = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=ROOT,
            capture_output=True,
            check=True,
        ).stdout
    except OSError as e:
        sys.exit(f"check-versions: the tree has a .git, but git does not run: {e}")
    except subprocess.CalledProcessError as e:
        sys.exit(f"check-versions: git ls-files failed: {os.fsdecode(e.stderr).strip()}")
    return {f for f in os.fsdecode(out).split("\0") if f}


def glob_regex(glob):
    """A .gitignore glob as a regular expression over a slash-separated
    path (gitignore(5)): `*`, `?` and a bracket expression never match a
    slash, `**/` matches any number of directories, a `**` that ends the
    glob everything below, and a backslash quotes the character after it."""
    out, i, n = [], 0, len(glob)
    while i < n:
        c = glob[i]
        if c == "*":
            j = i
            while j < n and glob[j] == "*":
                j += 1
            whole_part = (i == 0 or glob[i - 1] == "/") and (j == n or glob[j] == "/")
            if j - i >= 2 and whole_part and j == n:
                out.append(".*")
            elif j - i >= 2 and whole_part:
                out.append("(?:.*/)?")
                j += 1
            else:
                out.append("[^/]*")
            i = j
            continue
        if c == "?":
            out.append("[^/]")
        elif c == "[":
            # A `]` just after the `[` or the `[!` is a member, not the end.
            end = glob.find("]", i + (3 if glob[i + 1 : i + 2] in ("!", "^") else 2))
            if end < 0:
                out.append(re.escape(c))
            else:
                members = glob[i + 1 : end]
                negated = members[:1] in ("!", "^")
                members = members[1:] if negated else members
                members = re.sub(r"\\(.)", lambda m: re.escape(m.group(1)), members)
                members = members.replace("[", r"\[").replace("^", r"\^")
                out.append(f"(?!/)[{'^' if negated else ''}{members}]")
                i = end + 1
                continue
        elif c == "\\" and i + 1 < n:
            i += 1
            out.append(re.escape(glob[i]))
        else:
            out.append(re.escape(c))
        i += 1
    return "".join(out)


def gitignore_rules(text, base):
    """The patterns of the .gitignore in the directory `base` (relative to
    ROOT: "" or ending in "/"), in the file's order, as (base, regex,
    negated, directories only). As gitignore(5) has it: a pattern with a
    slash before its end is relative to the .gitignore's directory, any
    other matches a name at any depth below it; a trailing slash matches
    directories only; `!` re-includes; trailing spaces are dropped unless
    quoted with a backslash."""
    rules = []
    for line in text.splitlines():
        if line.startswith("#"):
            continue
        pattern = line.rstrip(" ")
        if pattern.endswith("\\") and len(pattern) < len(line):
            pattern += " "
        negated = pattern.startswith("!")
        if negated or pattern[:2] in ("\\!", "\\#"):
            pattern = pattern[1:]
        dirs_only = pattern.endswith("/")
        pattern = pattern.rstrip("/")
        if not pattern:
            continue
        anchored = "/" in pattern
        regex = glob_regex(pattern.lstrip("/"))
        if not anchored:
            regex = "(?:.*/)?" + regex
        rules.append((base, re.compile(regex + r"\Z", re.S), negated, dirs_only))
    return rules


def ignored(rules, path, is_dir):
    """Whether `path` is ignored: the last pattern that matches it decides,
    and a deeper .gitignore's patterns come after its parents'."""
    verdict = False
    for base, regex, negated, dirs_only in rules:
        if (is_dir or not dirs_only) and regex.match(path[len(base):]):
            verdict = not negated
    return verdict


def walked_files():
    """The paths `git ls-files` would list, found without git: everything
    under ROOT that no .gitignore on its way down ignores. .git, a nested
    repository (a directory with a .git of its own, which git lists as one
    entry) and a virtual environment are not entered."""
    found = set()

    def visit(directory, base, rules):
        try:
            entries = sorted(os.scandir(directory), key=lambda e: e.name)
        except OSError:
            return
        names = {e.name for e in entries}
        if base and ("pyvenv.cfg" in names or ".git" in names):
            return
        gitignore = os.path.join(directory, ".gitignore")
        if ".gitignore" in names and os.path.isfile(gitignore):
            with open(gitignore, encoding="utf-8", errors="surrogateescape") as f:
                rules = rules + gitignore_rules(f.read(), base)
        for e in entries:
            if e.name == ".git":
                continue
            path = base + e.name
            is_dir = e.is_dir(follow_symlinks=False)
            if ignored(rules, path, is_dir):
                continue
            if is_dir:
                visit(e.path, path + "/", rules)
            else:
                found.add(path)

    visit(str(ROOT), "", [])
    return found


def tree_files(paths):
    """The files among `paths`: each a file (a symbolic link to one counts)
    outside every virtual environment."""
    venvs = tuple(p[: -len("pyvenv.cfg")] for p in paths if p.endswith("/pyvenv.cfg"))
    return {p for p in paths if not p.startswith(venvs) and (ROOT / p).is_file()}


def version_files(paths):
    """The paths among `paths` that hold a version string, and the
    toolchain file, sorted."""
    return sorted(p for p in paths if p.rsplit("/", 1)[-1] in KINDS or p in (HEAD_HBS, TOOLCHAIN))


def list_tree():
    """The version files of the tree, and how they were found. In a git
    checkout git lists them, and the walk used without .git must find the
    same ones; the script stops if it does not."""
    walked = version_files(tree_files(walked_files()))
    listed = git_files()
    if listed is None:
        return walked, "found by walking the tree, which has no .git"
    listed = version_files(tree_files(listed))
    if listed != walked:
        lines = [f"  only git lists {p}" for p in sorted(set(listed) - set(walked))]
        lines += [f"  only the walk finds {p}" for p in sorted(set(walked) - set(listed))]
        sys.exit(
            "check-versions: walking the tree, as the check does without .git, "
            "finds other files than git lists:\n" + "\n".join(lines)
        )
    return listed, "listed by git, and the walk used without .git finds the same"


def table_key(lines, table, key):
    """Index and value of `key = "value"` directly in `[table]`, the string
    'workspace' when the key is inherited from the workspace, or None."""
    current = None
    plain = re.compile(rf'{re.escape(key)}\s*=\s*"([^"]*)"\s*(#.*)?\Z')
    inherited = re.compile(
        rf"{re.escape(key)}(\.workspace\s*=\s*true|\s*=\s*\{{\s*workspace\s*=\s*true\s*\}})\s*(#.*)?\Z"
    )
    for i, line in enumerate(lines):
        s = line.strip()
        if s.startswith("["):
            current = s.split("#", 1)[0].strip()
        elif current == f"[{table}]":
            if plain.match(s):
                return i, plain.match(s).group(1)
            if inherited.match(s):
                return i, "workspace"
    return None


def set_value(lines, i, value):
    """Replace the quoted value on line i, which `table_key` found."""
    lines[i] = re.sub(r'"[^"]*"', f'"{value}"', lines[i], count=1)


def workspace_version():
    lines = (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines(keepends=True)
    found = table_key(lines, "workspace.package", "version")
    if not found or found[1] == "workspace":
        sys.exit("check-versions: Cargo.toml has no [workspace.package] version")
    return found[1]


class Tree:
    """Visits every version string of the tree. Checking, it lists the ones
    that differ from the wanted version; writing, it makes them equal to it."""

    def __init__(self, want, write):
        self.want, self.want_py, self.write = want, pep440(want), write
        self.checked, self.differ, self.changed = 0, [], []

    def see(self, path, what, found, want):
        self.checked += 1
        if found != want:
            self.differ.append(f"{path}: {what} is {found}, not {want}")

    def save(self, path, old, new):
        if self.write and new != old:
            (ROOT / path).write_text(new, encoding="utf-8")
            self.changed.append(path)

    def cargo_toml(self, path):
        text = (ROOT / path).read_text(encoding="utf-8")
        lines = text.splitlines(keepends=True)
        if path == "Cargo.toml":
            found = table_key(lines, "workspace.package", "version")
            if self.write:
                set_value(lines, found[0], self.want)
            else:
                self.see(path, "the workspace version", found[1], self.want)
        if not any(line.strip().split("#", 1)[0].strip() == "[package]" for line in lines):
            self.save(path, text, "".join(lines))
            return
        found = table_key(lines, "package", "version")
        if found and found[1] == "workspace":
            self.checked += 1
        elif found is None:
            self.see(path, "the package version", "unset (0.0.0)", self.want)
        elif self.write:
            set_value(lines, found[0], self.want)
        else:
            self.see(path, "the package version", found[1], self.want)
        self.save(path, text, "".join(lines))

    def cargo_lock(self, path):
        text = (ROOT / path).read_text(encoding="utf-8")
        blocks = re.split(r"(?m)^(?=\[\[package\]\]$)", text)
        for n, block in enumerate(blocks):
            if not block.startswith("[[package]]") or re.search(r"(?m)^source = ", block):
                continue
            name = re.search(r'(?m)^name = "([^"]*)"$', block).group(1)
            version = re.search(r'(?m)^version = "([^"]*)"$', block)
            if self.write:
                blocks[n] = block[: version.start(1)] + self.want + block[version.end(1):]
            else:
                self.see(path, f"the locked version of {name}", version.group(1), self.want)
        self.save(path, text, "".join(blocks))

    def npm(self, path):
        text = (ROOT / path).read_text(encoding="utf-8")
        data = json.loads(text)
        places = [("the version", data)]
        if path.endswith("package-lock.json"):
            places.append(("the root package's version", data.get("packages", {}).get("", {})))
        for what, obj in places:
            if self.write:
                obj["version"] = self.want
            else:
                self.see(path, what, obj.get("version", "unset"), self.want)
        if self.write:
            if json.dumps(json.loads(text), indent=2, ensure_ascii=False) + "\n" != text:
                sys.exit(f"check-versions: {path} is not in npm's layout; format it with npm first")
            self.save(path, text, json.dumps(data, indent=2, ensure_ascii=False) + "\n")

    def pyproject(self, path):
        text = (ROOT / path).read_text(encoding="utf-8")
        lines = text.splitlines(keepends=True)
        found = table_key(lines, "project", "version")
        if found is None:
            self.see(path, "the [project] version", "unset", self.want_py)
        elif self.write:
            set_value(lines, found[0], self.want_py)
        else:
            self.see(path, "the [project] version", found[1], self.want_py)
        self.save(path, text, "".join(lines))

    def head_hbs(self, path):
        text = (ROOT / path).read_text(encoding="utf-8")
        m = HBS_VERSION.search(text)
        if m is None:
            self.see(path, "the pm-version meta", "missing", self.want)
        elif self.write:
            self.save(path, text, HBS_VERSION.sub(rf"\g<1>{self.want}\g<3>", text, count=1))
        else:
            self.see(path, "the pm-version meta", m.group(2), self.want)

    def walk(self, files):
        kinds = {
            "Cargo.toml": self.cargo_toml,
            "Cargo.lock": self.cargo_lock,
            "package.json": self.npm,
            "package-lock.json": self.npm,
            "pyproject.toml": self.pyproject,
        }
        # The workspace version first: every other place is compared with it.
        for path in sorted(files, key=lambda f: f != "Cargo.toml"):
            kind = kinds.get(path.rsplit("/", 1)[-1])
            if kind:
                kind(path)
            elif path == HEAD_HBS:
                self.head_hbs(path)


def rust_versions(files):
    """Every rust-version among the Cargo manifests in `files` against the
    toolchain rust-toolchain.toml pins: the ones that disagree, and a
    summary of the check."""
    if TOOLCHAIN not in files:
        return [f"{TOOLCHAIN} is missing, so no toolchain pins the rust-versions"], ""
    found = table_key((ROOT / TOOLCHAIN).read_text(encoding="utf-8").splitlines(), "toolchain", "channel")
    channel = found[1] if found else "unset"
    if not re.match(r"\d+\.\d+\.\d+\Z", channel):
        return [f"{TOOLCHAIN}: the channel is {channel}, not a version a rust-version can agree with"], ""
    differ, stated, inherited = [], 0, 0
    for path in files:
        if path.rsplit("/", 1)[-1] != "Cargo.toml":
            continue
        lines = (ROOT / path).read_text(encoding="utf-8").splitlines()
        for table in ("workspace.package", "package"):
            found = table_key(lines, table, "rust-version")
            if found is None:
                continue
            if found[1] == "workspace":
                inherited += 1
                continue
            stated += 1
            value = found[1]
            if not RUST_VERSION.match(value) or channel.split(".")[: value.count(".") + 1] != value.split("."):
                differ.append(f"{path}: the rust-version is {value}, not the toolchain's {channel}")
    return differ, f"{stated + inherited} rust-versions ({inherited} inherited) agree with the toolchain {channel}"


def main(argv):
    if argv and (argv[0] != "--set" or len(argv) != 2):
        sys.exit("usage: scripts/check-versions.py [--set VERSION]")
    files, how = list_tree()
    if argv:
        try:
            tree = Tree(argv[1], write=True)
        except ValueError as e:
            sys.exit(f"check-versions: {e}")
        tree.walk(files)
        print(f"check-versions: set {argv[1]} in {len(tree.changed)} file(s)")
        for path in tree.changed:
            print(f"  {path}")
    want = workspace_version()
    try:
        tree = Tree(want, write=False)
    except ValueError as e:
        sys.exit(f"check-versions: the workspace version: {e}")
    tree.walk(files)
    rust_differ, rust = rust_versions(files)
    if tree.differ:
        print(f"check-versions: {len(tree.differ)} version string(s) differ from the workspace version {want}:")
        for line in tree.differ:
            print(f"  {line}")
    if rust_differ:
        print(f"check-versions: {len(rust_differ)} rust-version(s) disagree with {TOOLCHAIN}:")
        for line in rust_differ:
            print(f"  {line}")
    if tree.differ or rust_differ:
        return 1
    print(f"check-versions: ok ({tree.checked} version strings, all {want}; {rust}; in {len(files)} files {how})")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
