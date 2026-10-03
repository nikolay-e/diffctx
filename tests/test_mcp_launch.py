from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from diffctx.version import __version__

# The base package installs diffctx-mcp although anyio and mcp come only with
# the [mcp] extra (#333). Hiding them through sys.modules reproduces a base
# install inside any environment, including one where the extra is present.
_WITHOUT_EXTRA = "import sys; sys.modules['anyio'] = None; sys.modules['mcp'] = None; "
_ENTRYPOINTS = {
    "diffctx-mcp": "sys.argv = ['diffctx-mcp', *sys.argv[1:]]; from diffctx.mcp.launch import main; main()",
    "diffctx mcp": "sys.argv = ['diffctx', 'mcp', *sys.argv[1:]]; from diffctx.cli import main; main()",
}


def _run(code: str, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-c", code, *args], capture_output=True, text=True, timeout=60, stdin=subprocess.DEVNULL
    )


@pytest.mark.parametrize("entry", list(_ENTRYPOINTS))
def test_missing_extra_is_an_instruction_not_a_traceback(entry):
    result = _run(_WITHOUT_EXTRA + _ENTRYPOINTS[entry])
    assert result.returncode == 3, result.stderr
    assert "Traceback" not in result.stderr
    assert "diffctx[mcp]" in result.stderr
    assert result.stdout == ""


@pytest.mark.parametrize("entry", list(_ENTRYPOINTS))
@pytest.mark.parametrize("flag", ["--version", "-v"])
def test_version_needs_no_extra(entry, flag):
    result = _run(_WITHOUT_EXTRA + _ENTRYPOINTS[entry], flag)
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == f"{entry} {__version__}"


@pytest.mark.parametrize("entry", list(_ENTRYPOINTS))
def test_help_needs_no_extra(entry):
    result = _run(_WITHOUT_EXTRA + _ENTRYPOINTS[entry], "--help")
    assert result.returncode == 0, result.stderr
    assert result.stdout.startswith(f"usage: {entry}")


def test_installed_script_reports_the_same_version_as_the_cli():
    bin_dir = str(Path(sys.executable).parent)
    script, cli = shutil.which("diffctx-mcp", path=bin_dir), shutil.which("diffctx", path=bin_dir)
    if script is None or cli is None:
        pytest.skip("the console scripts are not installed beside this interpreter")
    mcp_version = subprocess.run([script, "--version"], capture_output=True, text=True, timeout=60, check=True).stdout
    cli_version = subprocess.run([cli, "--version"], capture_output=True, text=True, timeout=60, check=True).stdout
    assert mcp_version.split()[-1] == cli_version.split()[-1] == __version__
