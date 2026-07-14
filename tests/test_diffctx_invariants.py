from __future__ import annotations

import itertools
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
    cmd = [sys.executable, "-m", "diffctx", *args]
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
    cmd = [sys.executable, "-m", "diffctx", *args]
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
    cmd = [sys.executable, "-m", "diffctx", *args]
    env = {**dict(__import__("os").environ), "PYTHONPATH": str(SRC_DIR)}
    result = subprocess.run(cmd, cwd=empty_repo, env=env, capture_output=True, text=True, check=False)
    assert result.returncode != 0, (
        f"Zero-commit repo must raise a non-zero exit, not silently empty. "
        f"stdout={result.stdout[:200]} stderr={result.stderr[:200]}"
    )


def test_max_fragments_zero_falls_back_to_default(tmp_path):
    repo, diff_range = _make_diff_repo(tmp_path)
    args = [".", "--diff", diff_range, "--budget", "1024", "-f", "txt"]
    out, err = _run(repo.path, args, {"DIFFCTX_MAX_FRAGMENTS": "0"})
    tokens = _extract_tokens(out + err)
    assert (
        tokens > 0
    ), f"DIFFCTX_MAX_FRAGMENTS=0 produced empty output; should be rejected and fall back to default. tokens={tokens}"


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


def test_diff_context_output_has_orientation_header_and_roles(tmp_path):
    import json

    repo, diff_range = _make_diff_repo(tmp_path)
    stdout, _ = _run(repo.path, [".", "--diff", diff_range, "--budget", "2048", "-f", "json"])
    out = json.loads(stdout)

    assert out["commit_message"] == "add div"
    assert "src/calc.py" in out["changed_files"]

    fragments = out["fragments"]
    roles = [f.get("role") for f in fragments]
    changed_count = sum(1 for r in roles if r == "changed")
    assert changed_count >= 1, "the changed code must be marked role=changed"

    changed_positions = [i for i, r in enumerate(roles) if r == "changed"]
    assert changed_positions == list(
        range(changed_count)
    ), f"changed fragments must come first; got positions {changed_positions}"

    changed_paths = {f["path"] for f in fragments if f.get("role") == "changed"}
    assert "src/calc.py" in changed_paths


def test_diff_context_merges_contiguous_fragments(tmp_path):
    import json

    repo, diff_range = _make_diff_repo(tmp_path)
    stdout, _ = _run(repo.path, [".", "--diff", diff_range, "--budget", "4096", "-f", "json"])
    fragments = json.loads(stdout)["fragments"]

    for f in fragments:
        start, end = (int(x) for x in f["lines"].split("-"))
        assert start <= end, f"fragment line range inverted: {f['lines']}"


def test_diff_context_no_contained_or_unmerged_fragments(tmp_path):
    """Regression (f1a1647d): the render merge pass must, per (path, role),
    drop any fragment fully contained in another and merge line-contiguous
    runs. A change editing several functions in one file produces multiple
    same-role fragments, exercising that pass; the output must never carry a
    range nested inside a sibling nor an unmerged `next.start == cur.end + 1`
    pair (both are pure per-fragment scaffolding duplication)."""
    import json
    from collections import defaultdict

    repo = Pygit2Repo(tmp_path / "repo")
    base_src = "".join(f"def f{i}(x):\n    y = x + {i}\n    return y\n\n" for i in range(12))
    repo.add_file("src/mod.py", base_src)
    repo.add_file("src/other.py", "from mod import f0, f5, f11\n\ndef run():\n    return f0(1) + f5(2) + f11(3)\n")
    base = repo.commit("init")
    edited_src = "".join(
        (
            f"def f{i}(x):\n    y = x * {i}\n    z = y + 1\n    return z\n\n"
            if i in (2, 5, 9)
            else f"def f{i}(x):\n    y = x + {i}\n    return y\n\n"
        )
        for i in range(12)
    )
    repo.add_file("src/mod.py", edited_src)
    head = repo.commit("edit several functions")

    stdout, _ = _run(repo.path, [".", "--diff", f"{base}..{head}", "--budget", "8000", "-f", "json"])
    fragments = json.loads(stdout)["fragments"]

    by_key: dict[tuple[str, str | None], list[tuple[int, int]]] = defaultdict(list)
    for f in fragments:
        start, end = (int(x) for x in f["lines"].split("-"))
        by_key[(f["path"], f.get("role"))].append((start, end))

    assert any(
        len(ranges) >= 2 for ranges in by_key.values()
    ), "fixture must yield a multi-fragment group to exercise the merge pass"

    for (path, role), ranges in by_key.items():
        ordered = sorted(ranges)
        for (a_start, a_end), (b_start, b_end) in itertools.pairwise(ordered):
            assert not (
                b_start >= a_start and b_end <= a_end
            ), f"contained fragment {(b_start, b_end)} inside {(a_start, a_end)} for {path} role={role}"
            assert (
                b_start != a_end + 1
            ), f"unmerged contiguous fragments {(a_start, a_end)} and {(b_start, b_end)} for {path} role={role}"


def test_diff_context_scopes_markdown_preamble_change_not_whole_file(tmp_path):
    """Regression (#91): a change to a lone H1's own preamble (before its
    first `##` child heading) used to select a fragment spanning the H1's
    entire nested span — the rest of the document, down to EOF — instead of
    just the touched paragraph, because a heading's fragment extended to the
    next same-or-higher-level heading rather than the very next heading."""
    import json

    repo = Pygit2Repo(tmp_path / "repo")
    body = "\n".join(f"## Section {i}\nBody text for section {i}.\n" for i in range(8))
    repo.add_file("NOTES.md", f"# Notes\n\nOriginal preamble sentence.\n\n{body}")
    base = repo.commit("initial")
    repo.add_file("NOTES.md", f"# Notes\n\nUpdated preamble sentence.\n\n{body}")
    head = repo.commit("update preamble")

    stdout, _ = _run(repo.path, [".", "--diff", f"{base}..{head}", "-f", "json"])
    fragments = json.loads(stdout)["fragments"]

    changed = [f for f in fragments if f.get("role") == "changed" and f["path"] == "NOTES.md"]
    assert changed, "expected a role=changed fragment for NOTES.md"
    for f in changed:
        start, end = (int(x) for x in f["lines"].split("-"))
        assert end - start < 8, f"H1 preamble fragment should stay small, got lines {f['lines']} (whole-file dump?)"
        assert "Section 7" not in f["content"], "fragment should not pull in the last child section"


def test_deletion_only_diff_reports_deleted_files_via_bridge(tmp_path):
    """The CLI path emits deleted_files/renamed_files on a deletion-only diff
    (empty_output_from_state); the pybridge select_with_params empty branch
    used to return a bare skeleton instead, so MCP/benchmark consumers lost
    the only signal the diff carried."""
    from diffctx._diffctx import compute_scored_state, select_with_params

    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("kept.py", "def kept():\n    return 1\n")
    repo.add_file("doomed.py", "def doomed():\n    return 2\n")
    repo.commit("base")
    repo.remove_file("doomed.py")
    repo.commit("delete doomed")

    state = compute_scored_state(str(repo.path), "HEAD~1..HEAD")
    out = select_with_params(state, budget_tokens=8000, tau=0.12)
    assert out.get("deleted_files") == ["doomed.py"]
    assert out.get("fragment_count") == 0
