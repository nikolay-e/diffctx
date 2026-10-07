from __future__ import annotations

import json
import os
import re
import sys
from pathlib import Path

import pytest

from diffctx.version import __version__

PROJECT_ROOT = Path(__file__).parent.parent

UVX_ARGS = ["--from", "diffctx[mcp]", "diffctx-mcp"]


def _load(name: str) -> dict:
    return json.loads((PROJECT_ROOT / name).read_text(encoding="utf-8"))


class TestRegistryManifest:
    """server.json is the immutable source for the official MCP registry:
    a bad version or an over-long description fails the publish AFTER the
    PyPI release already shipped, burning the version number."""

    def test_version_matches_package(self):
        assert _load("server.json")["version"] == __version__

    def test_name_is_the_verified_namespace(self):
        assert _load("server.json")["name"] == "io.github.nikolay-e/diffctx"

    def test_description_within_registry_schema_cap(self):
        assert len(_load("server.json")["description"]) <= 100

    def test_package_invocation_is_the_documented_uvx_command(self):
        pkg = _load("server.json")["packages"][0]
        assert pkg["identifier"] == "diffctx"
        assert pkg["transport"]["type"] == "stdio"
        args = []
        for arg in pkg["runtimeArguments"]:
            if arg["type"] == "named":
                args += [arg["name"], arg["value"]]
            else:
                args.append(arg["value"])
        assert args == UVX_ARGS

    def test_pypi_readme_carries_the_ownership_marker(self):
        """The registry proves PyPI package ownership by finding this exact
        literal (one space after the colon, case-sensitive) in the published
        description. Dropping it from README.md breaks every future publish."""
        readme = (PROJECT_ROOT / "README.md").read_text(encoding="utf-8")
        assert "mcp-name: io.github.nikolay-e/diffctx" in readme


class TestClaudePlugin:
    def test_manifest_version_matches_package(self):
        assert _load("plugin/.claude-plugin/plugin.json")["version"] == __version__

    def test_marketplace_lists_the_plugin_folder(self):
        [entry] = _load(".claude-plugin/marketplace.json")["plugins"]
        assert entry["name"] == _load("plugin/.claude-plugin/plugin.json")["name"]
        assert (PROJECT_ROOT / entry["source"] / ".claude-plugin" / "plugin.json").is_file()

    def test_plugin_mcp_json_pins_the_released_package(self):
        """Anthropic's plugin directory blocks an unpinned uvx launcher, and an
        installed plugin must start the version it was reviewed at."""
        server = _load("plugin/.mcp.json")["mcpServers"]["diffctx"]
        assert server["command"] == "uvx"
        # `uvx <package>==<version> <subcommand>` and no option: the only shape
        # the directory reads as a locked launch. `-c constraints.txt` made it
        # report "Launcher lock invalid"; `--from` read as an unconfirmed pin.
        assert server["args"] == [f"diffctx[mcp]=={__version__}", "mcp"]

    def test_plugin_uv_lock_pins_the_launcher_tree_from_pypi(self):
        """The directory takes the uv.lock beside .mcp.json as the launcher's
        dependency tree: exact versions, a registry source and hashes for every
        package, the launched one at the version the launcher names."""
        tomllib = pytest.importorskip("tomllib" if sys.version_info >= (3, 11) else "tomli")

        plugin = PROJECT_ROOT / "plugin"
        project = tomllib.loads((plugin / "pyproject.toml").read_text(encoding="utf-8"))["project"]
        assert project["dependencies"] == [f"diffctx[mcp]=={__version__}"]

        lock_path = plugin / "uv.lock"
        # Past 256 KiB the directory does not read a file and holds the version.
        assert lock_path.stat().st_size < 256 * 1024
        packages = tomllib.loads(lock_path.read_text(encoding="utf-8"))["package"]
        [root] = [p for p in packages if p["source"] == {"virtual": "."}]
        assert root["name"] == project["name"]
        locked = [p for p in packages if p is not root]
        assert {"diffctx", "mcp", "pathspec", "pydantic", "anyio"} <= {p["name"] for p in locked}
        for package in locked:
            assert package["source"] == {"registry": "https://pypi.org/simple"}, package["name"]
            artifacts = package.get("wheels", []) + ([package["sdist"]] if "sdist" in package else [])
            assert artifacts, package["name"]
            assert all(a["hash"].startswith("sha256:") for a in artifacts), package["name"]
        [diffctx] = [p for p in locked if p["name"] == "diffctx"]
        assert diffctx["version"] == __version__

    def test_mcp_json_is_self_bootstrapping(self):
        server = _load(".mcp.json")["mcpServers"]["diffctx"]
        assert server["command"] == "uvx"
        assert server["args"] == UVX_ARGS

    @pytest.mark.parametrize("command", ["diffctx", "impact", "commit"])
    def test_skill_has_frontmatter_and_a_real_tool(self, command):
        """Plugin commands instruct the model to call an MCP tool by name;
        a tool rename that skips these files ships a plugin whose commands
        reference nothing."""
        text = (PROJECT_ROOT / "plugin" / "skills" / command / "SKILL.md").read_text(encoding="utf-8")
        front = re.match(r"\A---\n(.*?)\n---\n", text, re.DOTALL)
        assert front
        assert "description:" in front.group(1)

        # Tool-name-shaped references, not every backticked word: the commands
        # also name parameters (`diff_ref`, `fragment_ids`) and values
        # (`"locate"`). The pattern was `get_[a-z_]+` until #127 renamed the tool,
        # at which point it matched nothing and the assertion below was the only
        # thing that noticed.
        referenced = set(re.findall(r"`(diffctx_[a-z_]+|get_[a-z_]+)`", text))
        assert referenced

        pytest.importorskip("mcp")
        from diffctx.mcp import server as mcp_server

        # The default surface, deliberately: a plugin command that only works
        # once the operator sets DIFFCTX_MCP_LEGACY_TOOLS is a broken command.
        exported = {t.name for t in mcp_server.mcp._tool_manager.list_tools()}
        assert referenced <= exported, f"unknown tools referenced: {referenced - exported}"


