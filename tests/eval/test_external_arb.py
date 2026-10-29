"""Agent Retrieval Bench edit2ripple, end to end against real git and the real engine.

The seeded repository is built here, so the cases run offline: the "upstream" is
a local git repository the adapter clones exactly as it clones GitHub, the anchor
diff is a real `git diff` of the change, and ranking is the shipped engine. What
cannot run offline — the pinned release and the benchmark's own evaluator — has
its own case, enabled by pointing two variables at a fetched release:

    ARB_E2R_DATA=<dir>/data  ARB_EVALUATOR_PYTHON=<venv>/bin/python pytest tests/eval/test_external_arb.py
"""

from __future__ import annotations

import functools
import json
import operator
import os
import subprocess
import sys
from collections import Counter
from dataclasses import asdict
from pathlib import Path

import pytest
from hypothesis import given
from hypothesis import strategies as st

from eval.external.arb_edit2ripple import baseline_inputs, rank_instance, ranking_from_locate
from eval.external.contextbench_nontrivial import RESULTS_NAME, merge
from eval.external.sharding import assign_repos, parse_shard, shard_items
from eval.harness.adapters.arb_edit2ripple import ArbEdit2RippleAdapter, repair_anchor_diff, seed_patch
from eval.harness.adapters.base import EvalResult

REPO = "acme/widget"
ANCHOR = "widget/pricing.py"
GOLD = "tests/test_pricing.py"


def _git(root: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "-C", str(root), *args],
        check=True,
        capture_output=True,
        text=True,
    )
    return done.stdout.strip()


def _write(root: Path, rel: str, text: str) -> None:
    target = root / rel
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")


@pytest.fixture
def upstream(tmp_path: Path) -> dict:
    """A small project with a base commit and a fix commit touching the anchor.

    `tests/test_pricing.py` exercises `price_with_tax`; `widget/shipping.py` and
    `docs/notes.md` do not mention it.
    """
    root = tmp_path / "upstream" / REPO
    root.mkdir(parents=True)
    _git(root, "init", "-q", "-b", "main")
    _write(root, ANCHOR, "TAX = 0.2\n\n\ndef price_with_tax(amount):\n    return amount * (1 + TAX)\n")
    _write(
        root,
        GOLD,
        "from widget.pricing import price_with_tax\n\n\ndef test_tax():\n    assert price_with_tax(100) == 120\n",
    )
    _write(root, "widget/shipping.py", "def shipping_cost(weight):\n    return 5 + weight * 2\n")
    _write(root, "docs/notes.md", "# Notes\n\nNothing about prices here.\n")
    _git(root, "add", ".")
    _git(root, "commit", "-q", "-m", "base")
    base = _git(root, "rev-parse", "HEAD")
    _write(root, ANCHOR, "TAX = 0.25\n\n\ndef price_with_tax(amount):\n    return round(amount * (1 + TAX), 2)\n")
    full_diff = _git(root, "diff", "--no-color", "HEAD", "--", ANCHOR)
    hunks = full_diff[full_diff.index("@@") :] + "\n"
    return {"template": str(tmp_path / "upstream") + "/{repo}", "base": base, "hunks": hunks, "tmp": tmp_path}


def _sample_row(upstream: dict) -> dict:
    return {
        "id": "deadbeef0001",
        "repo": REPO,
        "base_commit": upstream["base"],
        "task_type": "edit2ripple",
        "query": {"anchor_file": ANCHOR, "anchor_diff": upstream["hunks"], "intent": "raise the tax rate"},
        "gold": {"files": [GOLD], "given_files": [ANCHOR]},
    }


def _instance(upstream: dict, *, gold_files: list[str] | None = None):
    row = _sample_row(upstream)
    if gold_files is not None:
        row["gold"]["files"] = gold_files
    samples = upstream["tmp"] / "samples.jsonl"
    samples.write_text(json.dumps(row) + "\n", encoding="utf-8")
    return list(ArbEdit2RippleAdapter(samples, upstream["template"]).load())


