#!/usr/bin/env bash
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
# The documentation gate: build the book and run the structural checks.
# Mirrors the ci/docs job; `just docs` runs it locally.
set -euo pipefail
cd "$(dirname "$0")/../.."
python3 scripts/docs/check-version.py
python3 scripts/docs/check-summary.py
python3 scripts/docs/check-links.py
python3 scripts/docs/lint-terms.py
python3 scripts/docs/check-generated.py
python3 scripts/docs/check-hosts.py
mdbook build book
python3 scripts/docs/check-links.py --html
# The smoke run's verdict is its last line, not its exit status alone: macOS's
# bash 3.2 exits 0 from a script whose EXIT trap runs after an unbound-variable
# error, and the run has such a trap for its cleanup.
smoke=$(mktemp "${TMPDIR:-/tmp}/pms-docs-smoke.XXXXXXXX")
scripts/docs/smoke.sh 2>&1 | tee "$smoke"
grep -q '^docs smoke: ok' "$smoke" || { rm -f "$smoke"; echo "docs gate: the smoke run did not finish"; exit 1; }
rm -f "$smoke"
echo "docs gate: ok"