class TestPluginEvalMock:
    def test_the_eval_tool_listing_is_the_server_s_own(self):
        """#320: the plugin evals serve `diffctx_context` from a mock, and a
        listing that drifted from the server measured a tool nobody ships
        (an old description, `mode` defaulting to locate, no `symbol`)."""
        pytest.importorskip("mcp")
        import asyncio

        from diffctx.mcp.server import mcp as server

        # Compared by name: other tests in the same process register the
        # opt-in legacy tools on this shared server.
        live = {
            t.name: {
                k: v
                for k, v in t.model_dump(by_alias=True, exclude_none=True, mode="json").items()
                if k in ("name", "description", "inputSchema")
            }
            for t in asyncio.run(server.list_tools())
        }
        mocked = _load("plugin/evals/mocks/diffctx/_tools.json")["tools"]
        assert [t["name"] for t in mocked] == ["diffctx_context"]
        assert all(t == live[t["name"]] for t in mocked)


class TestPluginHook:
    HOOKS = PROJECT_ROOT / "plugin" / "hooks"

    def test_hooks_json_wires_the_session_start_and_impact_around_git(self):
        hooks = _load("plugin/hooks/hooks.json")["hooks"]
        [start] = hooks["SessionStart"]
        [session] = start["hooks"]
        assert session["command"].endswith("/hooks/diffctx-session.sh")
        for event in ("PreToolUse", "PostToolUse"):
            [entry] = hooks[event]
            # A Grep-tool search is answered after it ran (#337); the gate stays on Bash.
            assert entry["matcher"] == ("Bash|Grep" if event == "PostToolUse" else "Bash")
            [hook] = entry["hooks"]
            assert hook["command"].endswith("/hooks/diffctx-impact.sh")
            assert hook["timeout"] > _diffctx_hook_deadline()
        assert sorted(p.name for p in self.HOOKS.glob("*.sh")) == ["diffctx-impact.sh", "diffctx-session.sh"]
        for script in self.HOOKS.glob("*.sh"):
            assert os.access(script, os.X_OK)
            assert script.read_text(encoding="utf-8").splitlines()[-1] == "exit 0"

    def test_the_hooks_run_only_the_pinned_package(self):
        """The directory runs code from the reviewed repository or a package
        pinned to an exact version, written plainly in the command (1.18.1 was
        refused for downloading the release binary). Both scripts launch the
        MCP server's own pinned spec, so the environment its start cached is
        the one the git hook finds offline."""
        pin = f'"diffctx[mcp]=={__version__}"'
        for name in ("diffctx-impact.sh", "diffctx-session.sh"):
            text = (self.HOOKS / name).read_text(encoding="utf-8")
            for fetcher in ("curl", "wget", "releases/download", "checksums", "constraints"):
                assert fetcher not in text, (name, fetcher)
        impact = (self.HOOKS / "diffctx-impact.sh").read_text(encoding="utf-8")
        assert f"uvx -q --offline {pin}" in impact
        assert '"${args[@]}"' in impact
        session = (self.HOOKS / "diffctx-session.sh").read_text(encoding="utf-8")
        assert f"uvx -q {pin} --version" in session
        assert f"uvx diffctx=={__version__} . --diff --mode impact -f md" in session
        assert f"uvx diffctx=={__version__} . --symbol NAME -f md" in session
        assert "&)" in session
        assert _load("plugin/hooks/hooks.json")["hooks"]["SessionStart"][0]["hooks"][0]["timeout"] <= 10

    def test_the_skills_name_the_pinned_package(self):
        """The no-MCP fallback in each skill is a command the model runs, so it
        carries the exact version too; cd.yml bumps it with the hooks."""
        for skill in sorted((PROJECT_ROOT / "plugin" / "skills").glob("*/SKILL.md")):
            text = skill.read_text(encoding="utf-8")
            specs = re.findall(r"uvx (diffctx\S*)", text)
            assert specs, skill
            assert all(spec == f"diffctx=={__version__}" for spec in specs), (skill, specs)

    def test_the_hook_options_are_declared(self):
        options = _load("plugin/.claude-plugin/plugin.json")["userConfig"]
        assert options["impact_hook"]["type"] == "boolean"
        assert options["impact_hook"]["default"] is True
        assert options["impact_gate"]["type"] == "boolean"
        assert options["impact_gate"]["default"] is False

    @pytest.mark.parametrize("command", ["diffctx", "impact", "commit"])
    def test_skill_descriptions_fit_the_trigger_cap(self, command):
        text = (PROJECT_ROOT / "plugin" / "skills" / command / "SKILL.md").read_text(encoding="utf-8")
        front = re.match(r"\A---\n(.*?)\n---\n", text, re.DOTALL).group(1)
        description = re.search(r"^description:\s*(.*)$", front, re.MULTILINE).group(1)
        assert len(description) < 1536


