"""CfgNet-500 scoring: pins, the digest gate, candidate judging over a real project graph.

The labels live in a repository with no licence and are not in this tree, so the
offline tests build their own CSV and project. The end-to-end run over the real
smallest project (opt-in: `XLANG_EVAL_NETWORK=1`) fetches the labels at their
pinned commit and clones the project at its pinned version.
"""

from __future__ import annotations

import copy
import csv
import os
import re
import subprocess
from pathlib import Path

import pytest

from eval.workflows import cfgnet500 as cn

NETWORK = os.environ.get("XLANG_EVAL_NETWORK") == "1"

HEADER = [
    "dependency_category", "index", "link_str", "project", "option_name", "option_value", "option_type", "option_file",
    "option_technology", "dependent_option_name", "dependent_option_value", "dependent_option_type", "dependent_option_file",
    "dependent_option_technology", "config_type", "rating", "final_rating", "final_category", "sub_category", "explanation",
]  # fmt: skip


def _row(
    index: int, file_a: str, file_b: str, label: str, tech_a: str = "spring", tech_b: str = "docker-compose"
) -> dict[str, str]:
    row = dict.fromkeys(HEADER, "")
    row.update(
        index=str(index), project="demo", option_file=file_a, dependent_option_file=file_b,
        option_technology=tech_a, dependent_option_technology=tech_b, final_rating=label,
    )  # fmt: skip
    return row


def _write_csv(path: Path, rows: list[dict[str, str]]) -> None:
    with path.open("w", newline="", encoding="utf-8") as f:
        writer = csv.DictWriter(f, fieldnames=HEADER)
        writer.writeheader()
        writer.writerows(rows)


def _demo_pins(rows: int, true_rows: int) -> dict:
    pins = copy.deepcopy(cn.load_pins())
    pins["labels_source"].update(rows=rows, true_rows=true_rows)
    pins["projects"] = {"demo": {"repo": "example/demo", "sha": "abcdef0"}}
    return pins


def test_pins_name_ten_projects_with_short_versions() -> None:
    pins = cn.load_pins()
    assert len(pins["projects"]) == 10
    for spec in pins["projects"].values():
        assert re.fullmatch(r"[0-9a-f]{7}", spec["sha"])
        assert re.fullmatch(r"[\w.-]+/[\w.-]+", spec["repo"])
    source = pins["labels_source"]
    assert re.fullmatch(r"[0-9a-f]{40}", source["commit"])
    assert (source["rows"], source["true_rows"] + source["false_rows"]) == (500, 500)


def test_a_label_file_with_the_wrong_digest_is_refused(tmp_path: Path) -> None:
    (tmp_path / "all_dependencies.csv").write_text("not the pinned file\n")
    with pytest.raises(RuntimeError, match="differs from the pinned"):
        cn.fetch_csv(tmp_path, cn.load_pins())


def test_candidates_are_read_against_the_pinned_counts(tmp_path: Path) -> None:
    csv_path = tmp_path / "labels.csv"
    _write_csv(
        csv_path, [_row(0, "a.yml", "b.yml", "TRUE"), _row(1, "a.yml", "c.yml", "FALSE"), _row(2, "c.yml", "c.yml", "false")]
    )
    candidates = cn.load_candidates(csv_path, _demo_pins(3, 1))
    assert [c.label for c in candidates] == [True, False, False]
    assert candidates[0].tech_pair == "docker-compose+spring"
    with pytest.raises(RuntimeError, match="pinned"):
        cn.load_candidates(csv_path, _demo_pins(3, 2))


def _git(repo: Path, *args: str) -> None:
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", *args],
        cwd=repo, check=True, capture_output=True,
    )  # fmt: skip


