from __future__ import annotations

import json
import os
import subprocess
import sys
import textwrap
import threading
import time

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def _repo(tmp_path, name, files):
    repo = Pygit2Repo(tmp_path / name)
    for i in range(files):
        repo.add_file(f"mod_{i}.py", f"def fn_{i}():\n    return {i}\n")
    repo.add_file("main.py", "\n".join(f"from mod_{i} import fn_{i}" for i in range(files)) + "\n")
    repo.commit("initial")
    repo.add_file("main.py", "\n".join(f"from mod_{i} import fn_{i}" for i in range(files)) + "\nEXTRA = 1\n")
    repo.commit("change")
    return repo


def test_expired_deadline_in_one_run_does_not_kill_a_concurrent_run(tmp_path):
    healthy_repo = _repo(tmp_path, "healthy", files=40)
    expired_repo = _repo(tmp_path, "expired", files=2)
    results: dict[str, dict] = {}
    errors: dict[str, Exception] = {}
    started = threading.Event()

    def run(key, repo, timeout, delay):
        started.wait()
        time.sleep(delay)
        try:
            results[key] = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", timeout=timeout)
        except Exception as e:  # a ceiling this run cannot meet is an ordinary error
            errors[key] = e

    threads = [
        # timeout=0 expires at the first git call, which is the earliest point
        # a ceiling can bite; what is under test is the SIBLING run, which used
        # to inherit the expired ceiling from a process-global (#210).
        threading.Thread(target=run, args=("healthy", healthy_repo, 300, 0.0)),
        threading.Thread(target=run, args=("expired", expired_repo, 0, 0.05)),
    ]
    for t in threads:
        t.start()
    started.set()
    for t in threads:
        t.join(timeout=120)

    assert "healthy" in results, errors.get("healthy")
    assert results["healthy"].get("fragments")
    assert "expired" in errors, "a zero ceiling must fail its own run"


def test_a_zero_ceiling_says_it_timed_out_rather_than_denying_the_repo(tmp_path):
    repo = _repo(tmp_path, "honest", files=2)
    try:
        diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", timeout=0)
    except Exception as e:
        message = str(e)
    else:
        raise AssertionError("a zero ceiling must fail")
    # `is_git_repo` used to collapse a timed-out `rev-parse` into "false", so a
    # ceiling too small to let git answer accused the repository instead.
    assert "timeout" in message.lower()
    assert "not a git repository" not in message


@pytest.mark.parametrize("surface", ["pack", "locate"])
def test_a_compute_deadline_yields_a_partial_artifact_not_an_exception(tmp_path, surface):
    repo = _repo(tmp_path, "deadline", files=30)
    call = (
        "diffctx.build_diff_context(root_dir=root, diff_range='HEAD~1')"
        if surface == "pack"
        else "json.loads(build_locate(root, 'HEAD~1'))"
    )
    child = textwrap.dedent(f"""
        import json, diffctx
        from pathlib import Path
        from diffctx._native.pipeline import build_locate

        root = Path({str(repo.path)!r})
        r = {call}
        print(json.dumps({{"coverage": r.get("coverage"), "changed": r.get("changed_files") or []}}))
        """)
    proc = subprocess.run(
        [sys.executable, "-c", child],
        capture_output=True,
        text=True,
        timeout=120,
        env={**os.environ, "DIFFCTX_TEST_DEADLINE_EXPIRED": "1"},
    )
    assert proc.returncode == 0, f"the deadline escaped as an error: {proc.stderr[-400:]}"
    report = json.loads(proc.stdout.strip().splitlines()[-1])
    assert report["coverage"], "the ceiling never fired; the artifact claims to be complete"
    if surface == "pack":
        assert report["coverage"]["status"] == "partial"
    assert "deadline" in report["coverage"]["limit_reasons"]
    assert report["changed"] == ["main.py"], "a partial artifact still lists every changed file"


def test_impact_past_its_deadline_answers_partially(tmp_path):
    """#382: a range that outlives the deadline returned nothing at all; the
    symbols answered so far are an answer, marked as partial."""
    repo = _repo(tmp_path, "impact-deadline", files=30)
    child = textwrap.dedent(f"""
        import json
        from pathlib import Path
        from diffctx._native.pipeline import build_impact

        print(build_impact(Path({str(repo.path)!r}), 'HEAD~1'))
        """)
    proc = subprocess.run(
        [sys.executable, "-c", child],
        capture_output=True,
        text=True,
        timeout=120,
        env={**os.environ, "DIFFCTX_TEST_DEADLINE_EXPIRED": "1"},
    )
    assert proc.returncode == 0, f"the deadline escaped as an error: {proc.stderr[-400:]}"
    doc = json.loads(proc.stdout)
    assert "deadline" in doc.get("limits", []), doc
    assert doc["empty"] is False
    assert doc["changed_files"] == ["main.py"]


_STALLING_GIT = """#!/bin/sh
for a in "$@"; do
  if [ "$a" = cat-file ]; then echo $$ >> "$STALLED_PIDS"; exec sleep 600; fi
done
exec "$REAL_GIT" "$@"
"""


@pytest.mark.skipif(sys.platform == "win32", reason="the stalling git is a POSIX shell script")
def test_a_cancelled_request_stops_its_native_run_and_git_children(tmp_path, monkeypatch):
    # The MCP server abandons a worker it gave up on; without the token the
    # native run went on to its own 60 s deadline behind a stalled git.
    from diffctx._diffctx import CancelToken

    repo = _repo(tmp_path, "stalled", files=2)
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = bin_dir / "git"
    fake.write_text(_STALLING_GIT, encoding="utf-8")
    fake.chmod(0o755)
    real_git = subprocess.run(["sh", "-c", "command -v git"], capture_output=True, text=True, check=True).stdout.strip()
    monkeypatch.setenv("REAL_GIT", real_git)
    pids_file = tmp_path / "stalled.pids"
    monkeypatch.setenv("STALLED_PIDS", str(pids_file))
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")

    token = CancelToken()
    outcome: dict[str, object] = {}

    def work():
        try:
            outcome["result"] = token.scope(
                lambda: diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1..HEAD", timeout=60)
            )
        except Exception as e:  # a withdrawn run may end as an error or a partial artifact
            outcome["error"] = e

    worker = threading.Thread(target=work)
    worker.start()
    time.sleep(1.0)
    cancelled_at = time.monotonic()
    token.cancel()
    worker.join(timeout=30)
    assert not worker.is_alive(), "the cancelled run kept going"
    assert time.monotonic() - cancelled_at < 5
    if "result" in outcome:
        coverage = json.dumps(outcome["result"].get("coverage"))
        assert "cancelled" in coverage, coverage
    stalled = [int(p) for p in pids_file.read_text(encoding="utf-8").split()]
    assert stalled, "the stalling git never ran: the test measured nothing"
    time.sleep(0.5)
    assert not [pid for pid in stalled if _alive(pid)], "a killed git child outlived the cancellation"


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True
