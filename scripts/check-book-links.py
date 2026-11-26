#!/usr/bin/env python3
"""Check the internal links of the book (book/CLAUDE.md).

Run it after `mdbook build book`:

    python3 scripts/check-book-links.py

Every relative Markdown link in book/src must name an existing file, and its `#anchor` must be
an id of the built HTML page (mdbook's own heading ids). It also reports references to the old
`docs/*.md` files, which moved into the book. Exit code 1 on any problem.
"""
import os
import re
import sys

BOOK = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "book")
SRC = os.path.abspath(os.path.join(BOOK, "src"))
HTML = os.path.abspath(os.path.join(BOOK, "book"))
OLD = re.compile(r"docs/[a-z-]+\.md|\b(server-dir|admin-api|http-clients|release-notes-[0-9.]+)\.md\b")


def pages():
    for folder, _, names in os.walk(SRC):
        for name in names:
            if name.endswith(".md"):
                yield os.path.join(folder, name)


def ids_of(page):
    html = os.path.join(HTML, os.path.relpath(page, SRC))[:-3] + ".html"
    if not os.path.exists(html):
        return None
    with open(html, encoding="utf-8") as f:
        return set(re.findall(r'id="([^"]+)"', f.read()))


def main():
    if not os.path.isdir(HTML):
        print("build the book first: mdbook build book")
        return 1
    ids = {os.path.abspath(p): ids_of(p) for p in pages()}
    problems = 0
    links = 0
    for page in sorted(ids):
        rel = os.path.relpath(page, SRC)
        with open(page, encoding="utf-8") as f:
            text = f.read()
        text = re.sub(r"```.*?```", "", text, flags=re.S)
        prose = re.sub(r"`[^`\n]*`", "", text)
        for match in re.finditer(r"\]\(([^)\s]+)\)", prose):
            url = match.group(1)
            if re.match(r"^[a-z]+:", url) or url.startswith("<"):
                continue
            links += 1
            path, _, anchor = url.partition("#")
            target = os.path.abspath(os.path.join(os.path.dirname(page), path)) if path else page
            if not os.path.exists(target):
                print(f"{rel}: missing file {url}")
                problems += 1
            elif anchor and target.endswith(".md"):
                known = ids.get(target)
                if known is None:
                    print(f"{rel}: {url}: the page is not in SUMMARY.md (no HTML)")
                    problems += 1
                elif anchor not in known:
                    print(f"{rel}: missing anchor {url}")
                    problems += 1
        for match in OLD.finditer(text):
            line = text[: match.start()].count("\n") + 1
            print(f"{rel}:{line}: reference to an old docs file: {match.group(0)}")
            problems += 1
    print(f"{links} relative links checked, {problems} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
