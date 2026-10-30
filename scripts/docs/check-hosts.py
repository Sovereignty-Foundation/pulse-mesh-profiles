#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""Refuse any tracked file that names a development host, links into a forge,
or cites an internal pull request or issue.

The tree links to the documentation site only. A development server's name, a
raw public IP address, a forge's source or edit path (`/src/branch/`,
`/_edit/`, `/blob/`, `/tree/`), or an `owner/repo#N` reference in any tracked
text file fails the check, so none of them can travel into a cut repository.
Loopback, private and documentation-range addresses, which tests and examples
use, are allowed. `book/guide-source/` is the notes the guides are written
from; the cut drops it, so it is not checked. `THIRD_PARTY_NOTICES.md`
quotes the dependencies' license texts word for word, forge links and all,
so it is checked for development hosts alone.
"""
import ipaddress, re, subprocess, sys

HOSTS = re.compile(r'pulsemesh\.dev|gitea-sovfound|\.ts\.net\b', re.I)
FORGE = re.compile(r'https?://[^\s)>"\'`]+/(?:src/branch|_edit|blob|tree)/')
REF = re.compile(r'(?<![\w/.-])[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+#\d+\b')
IPV4 = re.compile(r'(?<![\w.%])(\d{1,3}(?:\.\d{1,3}){3})(?![\d.])')
SKIP = ('Cargo.lock', 'package-lock.json', 'scripts/docs/check-hosts.py')
SKIP_DIRS = ('book/guide-source/',)
# Checked for development hosts alone: quoted third-party text.
HOSTS_ONLY = ('THIRD_PARTY_NOTICES.md',)

def public_ip(text):
    try:
        ip = ipaddress.ip_address(text)
    except ValueError:
        return False
    return ip.is_global

bad = []
for f in subprocess.check_output(['git', 'ls-files'], text=True).split('\n'):
    if not f or f.endswith(SKIP) or f.startswith(SKIP_DIRS):
        continue
    try:
        lines = open(f, encoding='utf-8').read().split('\n')
    except (UnicodeDecodeError, IsADirectoryError, FileNotFoundError):
        continue
    hosts_only = f.endswith(HOSTS_ONLY)
    for n, line in enumerate(lines, 1):
        if hosts_only:
            hit = HOSTS.search(line)
            hit = hit.group(0) if hit else None
        else:
            hit = HOSTS.search(line) or FORGE.search(line) or REF.search(line)
            hit = hit.group(0) if hit else next((m.group(1) for m in IPV4.finditer(line) if public_ip(m.group(1))), None)
        if hit:
            bad.append(f'{f}:{n}: {hit}')
if bad:
    print('host check: these lines name a development host, a public address, a forge path or an internal reference:')
    print('\n'.join(bad))
    sys.exit(1)
print('host check: ok')
