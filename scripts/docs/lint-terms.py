#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""Released text, source comments and manifests carry no internal process language and no names that no longer exist.

The Sovereignty stack's codenames (sovOS, Kronos, Canon, R-LEX and the rest) are
public and welcome; this lint is about how the tree was made, not what it is.
Scope grows as the language pass reaches each surface (see the paths below).
"""
import re, sys, pathlib
root = pathlib.Path(__file__).resolve().parents[2]
# the integrated tree's top levels and those of the repositories cut from it; a tree has some of them
scope = [root / "book/src", root / "README.md", root / "CONTRIBUTING.md", root / "STATUS.md", root / "pulse", root / "core", root / "mesh", root / "rings", root / "dev_ring", root / "peripheral", root / "scripts", root / "tools", root / "examples", root / "bindings", root / "clients", root / "plugins", root / "fuzz", root / "Cargo.toml", root / "modules.toml"]
scope = [p for p in scope if p.exists()]
# decisions/ holds the decision record and is read as data, not linted; build output and the rendered book are skipped.
SUFFIXES = (".md", ".rs", ".toml", ".py", ".js", ".mjs", ".sh", ".hbs", ".yml")
SKIP_DIRS = {"target", "node_modules", "pkg", "book"}  # build output, dependencies, the rendered book
banned = [
    (r"\bPhase (I|H|C|G|1|2a|2b|2c)\b", "internal phase label"),
    (r"deferred to Module 1", "internal process note"),
    (r"upstream mesh core|upstream-compat-tests|sovos-compat", "stale name"),
    (r"docs/(USER_GUIDE|DEVELOPER_ONBOARDING|RELEASE_READINESS)\.md", "retired document"),
    (r"\bSPEC\.md\b", "document that does not exist; the specification is Part 3 of the book"),
]
hits = []
for base in scope:
    files = [base] if base.is_file() else sorted(f for f in base.rglob("*") if f.suffix in SUFFIXES and not (SKIP_DIRS & set(f.relative_to(base).parts[:-1])) and f.resolve() != pathlib.Path(__file__).resolve())
    for f in files:
        for n, line in enumerate(f.read_text().splitlines(), 1):
            for pat, why in banned:
                if re.search(pat, line):
                    hits.append(f"{f.relative_to(root)}:{n}: {why}: {line.strip()[:100]}")
if hits:
    print("lint-terms: found internal language in released text:"); print("  " + "\n  ".join(hits)); sys.exit(1)
print("lint-terms: ok")
