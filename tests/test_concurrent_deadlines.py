from __future__ import annotations

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
    errors: dict[str, BaseException] = {}
    started = threading.Event()

    def run(key, repo, timeout, delay):
        started.wait()
        time.sleep(delay)
        try:
            results[key] = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", timeout=timeout)
        except BaseException as e:  # pyo3 surfaces the deadline panic as a BaseException
            errors[key] = e

    threads = [
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
