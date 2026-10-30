#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""The committed generated pages equal what the generators produce now."""
import subprocess, sys, tempfile, pathlib, filecmp
root = pathlib.Path(__file__).resolve().parents[2]
pages = ['reference/cli-reference.md', 'reference/daemon-reference.md', 'reference/mcp-tools.md', 'reference/modules.md', 'reference/crates.md', 'reference/errors.md', 'explanation/decision-log.md']
# the pages a binary prints exist where the workspace has the binary (gen-all.sh)
pages = [p for p in pages if (root / 'book/src' / p).exists()]
with tempfile.TemporaryDirectory() as tmp:
    subprocess.run(['scripts/docs/gen-all.sh', tmp], cwd=root, check=True, stdout=subprocess.DEVNULL)
    stale = [p for p in pages if not filecmp.cmp(root / 'book/src' / p, pathlib.Path(tmp) / p, shallow=False)]
if stale:
    print('check-generated: pages differ from their generators (run scripts/docs/gen-all.sh):'); print('  ' + '\n  '.join(stale)); sys.exit(1)
print(f'check-generated: ok ({len(pages)} pages)')