class TestAdapter:
    def test_gold_is_the_ripple_and_the_seed_is_the_anchor_alone(self, upstream):
        (inst,) = _instance(upstream)
        assert inst.gold_files == {GOLD}
        assert inst.gold_patch.startswith(f"diff --git a/{ANCHOR} b/{ANCHOR}\n--- a/{ANCHOR}\n+++ b/{ANCHOR}\n@@")
        assert inst.extra["seeding"] == "diff-only"
        assert inst.problem_statement is None, "the intent text is not part of the diff-only input"

    def test_a_sample_whose_only_gold_is_the_anchor_is_named_not_silently_dropped(self, upstream):
        assert _instance(upstream, gold_files=[ANCHOR]) == []
        adapter = ArbEdit2RippleAdapter(upstream["tmp"] / "samples.jsonl", upstream["template"])
        assert adapter.unseedable() == ["deadbeef0001"]

    def test_a_diff_the_release_trimmed_applies_only_after_repair(self, tmp_path):
        """Real git: a hunk whose last context line is blank loses it to `.strip()`, as 29 of the release's 58 do."""
        _git(tmp_path, "init", "-q", "-b", "main")
        _write(tmp_path, "a.txt", "one\ntwo\nthree\n\n")
        _git(tmp_path, "add", ".")
        _git(tmp_path, "commit", "-q", "-m", "base")
        _write(tmp_path, "a.txt", "one\nTWO\nthree\n\n")
        full = _git(tmp_path, "diff", "--no-color", "--", "a.txt")
        trimmed = full[full.index("@@") :].rstrip() + "\n"
        _git(tmp_path, "checkout", "-q", "--", "a.txt")

        def applies(patch: str) -> bool:
            (tmp_path / "p.patch").write_text(patch, encoding="utf-8")
            check = subprocess.run(["git", "-C", str(tmp_path), "apply", "--check", "p.patch"], capture_output=True, text=True)
            return check.returncode == 0

        header = "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n"
        assert not applies(header + trimmed), "the shipped shape must really be unappliable, or the repair proves nothing"
        assert applies(seed_patch("a.txt", trimmed))

    def test_a_truncated_seed_is_flagged_and_a_hunk_cut_before_its_change_is_left_out(self):
        text = "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n@@ -40,4 +40,4 @@ def f():\n x\n y\n z\n...[truncated]"
        repaired, info = repair_anchor_diff(text)
        assert info == {"truncated": True, "recounted_hunks": 0, "dropped_changeless_hunks": 1}
        assert repaired == "@@ -1,3 +1,3 @@\n a\n-b\n+B\n c\n"

    def test_a_diff_with_no_change_left_is_named_not_dropped(self, upstream):
        row = _sample_row(upstream)
        row["query"]["anchor_diff"] = "@@ -1,2 +1,2 @@\n a\n b\n...[truncated]"
        samples = upstream["tmp"] / "empty.jsonl"
        samples.write_text(json.dumps(row) + "\n", encoding="utf-8")
        adapter = ArbEdit2RippleAdapter(samples, upstream["template"])
        assert list(adapter.load()) == [] and adapter.unseedable() == ["deadbeef0001"]

    def test_the_diff_only_baseline_query_carries_the_repaired_diff_and_no_intent(self, upstream):
        root = upstream["tmp"] / "data"
        release = root / "benchmark" / "v2_edit2ripple"
        release.mkdir(parents=True)
        row = _sample_row(upstream)
        row["query"]["anchor_diff"] = "@@ -1,2 +1,2 @@\n-a\n+b\n c\n...[truncated]"
        (release / "samples.jsonl").write_text(json.dumps(row) + "\n", encoding="utf-8")
        rankings = upstream["tmp"] / "rankings.jsonl"
        rankings.write_text(json.dumps({"sample_id": row["id"], "status": "ok"}) + "\n", encoding="utf-8")
        out = upstream["tmp"] / "inputs"

        args = type("Args", (), {"data_root": root, "rankings": [rankings], "out": out})()
        assert baseline_inputs(args) == 0

        (full,) = [json.loads(line) for line in (out / "samples_full.jsonl").read_text().splitlines()]
        (diff_only,) = [json.loads(line) for line in (out / "samples_diffonly.jsonl").read_text().splitlines()]
        assert full["query"]["intent"] == "raise the tax rate"
        assert set(diff_only["query"]) == {"anchor_file", "anchor_diff"}
        assert diff_only["query"]["anchor_diff"] == "@@ -1,2 +1,2 @@\n-a\n+b\n c\n"
        assert json.loads((out / "ok_ids.json").read_text()) == [row["id"]]

    def test_the_seed_patch_ends_with_a_newline(self):
        assert seed_patch("a.py", "@@ -1 +1 @@\n-x\n+y").endswith("+y\n")


