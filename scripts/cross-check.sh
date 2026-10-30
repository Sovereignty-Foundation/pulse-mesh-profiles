#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Lint the workspace for x86_64 Linux from any host, an arm64 Mac
# included: clippy -D warnings with every feature on, on every target,
# with --target x86_64-unknown-linux-gnu. The x86-only code (the umwait
# and mwaitx wait strategies, the x86 paths in core-ring and the hft ring)
# compiles and is linted; nothing runs.
#
#   scripts/cross-check.sh
#
# A crate is left out when its dependency graph links a native library
# (a package with a `links` key; today only libsqlite3-sys, which builds
# rusqlite's bundled SQLite and so needs a C cross compiler and an x86_64
# Linux sysroot). The script names every crate it leaves out. The graph's
# other C, in blake3, falls back to blake3's Rust code when no x86 C
# compiler is found. Needs the target's standard library and python3,
# which reads `cargo metadata`.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET=x86_64-unknown-linux-gnu

missing() { echo "cross-check: $1" >&2; exit 127; }
[ -d "$(rustc --print sysroot)/lib/rustlib/$TARGET" ] ||
  missing "the $TARGET target is not installed: rustup target add $TARGET"
command -v python3 >/dev/null 2>&1 || missing "python3 is not installed: install Python 3"

# One line per workspace member: its name, then the packages with a
# `links` key in its dependency graph for the target, if any.
plan=$(cargo metadata --format-version 1 --all-features --filter-platform "$TARGET" | python3 -c '
import json, sys
meta = json.load(sys.stdin)
packages = {p["id"]: p for p in meta["packages"]}
deps = {n["id"]: [d["pkg"] for d in n["deps"]] for n in meta["resolve"]["nodes"]}
for member in meta["workspace_members"]:
    seen, todo = set(), [member]
    while todo:
        for dep in deps[todo.pop()]:
            if dep not in seen:
                seen.add(dep)
                todo.append(dep)
    native = sorted({packages[d]["name"] for d in seen if packages[d].get("links")})
    print(packages[member]["name"], ",".join(native))
')
args=()
n=0
left_out=""
while read -r name native; do
  [ -n "$name" ] || continue
  if [ -z "$native" ]; then
    args+=(-p "$name")
    n=$((n + 1))
  else
    left_out="$left_out $name ($native)"
  fi
done <<EOF
$plan
EOF
[ "$n" -gt 0 ] || { echo "cross-check: every crate links a native library; nothing to check" >&2; exit 1; }
echo "cross-check: clippy for $TARGET on $n crates; left out, as they link a native library:${left_out:- none}" >&2
exec cargo clippy "${args[@]}" --all-targets --all-features --target "$TARGET" -- -D warnings
