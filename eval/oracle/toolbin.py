"""Release binaries the oracle runs: downloaded once, verified against the pinned SHA-256, never trusted unchecked."""

from __future__ import annotations

import gzip
import hashlib
import shutil
import tarfile
import tempfile
import urllib.request
from pathlib import Path

from corpus import ROOT, Tool

BIN = ROOT / "cache" / "bin"


def tool_path(tool: Tool) -> Path:
    return BIN / f"{tool.name}-{tool.version}" / tool.name


def install_tool(tool: Tool) -> Path:
    target = tool_path(tool)
    if target.exists():
        return target
    url = tool.download_url()
    if not url.startswith("https://"):
        raise ValueError(f"refusing a non-https download: {url}")
    expected = tool.sha256[tool.build_name()]
    with tempfile.TemporaryDirectory(prefix="oracle-tool-") as scratch:
        payload = Path(scratch) / "payload"
        with urllib.request.urlopen(url, timeout=300) as response:
            payload.write_bytes(response.read())
        actual = hashlib.sha256(payload.read_bytes()).hexdigest()
        if actual != expected:
            raise RuntimeError(f"{tool.name} checksum mismatch for {tool.build_name()}: expected {expected}, got {actual}")
        target.parent.mkdir(parents=True, exist_ok=True)
        _place(tool, payload, Path(scratch), target)
    target.chmod(0o755)
    return target


def _place(tool: Tool, payload: Path, scratch: Path, target: Path) -> None:
    if tool.archive == "file":
        shutil.copyfile(payload, target)
    elif tool.archive == "gz":
        with gzip.open(payload, "rb") as source, target.open("wb") as sink:
            shutil.copyfileobj(source, sink)
    else:
        with tarfile.open(payload) as tar:
            tar.extractall(scratch / "out", filter="data")
        unpacked = next(p for p in (scratch / "out").rglob(tool.name) if p.is_file())
        shutil.move(str(unpacked), target)