def _diffctx_hook_deadline() -> int:
    source = (PROJECT_ROOT / "crates" / "diffctx-native" / "src" / "hook.rs").read_text(encoding="utf-8")
    return int(re.search(r"HOOK_DEADLINE_SECS: u64 = (\d+)", source).group(1))


class TestDistributionPins:
    def test_glama_manifest_names_the_maintainer(self):
        assert "nikolay-e" in _load("glama.json")["maintainers"]

    def test_action_default_version_matches_package(self):
        action = (PROJECT_ROOT / "action.yml").read_text(encoding="utf-8")
        m = re.search(r"diffctx-version:.*?default:\s*([\d.]+)", action, re.DOTALL)
        assert m
        assert m.group(1) == __version__

    def test_action_docs_pin_a_tag_that_carries_the_action(self):
        """v1.12.2 and older tags predate action.yml — `uses: @<tag>` on them
        fails to resolve. The docs pin must never point below 1.12.3."""
        docs = (PROJECT_ROOT / "docs/product/github-action.md").read_text(encoding="utf-8")
        pins = {tuple(map(int, v.split("."))) for v in re.findall(r"nikolay-e/diffctx@v([\d.]+)", docs)}
        assert pins
        release = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", __version__)
        assert release, f"non-release version string: {__version__}"
        current = tuple(map(int, release.groups()))
        for pin in pins:
            assert (1, 12, 3) <= pin <= current

    def test_cd_republishes_the_registry_with_oidc(self):
        """The registry version must track releases; the job doing that needs
        the OIDC grant or it fails only at release time."""
        cd = (PROJECT_ROOT / ".github/workflows/cd.yml").read_text(encoding="utf-8")
        job = re.search(r"publish-mcp-registry:.*?(?=\n  [a-z-]+:\n)", cd, re.DOTALL)
        assert job
        assert "id-token: write" in job.group(0)
        assert "login github-oidc" in job.group(0)

    def test_mcp_publisher_download_is_pinned_and_verified(self):
        """The release assets are named linux_amd64 (Go convention), not the
        uname -m x86_64 — and an unpinned binary download in a publishing job
        is a supply-chain hole. Both invariants live or die together here."""
        cd = (PROJECT_ROOT / ".github/workflows/cd.yml").read_text(encoding="utf-8")
        job = re.search(r"publish-mcp-registry:.*?(?=\n  [a-z-]+:\n)", cd, re.DOTALL)
        assert job
        assert "mcp-publisher_linux_amd64.tar.gz" in job.group(0)
        assert "sha256sum -c" in job.group(0)
        assert re.search(r"PUBLISHER_SHA256=[0-9a-f]{64}", job.group(0))


