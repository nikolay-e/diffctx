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

        referenced = set(re.findall(r"`(get_[a-z_]+)`", text))
        assert referenced

        from diffctx.mcp import server as mcp_server

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
        assert stubbed == runtime, f"stub-only: {stubbed - runtime}; runtime-only: {runtime - stubbed}"
