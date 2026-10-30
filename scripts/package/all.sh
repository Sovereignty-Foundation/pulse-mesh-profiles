#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# Build every binding's artifacts into target/package/, then smoke-test
# them. It publishes nothing and uploads nothing: the artifacts stay in
# target/package/.
#
#   scripts/package/all.sh
#
# Runs, in order, stopping at the first that fails:
#   wheels.sh   the Python wheels: macOS (on a Mac) and manylinux_2_28
#               aarch64 and x86_64
#   node.sh     the Node package, pulse-wire-<version>.tgz, with the
#               darwin arm64 and x64 (on a Mac) and linux arm64 and x64
#               modules
#   wasm.sh     the WebAssembly package, pulse-wire-wasm-<version>.tgz
#   smoke.sh    each artifact installed into a clean environment, on the
#               host and, for the Linux ones, in Docker
# then lists the artifacts with their sizes. Each script's header names
# its targets, its prerequisites and its settings; all of them apply
# here. Together they need cargo, python3 3.11 or later, node 18 or later
# and npm, the wasm32-unknown-unknown target and the pinned
# wasm-bindgen-cli, and Docker and rsync for the Linux legs (on an arm64
# host the x86_64 builds run under emulation, and take most of the time).
. "$(dirname "$0")/lib.sh"

scripts/package/wheels.sh
scripts/package/node.sh
scripts/package/wasm.sh
scripts/package/smoke.sh
echo "package: the artifacts in ${OUT#"$ROOT"/}:"
list_artifacts "$OUT"/*.whl "$OUT"/*.tgz
