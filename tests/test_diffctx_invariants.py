from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"

TOKEN_RE = re.compile(r"^([\d,]+)\s+tokens\b", re.MULTILINE)


def _make_diff_repo(tmp_path: Path) -> tuple[Pygit2Repo, str]:
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file(
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n\ndef mul(a, b):\n    return a * b\n",
    )
    repo.add_file(
        "src/main.py",
        "from calc import add, sub, mul\n\ndef run():\n    return add(1, 2) + sub(3, 1) + mul(2, 4)\n",
    )
    repo.add_file("README.md", "# Demo\n\nUses `calc` helpers.\n")
    base = repo.commit("initial")

    repo.add_file(
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\n"
        "def sub(a, b):\n    return a - b\n\n"
        "def mul(a, b):\n    return a * b\n\n"
        "def div(a, b):\n    if b == 0:\n        raise ZeroDivisionError\n    return a / b\n",
    )
    head = repo.commit("add div")
    return repo, f"{base}..{head}"


def _run(
    cwd: Path,
    args: list[str],
    extra_env: dict[str, str] | None = None,
) -> tuple[str, str]:
    cmd = [sys.executable, "-m", "treemapper", *args]
    env = {"PYTHONPATH": str(SRC_DIR)}
    if extra_env:
        env.update(extra_env)
    result = subprocess.run(
        cmd,
        cwd=cwd,
        env={**dict(__import__("os").environ), **env},
        capture_output=True,
        text=True,
        check=True,
    )
    return result.stdout, result.stderr


def _extract_tokens(combined_output: str) -> int:
    m = TOKEN_RE.search(combined_output)
    assert m, f"Could not parse token count from output:\n{combined_output[:500]}"
    return int(m.group(1).replace(",", ""))


def test_diffctx_output_is_byte_identical_across_runs(tmp_path):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]
    runs = [_run(repo.path, args)[0] for _ in range(5)]
    distinct = set(runs)
    assert (
        len(distinct) == 1
    ), f"Non-deterministic output: {len(distinct)} distinct outputs across 5 runs. First diff: {next(iter(distinct))[:300]}"


@pytest.mark.parametrize("threads", ["1", "2", "4", "14"])
def test_diffctx_output_is_invariant_under_rayon_thread_count(tmp_path, threads):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]
    baseline_out, _ = _run(repo.path, args, {"RAYON_NUM_THREADS": "1"})
    actual_out, _ = _run(repo.path, args, {"RAYON_NUM_THREADS": threads})
    assert baseline_out == actual_out, (
        f"Non-determinism under RAYON_NUM_THREADS={threads}: "
        f"output differs from RAYON_NUM_THREADS=1. "
        f"This indicates a parallel reduce or concurrent state mutation race."
    )


@pytest.mark.parametrize("objective", ["submodular", "boltzmann"])
def test_diffctx_objective_modes_are_deterministic(tmp_path, objective):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]
    runs = [_run(repo.path, args, {"DIFFCTX_OBJECTIVE": objective})[0] for _ in range(3)]
    assert len(set(runs)) == 1, f"Non-determinism in DIFFCTX_OBJECTIVE={objective} mode across 3 runs."


def test_extreme_core_budget_fraction_is_clamped(tmp_path):
    repo, diff_range = _make_diff_repo(tmp_path)
    budget = 1024
    args = [".", "--diff", diff_range, "--budget", str(budget), "-f", "txt"]

    out, err = _run(repo.path, args)
    baseline_tokens = _extract_tokens(out + err)
    out, err = _run(repo.path, args, {"DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION": "42"})
    extreme_tokens = _extract_tokens(out + err)

    assert extreme_tokens < 4 * budget, (
        f"core_budget_fraction=42 should clamp to 1.0, but produced {extreme_tokens} tokens "
        f"(budget={budget}, baseline={baseline_tokens}). Underflow likely."
    )