class TestNativeModuleStub:
    def test_stub_names_match_the_runtime_module(self):
        """diffctx._diffctx ships a .pyi so editor type-checkers see the
        native surface; a stub that drifts from the module is worse than
        none. Names must match exactly in both directions."""
        import ast

        import diffctx._diffctx as native

        runtime = {n for n in dir(native) if not n.startswith("_")}
        tree = ast.parse((PROJECT_ROOT / "src/diffctx/_diffctx.pyi").read_text(encoding="utf-8"))
        stubbed = {node.name for node in tree.body if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))}
        # Module-level constants are part of the surface too: the engine exports
        # the shipped defaults so the Python layers stop restating them (#175),
        # and a stub that omits them hides exactly the names callers should be
        # reading instead of hardcoding.
        stubbed |= {
            target.id for node in tree.body if isinstance(node, ast.AnnAssign) and isinstance(target := node.target, ast.Name)
        }
        assert stubbed == runtime, f"stub-only: {stubbed - runtime}; runtime-only: {runtime - stubbed}"


class TestLandingPageClaims:
    """The landing page is where a stranger decides whether to adopt the tool.

    Its headline comparison shipped for months as `48,210 -> 6,930 tokens, 7x`
    with no source anywhere — not on the page, not in the repository. Three
    independent first-visit probes all reached the same verdict: the one number
    meant to justify adoption could not be checked. A figure a reader cannot
    reproduce is worth less than no figure, so the gate is provenance, not the
    value: the stat block must name a public commit and the command that
    produces it. The numbers themselves are deliberately NOT pinned here — they
    move with the engine, and a test that froze them would only teach the next
    author to edit the test.
    """

    @staticmethod
    def _ratio_block() -> str:
        html = (PROJECT_ROOT / "docs" / "index.html").read_text(encoding="utf-8")
        start = html.index('<div class="ratio">')
        return html[start : html.index("</div>", html.index('class="source"'))]

    def test_the_headline_comparison_names_its_source(self):
        block = self._ratio_block()
        assert re.search(
            r'href="https://github\.com/[^"]+/commit/[0-9a-f]{7,40}"', block
        ), "the headline stat must link the commit it was measured on"
        assert "diffctx . --diff" in block, "the headline stat must show the command that reproduces it"
        assert "o200k" in block, "the headline stat must name the tokenizer it counted with"

    def test_every_demo_dial_names_the_flag_it_stands_for(self):
        html = (PROJECT_ROOT / "docs" / "index.html").read_text(encoding="utf-8")
        from diffctx.cli import _build_main_parser

        flags = {o for a in _build_main_parser(prog="diffctx", version="x")._actions for o in a.option_strings}
        for dial in ("alpha", "tau", "budget"):
            start = html.index(f'<label for="{dial}"')
            label = html[start : html.index("</label", start)]
            named = re.findall(r"--[a-z][a-z-]*", label)
            assert named, f"the {dial} dial does not say which flag it is"
            assert set(named) <= flags, f"the {dial} dial names a flag the CLI does not have: {named}"


