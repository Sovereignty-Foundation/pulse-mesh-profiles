#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""The version the book displays must be the workspace version."""
import re, sys, pathlib
root = pathlib.Path(__file__).resolve().parents[2]
cargo = (root / "Cargo.toml").read_text()
m = re.search(r'^\[workspace\.package\][^\[]*?^version\s*=\s*"([^"]+)"', cargo, re.M | re.S)
if not m:
    sys.exit("check-version: no [workspace.package] version in Cargo.toml")
want = m.group(1)
head = (root / "book/theme/head.hbs").read_text()
h = re.search(r'<meta name="pm-version" content="([^"]+)">', head)
if not h:
    sys.exit("check-version: book/theme/head.hbs has no pm-version meta")
if h.group(1) != want:
    sys.exit(f"check-version: book shows {h.group(1)} but the workspace is {want}")
print(f"check-version: ok ({want})")