@pytest.mark.parametrize(
    "env_var",
    [
        "DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION",
        "DIFFCTX_OP_RESCUE_BUDGET_FRACTION",
        "DIFFCTX_OP_PPR_ALPHA",
        "DIFFCTX_OP_PPR_FORWARD_BLEND",
    ],
)
def test_fraction_param_rejects_negative_falls_back_to_default(tmp_path, env_var):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]

    baseline_out, _ = _run(repo.path, args)
    negative_out, _ = _run(repo.path, args, {env_var: "-1.0"})

    assert (
        baseline_out == negative_out
    ), f"{env_var}=-1.0 should be rejected and fall back to default, but stdout differs (clamp/reject path inconsistent)."


def test_unreachable_revision_raises_clear_error(tmp_path):
    repo, _ = _make_diff_repo(tmp_path)
    args = [".", "--diff", "1111111111111111111111111111111111111111..HEAD", "--budget", "1024"]
    cmd = [sys.executable, "-m", "treemapper", *args]
    env = {**dict(__import__("os").environ), "PYTHONPATH": str(SRC_DIR)}
    result = subprocess.run(cmd, cwd=repo.path, env=env, capture_output=True, text=True, check=False)
    assert result.returncode != 0, (
        f"Unreachable revision must raise a non-zero exit, not silently empty. "
        f"stdout={result.stdout[:200]} stderr={result.stderr[:200]}"
    )


def test_zero_commit_repo_raises_clear_error(tmp_path):
    empty_repo = tmp_path / "empty_repo"
    empty_repo.mkdir()
    Pygit2Repo(empty_repo)
    args = [".", "--diff", "HEAD~1..HEAD", "--budget", "1024"]
    cmd = [sys.executable, "-m", "treemapper", *args]
    env = {**dict(__import__("os").environ), "PYTHONPATH": str(SRC_DIR)}
    result = subprocess.run(cmd, cwd=empty_repo, env=env, capture_output=True, text=True, check=False)
    assert result.returncode != 0, (
        f"Zero-commit repo must raise a non-zero exit, not silently empty. "
        f"stdout={result.stdout[:200]} stderr={result.stderr[:200]}"
    )


def test_max_fragments_zero_falls_back_to_default(tmp_path):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]
    out, err = _run(repo.path, args, {"TREEMAPPER_MAX_FRAGMENTS": "0"})
    tokens = _extract_tokens(out + err)
    assert (
        tokens > 0
    ), f"TREEMAPPER_MAX_FRAGMENTS=0 produced empty output; should be rejected and fall back to default. tokens={tokens}"


def test_ppr_alpha_one_does_not_degenerate(tmp_path):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]
    out, err = _run(repo.path, args, {"DIFFCTX_OP_PPR_ALPHA": "1.0", "DIFFCTX_SCORING": "ppr"})
    tokens = _extract_tokens(out + err)
    assert tokens > 0, (
        f"DIFFCTX_OP_PPR_ALPHA=1.0 produced empty output; PPR restart=0 must be clamped "
        f"to avoid all-zero rankings. tokens={tokens}"
    )


def test_release_profile_aborts_on_panic():
    cargo_toml = (PROJECT_ROOT / "diffctx" / "Cargo.toml").read_text()
    assert 'panic = "abort"' in cargo_toml, (
        'diffctx/Cargo.toml release profile must set panic = "abort". '
        "Removing it reintroduces UB on panic propagation across the PyO3 FFI boundary."
    )


def test_tiktoken_o200k_base_encoding_is_pinned():
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    fixture = "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n"
    tokens = enc.encode(fixture)
    assert len(tokens) == 24, (
        f"tiktoken o200k_base BPE drift: fixture now produces {len(tokens)} tokens, expected 24. "
        f"This breaks paper reproducibility — investigate before bumping tiktoken."
    )
    assert tokens[:5] == [
        1314,
        1147,
        6271,
        11,
        287,
    ], f"tiktoken o200k_base BPE drift: first 5 tokens changed to {tokens[:5]}, expected [1314, 1147, 6271, 11, 287]."
