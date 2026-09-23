from __future__ import annotations

import json
import subprocess
from pathlib import Path

from tests.conftest import run_diffctx_subprocess

VUE_BEFORE = """<template>
  <p>{{ total }}</p>
</template>

<script setup lang="ts">
import { formatPrice } from "../lib/money";

function sumLines(items: number[]): number {
  return items.reduce((a, b) => a + b, 0);
}
</script>
"""

VUE_AFTER = VUE_BEFORE.replace("a + b, 0", "a + b, 1")


def _git(repo: Path, *args: str) -> None:
    subprocess.run(["git", "-c", "user.email=t@e", "-c", "user.name=t", *args], cwd=repo, check=True, capture_output=True)


def _repo(tmp_path: Path, before: dict[str, str | bytes], after: dict[str, str | bytes]) -> Path:
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    for files, message in ((before, "base"), (after, "change")):
        for rel, content in files.items():
            target = repo / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            if isinstance(content, bytes):
                target.write_bytes(content)
            else:
                target.write_text(content, encoding="utf-8")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", message)
    return repo


def test_a_vue_script_setup_change_shows_its_function(tmp_path: Path) -> None:
    repo = _repo(tmp_path, {"ui/Cart.vue": VUE_BEFORE}, {"ui/Cart.vue": VUE_AFTER})
    doc = json.loads(run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"], cwd=repo).stdout)
    changed = [f for f in doc["fragments"] if f.get("role") == "changed"]
    assert [(f["kind"], f["lines"]) for f in changed] == [("function", "9-9")]
    md = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--full", "-q"], cwd=repo).stdout
    assert "function sumLines(items: number[]): number {" in md


def test_crlf_fragment_lines_match_the_file(tmp_path: Path) -> None:
    before = b"def one():\r\n    return 1\r\n\r\n\r\ndef two():\r\n    return 2\r\n"
    after = before.replace(b"return 2", b"return 3")
    repo = _repo(tmp_path, {"app.py": before}, {"app.py": after})
    doc = json.loads(run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"], cwd=repo).stdout)
    changed = [f for f in doc["fragments"] if f.get("role") == "changed"]
    assert [(f.get("symbol"), f["lines"]) for f in changed] == [("two", "5-6")]
    assert "\r" not in changed[0]["content"]
