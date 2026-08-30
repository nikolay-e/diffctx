from __future__ import annotations

import json
import re
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
        assert _load(".claude-plugin/plugin.json")["version"] == __version__

    def test_mcp_json_is_self_bootstrapping(self):
        server = _load(".mcp.json")["mcpServers"]["diffctx"]
        assert server["command"] == "uvx"
        assert server["args"] == UVX_ARGS

    @pytest.mark.parametrize("command", ["diffctx", "impact"])
    def test_slash_command_has_frontmatter_and_a_real_tool(self, command):
        """Plugin commands instruct the model to call an MCP tool by name;
        a tool rename that skips these files ships a plugin whose commands
        reference nothing."""
        text = (PROJECT_ROOT / "commands" / f"{command}.md").read_text(encoding="utf-8")
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

        from diffctx.mcp import server as mcp_server

        # The default surface, deliberately: a plugin command that only works
        # once the operator sets DIFFCTX_MCP_LEGACY_TOOLS is a broken command.
        exported = {t.name for t in mcp_server.mcp._tool_manager.list_tools()}
        assert referenced <= exported, f"unknown tools referenced: {referenced - exported}"


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