def test_judging_agrees_with_the_real_project_graph(tmp_path: Path) -> None:
    repo = tmp_path / "demo"
    (repo / "svc").mkdir(parents=True)
    (repo / "ops").mkdir()
    (repo / "svc/application.yml").write_text("ledger:\n  retry:\n    window: 30\n")
    (repo / "ops/docker-compose.yml").write_text(
        "services:\n  ledger:\n    image: ledger:1\n    environment:\n      - LEDGER_RETRY_WINDOW=30\n"
    )
    (repo / "ops/cache.yml").write_text("services:\n  cache:\n    image: redis:7\n")
    _git(repo, "init", "-q", "-b", "main")
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "demo")

    nodes, links = cn.file_links(repo)
    assert {"svc/application.yml", "ops/docker-compose.yml", "ops/cache.yml"} <= nodes
    assert all(len(pair) == 2 and pair <= nodes for pair in links)

    csv_path = tmp_path / "labels.csv"
    _write_csv(
        csv_path,
        [
            _row(0, "svc/application.yml", "ops/docker-compose.yml", "TRUE"),
            _row(1, "ops/cache.yml", "ops/docker-compose.yml", "FALSE"),
            _row(2, "ops/cache.yml", "ops/cache.yml", "TRUE"),
            _row(3, "ops/cache.yml", "ops/absent.yml", "FALSE"),
        ],
    )
    rows = cn.judge(cn.load_candidates(csv_path, _demo_pins(4, 2)), nodes, links)
    assert [r["status"] for r in rows] == ["decided", "decided", "same_file", "file_not_in_graph"]
    for r in rows[:2]:
        pair = frozenset(r["files"])
        assert r["link_categories"] == sorted(links.get(pair, set()))
        assert r["predicted"] == bool(set(r["link_categories"]) & cn.PREDICT_CATEGORIES)
        assert r["linked_any_category"] == bool(r["link_categories"])
    assert "option_value" not in rows[0]  # rows carry paths, technologies and the label, nothing from the label file's options

    summary = cn.summarize(rows)
    assert summary["status"] == {"decided": 2, "file_not_in_graph": 1, "same_file": 1}
    assert summary["same_file_true"] == 1
    assert summary["overall"]["candidates"] == 4 and summary["overall"]["decided"] == 2


def test_summary_arithmetic_on_a_known_table() -> None:
    def row(label: bool, predicted: bool, status: str = "decided") -> dict:
        return {
            "status": status,
            "label": label,
            "predicted": predicted,
            "linked_any_category": predicted,
            "tech_pair": "maven",
            "project": "p",
        }

    rows = [
        row(True, True),
        row(True, True),
        row(False, True),
        row(True, False),
        row(False, False),
        row(True, False, "same_file"),
    ]
    overall = cn.summarize(rows)["overall"]
    assert (overall["tp"], overall["fp"], overall["decided"]) == (2, 1, 5)
    assert overall["precision"] == pytest.approx(2 / 3, abs=1e-4)
    assert overall["recall_of_all_true"] == pytest.approx(2 / 4, abs=1e-4)
    assert overall["recall_of_decided_true"] == pytest.approx(2 / 3, abs=1e-4)
    assert overall["coverage"] == pytest.approx(5 / 6, abs=1e-4)


def test_wilson_interval_brackets_the_proportion() -> None:
    lo, hi = cn.wilson(17, 20)
    assert lo < 17 / 20 < hi
    assert cn.wilson(0, 0) is None


@pytest.mark.skipif(not NETWORK, reason="opt-in: fetches the pinned labels and clones the smallest project")
def test_the_smallest_real_project_end_to_end(tmp_path: Path) -> None:
    pins = cn.load_pins()
    candidates = cn.load_candidates(cn.fetch_csv(tmp_path / "labels", pins), pins)
    assert len(candidates) == 500 and sum(c.label for c in candidates) == 199
    mine = [c for c in candidates if c.project == "piggymetrics"]
    assert len(mine) == 50
    repo = cn.checkout_project(tmp_path / "repos", "piggymetrics", pins)
    nodes, links = cn.file_links(repo)
    rows = cn.judge(mine, nodes, links)
    summary = cn.summarize(rows)
    assert summary["overall"]["candidates"] == 50
    assert summary["status"].get("decided", 0) > 0
