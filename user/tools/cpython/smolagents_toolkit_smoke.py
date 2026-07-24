#!/usr/bin/env python3
"""Offline smoke test for SmolAgents' three built-in CLI tools."""

import sys
from pathlib import Path


args = sys.argv[1:]
if any(arg != "--exact" for arg in args):
    raise SystemExit("usage: smolagents_toolkit_smoke.py [--exact]")
exact = "--exact" in args
if exact:
    # -S is required on the board so /persist/python/user cannot shadow the
    # immutable release while its locked package closure is being certified.
    site_packages = Path(__file__).resolve().parent / "usr/lib/python3.14/site-packages"
    sys.path.insert(0, str(site_packages))

from importlib import metadata

import bs4
import click
import ddgs
import lxml.etree
import markdownify
import primp
import six
import soupsieve


EXPECTED = {
    "beautifulsoup4": "4.12.3",
    "click": "8.1.8",
    "ddgs": "9.0.0",
    "lxml": "6.1.1",
    "markdownify": "0.14.1",
    "primp": "0.15.0",
    "six": "1.17.0",
    "soupsieve": "2.6",
}


actual_versions = {}
for distribution, version in EXPECTED.items():
    actual = metadata.version(distribution)
    actual_versions[distribution] = actual
    if exact:
        assert actual == version, (distribution, actual, version)

# The effective board environment deliberately keeps pure Python packages in
# /persist/python/user.  Compatible updates there may shadow the release, but
# native packages must stay locked to the audited strict-aligned artifacts.
if not exact:
    assert actual_versions["lxml"] == EXPECTED["lxml"]
    assert actual_versions["primp"] == EXPECTED["primp"]
    compatible_majors = {
        "beautifulsoup4": {4},
        "click": {8},
        "ddgs": {9},
        "markdownify": {0, 1},
        "six": {1},
        "soupsieve": {2},
    }
    for distribution, majors in compatible_majors.items():
        major = int(actual_versions[distribution].split(".", 1)[0])
        assert major in majors, (distribution, actual_versions[distribution], majors)

root = lxml.etree.fromstring(b"<root><answer>42</answer></root>")
assert root.findtext("answer") == "42"

html = (
    '<html><body><h1>Mango</h1><p>strict toolkit '
    '<a href="https://example.invalid/docs">docs</a></p></body></html>'
)
soup = bs4.BeautifulSoup(html, "html.parser")
assert soup.select_one("h1").text == "Mango"
rendered = markdownify.markdownify(html)
assert "Mango" in rendered and "strict toolkit" in rendered
assert "[docs](https://example.invalid/docs)" in rendered
assert "strict toolkit" in soupsieve.select_one("p", soup).text
assert six.ensure_text(b"aligned") == "aligned"
assert click.unstyle("aligned") == "aligned"

# Constructors are intentionally exercised without external traffic.  This
# catches missing primp/lxml dependencies before SmolAgents reaches the same
# path while preserving deterministic QEMU-user and board verification.
primp_client = primp.Client(timeout=1)
search_client = ddgs.DDGS(timeout=1)
assert primp_client is not None and search_client is not None

print(
    "smolagents-toolkit-offline-smoke-ok",
    "mode=" + ("exact" if exact else "effective"),
    " ".join(
        f"{name}={version}" for name, version in sorted(actual_versions.items())
    ),
)
