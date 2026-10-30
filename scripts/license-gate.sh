#!/bin/sh
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
#
# License gate: the repository's license files are the pinned texts, before
# anything is pushed. Run by `.githooks/pre-push` (enable it with
# `just install-hooks`), by `just license-gate` and by `just check`. A
# pre-release cut carries the proprietary notice; a release cut carries the
# full license as LICENSE, its summary as NOTICE and THIRD_PARTY_NOTICES.md.
# The texts are approved by the Architect and pinned here by the cut. This
# checks the files are in place; it is not an audit of dependency licenses.
#
# Checks:
#   1. LICENSE is exactly the pinned license text (sha256).
#   2. STATUS.md is exactly the pinned status notice (sha256).
#   3. In a release cut, NOTICE is exactly the pinned notice and
#      THIRD_PARTY_NOTICES.md is present; in a pre-release cut there is no
#      NOTICE. There is no LICENSING.md.
#   4. Every Cargo manifest that names a license names a license file, and
#      every Cargo manifest that states publish states publish = false.
#   5. Every package.json is "UNLICENSED" and private.
#   6. Every tracked source file carries the SPDX identifier
#      LicenseRef-Proprietary in its first lines (generated files are
#      listed as exempt).
set -eu
cd "$(git rev-parse --show-toplevel)"
exec python3 - "$@" <<'PY'
import hashlib, json, re, subprocess, sys
LICENSE_SHA = "2d9d938198336c557dd0c6a913b01aab59add89ff04b4f88049ae4eed2929728"
STATUS_SHA = "306f7f6bef37df55f944d4dc62187609f18a9643544a7b125074e2b620233e50"
NOTICE_SHA = "f735f7726ab27bbb82afcc904a92608796224b5170de650bd56af4d8e9ec9ac7"  # empty in a pre-release cut
# the napi-generated Node binding files, where a cut repository and where the whole tree keep them
EXEMPT = {"bindings/node/index.js", "bindings/node/index.d.ts", "peripheral/bindings/node/index.js", "peripheral/bindings/node/index.d.ts"}
failures = []
def fail(msg): failures.append(msg)
def sha(b): return hashlib.sha256(b).hexdigest()
if sha(open("LICENSE", "rb").read()) != LICENSE_SHA: fail("LICENSE is not the pinned license text")
if sha(open("STATUS.md", "rb").read()) != STATUS_SHA: fail("STATUS.md is not the status notice")
tracked = [t for t in subprocess.check_output(["git", "ls-files", "-z"]).decode().split("\0") if t]
if "LICENSING.md" in tracked: fail("LICENSING.md is present; the license terms are LICENSE and NOTICE")
if NOTICE_SHA:
    if "NOTICE" not in tracked: fail("NOTICE is missing")
    elif sha(open("NOTICE", "rb").read()) != NOTICE_SHA: fail("NOTICE is not the pinned notice")
    if "THIRD_PARTY_NOTICES.md" not in tracked: fail("THIRD_PARTY_NOTICES.md is missing")
elif "NOTICE" in tracked: fail("NOTICE is present in a pre-release cut")
for f in tracked:
    name = f.rsplit("/", 1)[-1]
    if name == "Cargo.toml":
        text = open(f, encoding="utf-8").read()
        for m in re.finditer(r'^license\s*=\s*"([^"]*)"', text, re.M): fail(f"{f}: license = {m.group(1)!r}; a launch repository names its license file")
        for m in re.finditer(r'^publish\s*=\s*(\S+)', text, re.M):
            if m.group(1) != "false": fail(f"{f}: publish = {m.group(1)}")
    elif name == "package.json":
        d = json.load(open(f, encoding="utf-8"))
        if d.get("license") != "UNLICENSED": fail(f"{f}: license {d.get('license')!r}")
        if d.get("private") is not True: fail(f"{f}: not private")
sources = [f for f in tracked if f.rsplit(".", 1)[-1] in ("rs", "py", "js", "mjs", "ts", "sh") and f not in EXEMPT]
for f in sources:
    head = open(f, encoding="utf-8", errors="replace").read(400)
    ids = re.findall(r"SPDX-License-Identifier:\s*(\S+)", head)
    if not ids: fail(f"{f}: no SPDX identifier in its first lines")
    elif any(i != "LicenseRef-Proprietary" for i in ids): fail(f"{f}: SPDX identifier {ids}")
if failures:
    print("license gate FAILED:", file=sys.stderr)
    for m in failures: print("  " + m, file=sys.stderr)
    sys.exit(1)
print(f"license gate ok: LICENSE, STATUS.md{', NOTICE and THIRD_PARTY_NOTICES.md' if NOTICE_SHA else ''} are in place, {len(sources)} source files carry LicenseRef-Proprietary")
PY
