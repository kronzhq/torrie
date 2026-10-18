# /// script
# requires-python = ">=3.12"
# dependencies = ["markdown==3.7"]
# ///
"""Renders site/docs/*.md into docs.html, between its <!-- docs --> markers. Run after editing the docs:

    uv run site/build.py
"""
import re
from pathlib import Path
from urllib.parse import urljoin

import markdown

SITE = Path(__file__).parent
PAGES = ["README.md", "remote-control.md", "plugins.md"]
REPO = "https://github.com/kronzhq/torrie/blob/master/site/docs/"

nav, sections, anchors = [], [], {}
for page in PAGES:
    md = markdown.Markdown(extensions=["tables", "fenced_code", "toc"])
    html = md.convert((SITE / "docs" / page).read_text())
    for h1 in md.toc_tokens:
        anchors.setdefault(page, h1["id"])
        nav.append(f'<a class="group" href="#{h1["id"]}">{h1["name"]}</a>')
        nav += [f'<a href="#{h2["id"]}">{h2["name"]}</a>' for h2 in h1["children"]]
    sections.append(f'<section>\n{html}\n</section>')


# Links between the files become anchors on this page; other relative links point into the repo.
def link(m):
    href = m[1]
    if re.match(r"[a-z]+:|#", href):
        return m[0]
    file, _, hash = href.partition("#")
    return f'href="#{hash or anchors[file]}"' if file in anchors else f'href="{urljoin(REPO, href)}"'


body = re.sub(r'href="([^"]*)"', link, "\n".join(sections))
docs = SITE / "docs.html"
out = re.sub(
    r"(<!-- docs -->\n).*(<!-- /docs -->)",
    lambda m: f'{m[1]}  <nav>\n    {"\n    ".join(nav)}\n  </nav>\n  <main>\n{body}\n  </main>\n  {m[2]}',
    docs.read_text(),
    flags=re.S,
)
docs.write_text(out)
