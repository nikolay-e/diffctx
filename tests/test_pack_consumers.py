"""The two things outside the engine that read a pack document by shape.

The release smoke reads `provenance.engine.build` from the JSON (`ci.yml`,
`cd.yml`), and the PR dogfood script reads the Markdown's changed-files block,
fragment headings and withheld footer. A schema change that moves either would
break a release or a review comment without failing an engine test.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).resolve().parents[1]


@pytest.fixture
def repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("lib.py", "def helper(x):\n    return x + 1\n")
    repo.add_file("main.py", "from lib import helper\n\nprint(helper(1))\n")
    repo.commit("initial")
    repo.add_file("lib.py", "def helper(x):\n    return x + 2\n")
    repo.add_file("notes.txt", "released\n")
    repo.commit("bump helper")
    return repo


def test_the_engine_is_named_where_the_release_smoke_reads_it(repo):
    pack = json.loads(diffctx.to_json(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1..HEAD")))
    engine = pack["provenance"]["engine"]
    assert engine["name"] == "diffctx"
    assert engine["version"]
    # `build` is the release pipeline's commit; a source build carries none.
    assert set(engine) <= {"name", "version", "build"}

    for workflow in ("ci.yml", "cd.yml"):
        text = (PROJECT_ROOT / ".github" / "workflows" / workflow).read_text(encoding="utf-8")
        assert ".provenance.engine.build" in text, f"{workflow} no longer reads the build label from the pack"


@pytest.mark.skipif(sys.platform == "win32", reason="the dogfood script is bash")
def test_the_pr_dogfood_script_reads_the_pack_markdown(repo, tmp_path):
    pack = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1..HEAD")
    context = tmp_path / "context.md"
    context.write_text(diffctx.to_markdown(pack), encoding="utf-8")
    out = tmp_path / "comment.md"
    env = {
        "CONTEXT_FILE": str(context),
        "TOKEN_COUNT": "123",
        "DIFF_RANGE": "HEAD~1..HEAD",
        "EMPTY": "false",
        "RUN_URL": "https://example.invalid/run",
        "OUT": str(out),
        "PATH": "/usr/bin:/bin",
    }
    subprocess.run(
        ["bash", str(PROJECT_ROOT / ".github" / "scripts" / "pr-review-summary.sh")],
        check=True,
        capture_output=True,
        env=env,
    )
    comment = out.read_text(encoding="utf-8")
    fragments = len(pack["fragments"])
    changed = len(pack["inventory"]["changed"]["items"])
    assert f"{fragments} fragments across {changed} changed files" in comment
    for path in pack["inventory"]["changed"]["items"]:
        assert f"- `{path}`" in comment, path
    assert re.search(r"^\*\*Changed files:\*\*$", context.read_text(encoding="utf-8"), re.MULTILINE)
