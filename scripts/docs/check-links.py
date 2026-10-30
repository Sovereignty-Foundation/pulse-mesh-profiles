#!/usr/bin/env python3
# SPDX-License-Identifier: LicenseRef-Proprietary
# Copyright (c) 2026 Sovereignty Foundation
"""Relative links, images and `{{#include}}` files inside book/src resolve to files that exist.

A root-relative link points into another book on the site, as the cut (scripts/cut.py) writes them for a page that
went to another repository; the three books live under the website's docs section (`/docs/`, `/docs/daemon/`,
`/docs/profiles/`), so such a link starts with `/docs/`, and that book's own check covers the rest.

With --html it checks the rendered book under book/book instead, after `mdbook build book`: every relative href and src
on every page resolves to a file of the build. The source check cannot see what mdBook renames, and it does not read
the HTML that pages write by hand (the openers' buttons).
"""
import re, sys, pathlib
from html.parser import HTMLParser
from urllib.parse import unquote, urlsplit
src = pathlib.Path(__file__).resolve().parents[2] / "book/src"


class Targets(HTMLParser):
    def __init__(self):
        super().__init__()
        self.found = []

    def handle_starttag(self, tag, attrs):
        for name, value in attrs:
            if name in ("href", "src") and value:
                self.found.append(value)


def check_html():
    out = src.parent / "book"
    if not out.is_dir():
        sys.exit("check-links --html: no book/book; run `mdbook build book` first")
    bad = []
    pages = [p for p in sorted(out.rglob("*.html")) if p.name not in ("print.html", "toc.html", "404.html")]
    for page in pages:
        parser = Targets()
        parser.feed(page.read_text(errors="replace"))
        for target in parser.found:
            parts = urlsplit(target)
            if parts.scheme or target.startswith(("#", "/", "data:")):
                continue
            path = unquote(parts.path)
            if not path:
                continue
            resolved = (page.parent / path).resolve()
            if resolved.is_dir():
                resolved = resolved / "index.html"
            if out.resolve() not in resolved.parents or not resolved.exists():
                bad.append(f"{page.relative_to(out)}: {target}")
    if bad:
        print(f"check-links --html: {len(bad)} links in the rendered book lead nowhere:"); print("  " + "\n  ".join(sorted(set(bad)))); sys.exit(1)
    print(f"check-links --html: ok ({len(pages)} pages)")


if "--html" in sys.argv[1:]:
    check_html()
    sys.exit(0)
link = re.compile(r'!?\[[^\]]*\]\(([^)\s]+)(?:\s+"[^"]*")?\)')
# an include names a file, then optionally an anchor or a line range after a colon; mdbook only warns when the file
# is missing, and the page then renders without its listing
include = re.compile(r'\{\{#(?:include|rustdoc_include)\s+([^:}\s]+)')
bad = []
for page in sorted(src.rglob("*.md")):
    text = page.read_text()
    for target in include.findall(text):
        if not (page.parent / target).resolve().exists():
            bad.append(f"{page.relative_to(src)}: include {target}")
    # skip fenced code blocks
    text = re.sub(r"```.*?```", "", text, flags=re.S)
    for target in link.findall(text):
        if target.startswith(("http://", "https://", "mailto:", "#")):
            continue
        if target.startswith("/"):
            if not target.startswith("/docs/"):
                bad.append(f"{page.relative_to(src)}: {target} (a root-relative link points into one of the three books under /docs/)")
            continue
        path = target.split("#", 1)[0]
        if not path:
            continue
        resolved = (page.parent / path).resolve()
        if not resolved.exists():
            bad.append(f"{page.relative_to(src)}: {target}")
if bad:
    print("check-links: broken relative links:"); print("  " + "\n  ".join(bad)); sys.exit(1)
print("check-links: ok")
