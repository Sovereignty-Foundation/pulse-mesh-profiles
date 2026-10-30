#!/bin/sh
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Give the whole tree one version: scripts/set-version.sh <version>
#
# Writes <version> as the workspace version ([workspace.package] in
# Cargo.toml) and into every other place scripts/check-versions.py checks:
# each Cargo manifest that states its own version (the bindings and the
# fuzz harness), each Cargo.lock entry for a package built from this tree,
# each package.json and package-lock.json, each pyproject.toml (in its
# PEP 440 form) and the version the book displays. The places are listed
# once, in check-versions.py, which writes them and then checks the tree.
# LICENSE also names a release version, and this script leaves it
# unchanged, so a new version fails the license gate until a person
# updates LICENSE.
set -eu
if [ $# -ne 1 ]; then
    echo "usage: scripts/set-version.sh <version>" >&2
    exit 2
fi
cd "$(dirname "$0")/.."
exec python3 scripts/check-versions.py --set "$1"
