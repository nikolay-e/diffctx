"""The reported latency phases must add up to the reported total (#183).

Without this invariant a stage added outside the instrumentation disappears
silently: a 182s run reported 5.3s of phases and nothing said where the rest
went, which is what made a slow range look like an unexplained hang. A gap here
should fail loudly rather than surface as a wrong number in a results table.
"""

from __future__ import annotations

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

# Every phase that is expected to partition the run. `scoring_selection_ms` and
# `total_ms` are aggregates, not phases, so they are excluded from the sum.
PHASE_KEYS = [
    "pre_phase_ms",
    "parse_changed_ms",
    "universe_walk_ms",
    "discovery_ms",
    "parse_discovered_ms",
    "tokenization_ms",
    "graph_build_ms",
    "scoring_ms",
    "selection_ms",
]


@pytest.fixture
def latency_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "latency_repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file("src/main.py", "from calc import add\n\n\ndef run():\n    return add(1, 2)\n")
    repo.commit("initial")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n\n\ndef sub(a, b):\n    return a - b\n")
    repo.commit("add sub")
    return repo


def _latency(repo_path):
    from diffctx._native.pipeline import build_diff_context

    result = build_diff_context(repo_path, "HEAD~1..HEAD", budget_tokens=8000)
    latency = result.get("latency")
    assert latency is not None, "the pipeline reported no latency block at all"
    return latency


def test_every_phase_key_is_reported(latency_repo):
    latency = _latency(latency_repo.path)
    missing = [k for k in PHASE_KEYS if k not in latency]
    assert not missing, f"phases missing from the latency block: {missing}"


def test_phases_account_for_the_reported_total(latency_repo):
    """Anything unaccounted for shows up here as a residual. Render is the only
    stage deliberately outside the phase list, so the tolerance is small and
    absolute rather than proportional — a proportional bound would hide a
    genuinely large gap on a slow run."""
    latency = _latency(latency_repo.path)
    phase_sum = sum(latency[k] for k in PHASE_KEYS)
    total = latency["total_ms"]

    assert total > 0, "total_ms was not measured"
    residual = abs(total - phase_sum)
    assert residual < 50, (
        f"phases sum to {phase_sum:.1f}ms but total is {total:.1f}ms "
        f"(residual {residual:.1f}ms) — a stage is running outside the "
        f"instrumentation: {dict(sorted(latency.items()) )}"
    )


def test_the_pre_phase_is_actually_measured(latency_repo):
    """It covers the `git diff` calls and ignore resolution, so it can never
    legitimately be zero — zero means the timer was never started."""
    latency = _latency(latency_repo.path)
    assert latency["pre_phase_ms"] > 0, (
        "pre_phase_ms is zero: the git and ignore-resolution work before the " "heavy phase is not being timed"
    )


def test_selection_covers_the_post_passes(latency_repo):
    """`selection_ms` is the only home for the three post-passes. If it were
    reset to cover the greedy alone, their cost would vanish from the accounting
    while still being spent."""
    latency = _latency(latency_repo.path)
    assert latency["selection_ms"] > 0
    assert latency["selection_ms"] <= latency["total_ms"]