class TestRankingAgainstTheRealEngine:
    def test_the_file_that_names_the_changed_symbol_is_ranked_and_the_anchor_is_not(self, upstream, monkeypatch):
        monkeypatch.setenv("CB_REPOS_DIR", str(upstream["tmp"] / "cache"))
        worktrees = upstream["tmp"] / "cache" / "worktrees"
        worktrees.mkdir(parents=True)
        (inst,) = _instance(upstream)

        row = rank_instance(inst, worktrees, budget=8000, timeout=120)

        assert row["status"] == "ok", row.get("error", row)
        paths = [p for p, _ in row["ranked"]]
        assert ANCHOR not in paths, "the anchor is the seed, never an answer"
        assert GOLD in paths
        assert GOLD in row["selected_files"]
        assert "widget/shipping.py" not in paths or paths.index(GOLD) < paths.index("widget/shipping.py")
        assert row["engine_version"] and row["effective_config_hash"]

    def test_an_anchor_diff_that_does_not_apply_is_reported_not_ranked(self, upstream, monkeypatch):
        monkeypatch.setenv("CB_REPOS_DIR", str(upstream["tmp"] / "cache"))
        worktrees = upstream["tmp"] / "cache" / "worktrees"
        worktrees.mkdir(parents=True)
        row = _sample_row(upstream)
        row["query"]["anchor_diff"] = "@@ -1,2 +1,2 @@\n-NOT THE FILE\n+STILL NOT\n"
        samples = upstream["tmp"] / "bad.jsonl"
        samples.write_text(json.dumps(row) + "\n", encoding="utf-8")
        (inst,) = ArbEdit2RippleAdapter(samples, upstream["template"]).load()

        assert rank_instance(inst, worktrees, budget=8000, timeout=120)["status"] == "apply_fail"

    def test_ranking_orders_by_best_fragment_score_across_selection_and_overflow(self):
        doc = {
            "items": [{"path": "a.py", "score": 1.0}, {"path": "b.py", "score": 0.2}],
            "overflow": [{"path": "b.py", "score": 0.5}, {"path": "c.py", "score": 0.3}, {"path": "a.py", "score": 0.9}],
            "provenance": {"engine": {"version": "9.9"}, "effective_config_hash": "h", "selection": {"tau": 0.05}},
        }
        ranking = ranking_from_locate(doc, anchor_file="a.py")
        assert ranking["ranked"] == [["b.py", 0.5], ["c.py", 0.3]]
        assert ranking["selected_files"] == ["b.py"]


class TestSharding:
    @given(
        counts=st.dictionaries(st.text("abc", min_size=1, max_size=3), st.integers(1, 40), min_size=1, max_size=12),
        shards=st.integers(1, 7),
    )
    def test_shards_partition_the_items_and_never_split_a_repository(self, counts, shards):
        items = [(repo, n) for repo, count in counts.items() for n in range(count)]
        taken = [shard_items(items, lambda i: i[0], shards, k) for k in range(shards)]
        assert sorted(functools.reduce(operator.iadd, taken, [])) == sorted(items)
        owners = Counter(repo for part in taken for repo in {i[0] for i in part})
        assert set(owners.values()) <= {1}

    def test_the_split_is_deterministic(self):
        counts = Counter({"x/a": 9, "x/b": 9, "x/c": 3, "x/d": 3})
        assert assign_repos(counts, 3) == assign_repos(Counter(dict(reversed(list(counts.items())))), 3)

    @pytest.mark.parametrize("spec", ["", "3", "2/2", "-1/3", "a/b"])
    def test_a_malformed_shard_is_refused(self, spec):
        with pytest.raises(ValueError):
            parse_shard(spec)


class TestContextBenchNontrivialMerge:
    @staticmethod
    def _row(i: int, status: str, recall: float, nontrivial: float | None) -> dict:
        extra: dict = {"status": status, "engine_version": "1.20.0", "effective_config_hash": "h"}
        if nontrivial is not None:
            extra["nontrivial_file_recall"] = nontrivial
            extra["n_nontrivial_gold"] = 1
        row = EvalResult(f"contextbench_verified::i{i}", "contextbench_verified", recall, 0.1, budget=8000, extra=extra)
        return asdict(row)

    def test_shards_merge_and_a_failed_instance_counts_in_the_zero_filled_mean_only(self, tmp_path):
        # CI's test job installs no eval group; the summary's bootstrap needs numpy.
        pytest.importorskip("numpy")
        shard0, shard1 = tmp_path / "shard-0", tmp_path / "shard-1"
        for d in (shard0, shard1):
            d.mkdir()
        (shard0 / RESULTS_NAME).write_text(json.dumps([self._row(0, "ok", 1.0, 1.0), self._row(1, "ok", 0.5, 0.0)]))
        (shard1 / RESULTS_NAME).write_text(json.dumps([self._row(2, "ok", 0.0, 0.0), self._row(3, "clone_fail", 0.0, None)]))

        summary = merge([shard0, shard1], engine_ref="abc")

        assert (summary["rows"], summary["ok"]) == (4, 3)
        assert summary["statuses"] == {"clone_fail": 1, "ok": 3}
        assert summary["file_recall_ok"]["mean"] == pytest.approx(0.5)
        assert summary["file_recall_failures_zero"]["mean"] == pytest.approx(0.375)
        assert summary["nontrivial_file_recall_ok"]["n"] == 3

    def test_a_shard_that_produced_nothing_stops_the_merge(self, tmp_path):
        (tmp_path / "shard-0").mkdir()
        with pytest.raises(SystemExit):
            merge([tmp_path / "shard-0"], engine_ref="abc")