class TestLandingPagePwa:
    """The Pages site installs and opens offline: a manifest whose icons are
    real PNGs of the declared size, and a service worker the page registers
    whose precache names only files that exist."""

    DOCS = PROJECT_ROOT / "docs"

    @staticmethod
    def _png_size(path: Path) -> tuple[int, int]:
        header = path.read_bytes()[:24]
        assert header[:8] == b"\x89PNG\r\n\x1a\n", path
        return int.from_bytes(header[16:20], "big"), int.from_bytes(header[20:24], "big")

    def test_manifest_icons_exist_at_their_declared_sizes(self):
        manifest = json.loads((self.DOCS / "manifest.webmanifest").read_text(encoding="utf-8"))
        purposes = set()
        for icon in manifest["icons"]:
            path = self.DOCS / icon["src"].removeprefix("/")
            width, height = self._png_size(path)
            assert icon["sizes"] == f"{width}x{height}", path
            purposes.add(icon["purpose"])
        assert purposes == {"any", "maskable"}
        assert manifest["display"] == "standalone"
        assert manifest["theme_color"] == manifest["background_color"]

    def test_page_links_the_manifest_and_registers_the_worker(self):
        page = (self.DOCS / "index.html").read_text(encoding="utf-8")
        assert 'rel="manifest" href="/manifest.webmanifest"' in page
        assert 'rel="apple-touch-icon" href="/icons/apple-touch-icon.png"' in page
        assert (self.DOCS / "icons/apple-touch-icon.png").is_file()
        assert 'serviceWorker.register("/sw.js")' in page

    def test_the_site_root_serves_a_favicon(self):
        # Chrome requests /favicon.ico on its own whatever the page declares;
        # a 404 there is the console error every first visit logged.
        assert (self.DOCS / "favicon.ico").read_bytes()[:4] == b"\x00\x00\x01\x00"

    def test_page_takes_the_whole_ios_screen_and_hands_the_insets_back(self):
        page = (self.DOCS / "index.html").read_text(encoding="utf-8")
        assert "viewport-fit=cover" in page
        assert '<meta name="apple-mobile-web-app-status-bar-style" content="black-translucent" />' in page
        assert "min-height: calc(100% + env(safe-area-inset-top))" in page
        assert "padding-top: env(safe-area-inset-top)" in page

    @staticmethod
    def _precached() -> list[str]:
        worker = (PROJECT_ROOT / "docs" / "sw.js").read_text(encoding="utf-8")
        shell = re.search(r"SHELL_FILES = \[([^\]]+)\]", worker)
        assert shell is not None
        return re.findall(r'"([^"]+)"', shell.group(1))

    def test_worker_precache_names_only_files_that_exist(self):
        for entry in self._precached():
            target = self.DOCS / (entry.removeprefix("./") or "index.html")
            # Pages renders product/*.md to *.html; the source is what the
            # repository holds.
            assert target.is_file() or target.with_suffix(".md").is_file(), entry

    def test_every_manifest_shortcut_is_precached(self):
        manifest = json.loads((self.DOCS / "manifest.webmanifest").read_text(encoding="utf-8"))
        precached = {entry.removeprefix("./") for entry in self._precached()}
        for shortcut in manifest["shortcuts"]:
            assert shortcut["url"].removeprefix("/") in precached, shortcut


class TestLandingPageDiscoverability:
    """Search engines index the host docs/CNAME names: every absolute URL the
    site publishes lives on it, and the sitemap lists only pages that exist."""

    DOCS = PROJECT_ROOT / "docs"

    def _origin(self) -> str:
        return "https://" + (self.DOCS / "CNAME").read_text(encoding="utf-8").strip()

    def test_canonical_urls_name_the_custom_domain(self):
        page = (self.DOCS / "index.html").read_text(encoding="utf-8")
        origin = self._origin()
        assert f'<link rel="canonical" href="{origin}/" />' in page
        assert f'property="og:url" content="{origin}/"' in page
        assert "github.io" not in page
        card = re.search(r'property="og:image" content="([^"]+)"', page)
        assert card is not None
        assert card.group(1).startswith(origin + "/"), card.group(1)
        image = self.DOCS / card.group(1).removeprefix(origin + "/")
        assert TestLandingPagePwa._png_size(image) == (1200, 630)

    def test_robots_points_at_the_sitemap(self):
        robots = (self.DOCS / "robots.txt").read_text(encoding="utf-8")
        assert f"Sitemap: {self._origin()}/sitemap.xml" in robots.splitlines()

    def test_sitemap_lists_only_published_pages(self):
        origin = self._origin()
        locs = re.findall(r"<loc>([^<]+)</loc>", (self.DOCS / "sitemap.xml").read_text(encoding="utf-8"))
        assert f"{origin}/" in locs
        for loc in locs:
            assert loc.startswith(origin + "/"), loc
            target = self.DOCS / (loc.removeprefix(origin + "/") or "index.html")
            assert target.is_file() or target.with_suffix(".md").is_file(), loc
