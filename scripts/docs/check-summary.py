#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""Every page under book/src is reachable from SUMMARY.md, and every SUMMARY entry exists."""
import re, sys, pathlib
src = pathlib.Path(__file__).resolve().parents[2] / "book/src"
summary = (src / "SUMMARY.md").read_text()
linked = {src / p for p in re.findall(r'\]\(([^)#]+\.md)\)', summary)}
missing = sorted(str(p.relative_to(src)) for p in linked if not p.exists())
pages = {p for p in src.rglob("*.md") if p.name != "SUMMARY.md" and "_" not in p.relative_to(src).parts[0]}
orphans = sorted(str(p.relative_to(src)) for p in pages - linked)
bad = False
if missing:
    bad = True; print("check-summary: SUMMARY.md links to pages that do not exist:"); print("  " + "\n  ".join(missing))
if orphans:
    bad = True; print("check-summary: pages not listed in SUMMARY.md:"); print("  " + "\n  ".join(orphans))
if bad:
    sys.exit(1)
print(f"check-summary: ok ({len(linked)} pages)")