DATA = os.environ.get("ARB_E2R_DATA")
EVALUATOR = os.environ.get("ARB_EVALUATOR_PYTHON")


@pytest.mark.skipif(not (DATA and EVALUATOR), reason="needs a fetched release and an interpreter holding the pinned evaluator")
def test_a_real_sample_scores_through_the_benchmarks_own_evaluator(tmp_path):
    """The smallest gin sample, cloned from GitHub, ranked, then scored by the pinned evaluator."""
    sample_id = "02faf838effb2bcebf8d8cca"
    out = tmp_path / "out"
    env = {**os.environ, "CB_REPOS_DIR": str(tmp_path / "repos")}
    run = subprocess.run(
        [sys.executable, "-m", "eval", "arb-edit2ripple", "run", "--data-root", DATA, "--out", str(out),
         "--repos-dir", str(tmp_path / "repos"), "--sample-id", sample_id],
        capture_output=True, text=True, env=env, check=False,
    )  # fmt: skip
    assert run.returncode == 0, run.stderr[-2000:]
    (ranking,) = [json.loads(line) for line in (out / "rankings.jsonl").read_text().splitlines()]
    assert ranking["status"] == "ok"

    scorer = Path(__file__).resolve().parents[2] / ".github" / "scripts" / "arb_score.py"
    score = subprocess.run(
        [EVALUATOR, "-I", str(scorer), "--data-root", DATA, "--rankings", str(out / "rankings.jsonl"), "--out", str(out / "score")],
        capture_output=True, text=True, check=False,
    )  # fmt: skip
    assert score.returncode == 0, score.stderr[-2000:]
    summary = json.loads((out / "score" / "summary.json").read_text())
    assert summary["ok"] == 1
    assert summary["metrics_ok"]["Recall@20"] == 1.0, "the changed test file is the sample's one gold file"
    assert summary["ranked_files_per_sample_ok"] > 0, "every ranked file was dropped as outside the corpus"


class TestExternalTiming:
    @staticmethod
    def _arb(path, times):
        path.mkdir(parents=True)
        rows = [{"sample_id": f"s{i}", "status": "ok", "engine_s": t} for i, t in enumerate(times)]
        rows.append({"sample_id": "failed", "status": "apply_fail"})
        rows.append({"sample_id": "untimed", "status": "ok"})
        (path / "rankings.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))

    def test_the_head_is_compared_with_the_base_on_the_instances_both_ran(self, tmp_path):
        from eval.external.timing import collect, compare, render

        self._arb(tmp_path / "base" / "shard-0", [1.0, 2.0, 4.0])
        self._arb(tmp_path / "head" / "shard-0", [2.0, 2.0, 8.0, 5.0])

        report = compare(collect(tmp_path / "base"), collect(tmp_path / "head"))

        arb = report["arb_edit2ripple"]
        assert (arb["base"]["n"], arb["head"]["n"]) == (3, 4), "failed and untimed rows are not timings"
        assert arb["paired"]["n"] == 3 and arb["paired"]["median_ratio"] == 2.0
        assert arb["paired"]["total_ratio"] == pytest.approx(12 / 7, abs=1e-3)
        assert arb["paired"]["slower_than_1_5x"] == 2
        assert arb["base"]["p50_s"] == 2.0 and arb["head"]["max_s"] == 8.0
        assert "arb_edit2ripple" in render(report)

    def test_rows_without_a_per_instance_time_add_no_dataset(self, tmp_path):
        from eval.external.timing import collect

        shard = tmp_path / "shard-0"
        shard.mkdir()
        (shard / "rows.json").write_text(json.dumps([{"project": "mall", "status": "decided"}]))
        assert collect(tmp_path) == {}
