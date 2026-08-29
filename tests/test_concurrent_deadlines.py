from __future__ import annotations

import subprocess
import sys
import textwrap
import threading
import time

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


def test_a_compute_deadline_never_takes_the_process_down(tmp_path):
    """The deadline is a panic (the phases it guards return no `Result`), so how
    it crosses the FFI boundary is the whole question. Under the release
    profile's old `panic = "abort"` this exact expiry killed the interpreter
    with SIGABRT — measured on a published wheel, and fatal for the MCP server,
    which abandons a timed-out worker and keeps serving. Whether the ceiling
    actually fires here is timing; that the process survives either way is not.
    """
    repo = _repo(tmp_path, "slow", files=200)
    child = textwrap.dedent(f"""
        import diffctx

        try:
            diffctx.build_diff_context(
                root_dir={str(repo.path)!r}, diff_range="HEAD~1", timeout=1
            )
            print("COMPLETED")
        except TimeoutError:
            print("TIMEOUT")
        """)
    proc = subprocess.run([sys.executable, "-c", child], capture_output=True, text=True, timeout=600)

    assert proc.returncode >= 0, f"the child died on signal {-proc.returncode}: {proc.stderr[-400:]}"
    assert proc.returncode == 0, f"the deadline escaped as an uncatchable error: {proc.stderr[-400:]}"
    assert proc.stdout.strip() in {"COMPLETED", "TIMEOUT"}, proc.stdout
