# Only a browser proves the worker answers a navigation from its cache; the
# static half lives in test_publishing_manifests.py. Playwright is not a
# project dependency (ci.yml's docs-pwa job pins it), so this skips without it.

from __future__ import annotations

import functools
import http.server
import json
import re
import socketserver
import threading
from pathlib import Path
from typing import ClassVar

import pytest

playwright = pytest.importorskip("playwright")
from playwright.sync_api import Error as PlaywrightError  # noqa: E402
from playwright.sync_api import sync_playwright  # noqa: E402

DOCS = Path(__file__).parent.parent / "docs"
PREFIX = ""


class PagesStandIn(http.server.SimpleHTTPRequestHandler):
    # Pages renders product/*.md to *.html; the stand-in keeps the first heading,
    # which is what the offline assertion reads.
    extensions_map: ClassVar[dict[str, str]] = {
        **http.server.SimpleHTTPRequestHandler.extensions_map,
        ".webmanifest": "application/manifest+json",
    }

    def translate_path(self, path: str) -> str:
        path = path.split("?", 1)[0]
        assert path.startswith(PREFIX + "/"), path
        root = DOCS.resolve()
        target = (root / path[len(PREFIX) + 1 :]).resolve()
        # A request path is untrusted even on loopback: `..` must not leave docs/.
        return str(target) if target.is_relative_to(root) else str(root / "__outside_docs__")

    def do_GET(self) -> None:
        target = Path(self.translate_path(self.path))
        source = target.with_suffix(".md")
        if target.suffix == ".html" and not target.exists() and source.is_file():
            markdown = source.read_text(encoding="utf-8")
            heading = re.search(r"^# (.+)$", markdown, re.MULTILINE)
            assert heading, source
            body = f"<!doctype html><html><head><title>{heading.group(1)}</title></head><body><h1>{heading.group(1)}</h1><pre>{markdown}</pre></body></html>"
            payload = body.encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        super().do_GET()

    def end_headers(self) -> None:
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def log_message(self, *_args: object) -> None:
        return


@pytest.fixture
def site():
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), functools.partial(PagesStandIn, directory=str(DOCS)))
    server.daemon_threads = True
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    origin = f"http://127.0.0.1:{server.server_address[1]}"
    try:
        yield origin, server
    finally:
        server.shutdown()
        server.server_close()


@pytest.fixture
def browser():
    with sync_playwright() as pw:
        try:
            instance = pw.chromium.launch()
        except PlaywrightError as exc:
            pytest.skip(f"chromium is not installed: {exc}")
        yield instance
        instance.close()


def _first_heading(markdown_name: str) -> str:
    heading = re.search(r"^# (.+)$", (DOCS / markdown_name).read_text(encoding="utf-8"), re.MULTILINE)
    assert heading
    return heading.group(1)


@pytest.mark.timeout(120)
def test_installed_shortcuts_open_offline(site, browser):
    origin, server = site
    manifest = json.loads((DOCS / "manifest.webmanifest").read_text(encoding="utf-8"))
    context = browser.new_context()
    page = context.new_page()

    page.goto(f"{origin}{PREFIX}/")
    assert page.evaluate("navigator.serviceWorker.ready.then(r => r.active !== null)")
    for icon in manifest["icons"]:
        response = page.request.get(f"{origin}{icon['src']}")
        assert response.status == 200, icon
        assert response.headers["content-type"] == "image/png", icon
    assert page.request.get(f"{origin}{PREFIX}/manifest.webmanifest").status == 200
    for shortcut in manifest["shortcuts"]:
        cached = page.evaluate("url => caches.match(url).then(r => r !== undefined)", f"{origin}{shortcut['url']}")
        assert cached, f"{shortcut['url']} is not precached"

    server.shutdown()
    context.set_offline(True)

    for shortcut in manifest["shortcuts"]:
        page.goto(f"{origin}{shortcut['url']}")
        markdown_name = shortcut["url"].removeprefix(PREFIX + "/").removesuffix(".html") + ".md"
        assert page.locator("h1").first.inner_text() == _first_heading(markdown_name), shortcut

    page.goto(f"{origin}{PREFIX}/product/faq.html")
    note = page.get_by_role("status").inner_text()
    assert "You are offline" in note
    assert f"{PREFIX}/product/faq.html" in note
    context.close()
