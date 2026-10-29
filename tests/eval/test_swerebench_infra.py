"""SWE-rebench V2 infra slice: classification, the slice rule, sampling, and a real run.

Everything here runs offline against git repositories built in a temp dir and the
real engine. The network smoke over a real dataset instance is opt-in
(`XLANG_EVAL_NETWORK=1` plus `SWE_REBENCH_PARQUET`), because it needs the 429 MB
parquet and a clone of the instance's repository.
"""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path

import pytest

from eval.harness.adapters import swerebench_infra as sl
from eval.workflows import swerebench_infra as wf

NETWORK = os.environ.get("XLANG_EVAL_NETWORK") == "1"


def _git(repo: Path, *args: str) -> str:
    out = subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", *args],
        cwd=repo,
        check=True,
        capture_output=True,
        text=True,
    )
    return out.stdout


def _write(repo: Path, files: dict[str, str]) -> None:
    for rel, text in files.items():
        path = repo / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)


def _repo_with_fix(tmp_path: Path, before: dict[str, str], after: dict[str, str]) -> tuple[Path, str, str]:
    """A repository at `before`, and the unified diff to `after` as the dataset stores it."""
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q", "-b", "main")
    _write(repo, before)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-qm", "base")
    base = _git(repo, "rev-parse", "HEAD").strip()
    _write(repo, after)
    _git(repo, "add", "-A")
    patch = _git(repo, "diff", "--cached", "--no-color")
    _git(repo, "reset", "-q", "--hard", base)
    _git(repo, "clean", "-qfd")
    return repo, base, patch


BEFORE = {
    "app/settings.py": 'import os\nPOOL = int(os.environ.get("LEDGER_POOL_SIZE", "4"))\n',
    "app/util.py": "def twice(x):\n    return 2 * x\n",
    "deploy/docker-compose.yml": "services:\n  ledger:\n    image: ledger:1\n    environment:\n      - LEDGER_POOL_SIZE=4\n",
    ".github/workflows/ci.yml": "name: ci\non: [push]\njobs:\n  t:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo 1\n",
    "docs/guide.md": "# guide\n",
}
AFTER = {
    **BEFORE,
    "app/settings.py": 'import os\nPOOL = int(os.environ.get("LEDGER_POOL_SIZE", "8"))\n',
    "deploy/docker-compose.yml": "services:\n  ledger:\n    image: ledger:1\n    environment:\n      - LEDGER_POOL_SIZE=8\n",
    "docs/guide.md": "# guide\n\npool size is now 8\n",
    "app/new_module.py": "X = 1\n",
}


@pytest.mark.parametrize(
    ("path", "text", "expected"),
    [
        ("Dockerfile", "", "infra"),
        ("services/api/Dockerfile.prod", "", "infra"),
        ("build/app.dockerfile", "", "infra"),
        ("docker-compose.override.yml", "", "infra"),
        ("compose.yaml", "", "infra"),
        ("infra/main.tf", "", "infra"),
        ("envs/prod.tfvars", "", "infra"),
        (".github/workflows/release.yaml", "", "infra"),
        (".gitlab-ci.yml", "", "infra"),
        (".env.example", "", "infra"),
        ("charts/api/values.yaml", "", "infra"),
        ("deploy/Chart.yaml", "", "infra"),
        ("k8s/api.yaml", "", "infra"),
        ("ops/anything.yaml", "apiVersion: apps/v1\nkind: Deployment\n", "infra"),
        ("config/settings.yaml", "debug: true\n", "other"),
        ("src/app/main.py", "", "code"),
        ("cmd/server/main.go", "", "code"),
        ("src/lib.rs", "", "code"),
        ("tests/test_main.py", "", "other"),
        ("pkg/x/x_test.go", "", "other"),
        ("README.md", "", "other"),
        ("package-lock.json", "", "other"),
    ],
)
def test_classification(path: str, text: str, expected: str) -> None:
    block = sl.Block(path, path, "modified", f"diff --git a/{path} b/{path}\n+{text}")
    assert sl.classify_block(block) == expected


def test_a_fix_touching_both_sides_qualifies_and_hidden_gold_exists_at_base(tmp_path: Path) -> None:
    _, _, patch = _repo_with_fix(tmp_path, BEFORE, AFTER)
    entry = sl.qualify({"instance_id": "o__r-1", "repo": "o/r", "base_commit": "x", "language": "python", "patch": patch})
    assert entry is not None
    assert entry.code_hidden == ("app/settings.py",)  # the added module is a seed, not hidden gold
    assert entry.code_seed == ("app/new_module.py", "app/settings.py")
    assert entry.infra_hidden == ("deploy/docker-compose.yml",)
    assert entry.infra_kinds == ("compose",)


def test_a_fix_on_one_side_or_adding_only_does_not_qualify(tmp_path: Path) -> None:
    code_only = {**BEFORE, "app/util.py": "def twice(x):\n    return x + x\n"}
    _, _, patch = _repo_with_fix(tmp_path, BEFORE, code_only)
    assert sl.qualify({"instance_id": "a", "repo": "o/r", "base_commit": "x", "language": "python", "patch": patch}) is None
    added_infra = {**code_only, "deploy/Dockerfile": "FROM python:3\n"}
    other = tmp_path / "second"
    other.mkdir()
    _, _, patch = _repo_with_fix(other, BEFORE, added_infra)
    assert sl.qualify({"instance_id": "b", "repo": "o/r", "base_commit": "x", "language": "python", "patch": patch}) is None


def test_the_seed_patch_applies_one_side_only(tmp_path: Path) -> None:
    repo, _, patch = _repo_with_fix(tmp_path, BEFORE, AFTER)
    seed = sl.seed_patch(patch, "infra")
    (repo / "seed.patch").write_text(seed)
    _git(repo, "apply", "seed.patch")
    changed = set(_git(repo, "diff", "--name-only").split())
    assert changed == {"deploy/docker-compose.yml"}


def _entries(n: int) -> list[sl.SliceEntry]:
    kinds = [("ci",), ("dockerfile",), ("helm",), ("k8s",), ("compose",)]
    langs = ["python", "go", "rust", "js"]
    return [
        sl.SliceEntry(
            instance_id=f"o{i % 17}__r-{i}",
            repo=f"o{i % 17}/r",
            base_commit="c" * 40,
            language=langs[i % 4],
            code_seed=("a.py",),
            infra_seed=("Dockerfile",),
            code_hidden=("a.py",),
            infra_hidden=("Dockerfile",),
            infra_kinds=kinds[i % 5],
        )
        for i in range(400)
    ]


def test_the_sample_is_deterministic_bounded_and_spread_over_strata() -> None:
    entries = _entries(400)
    first = sl.draw_sample(entries, limit=60)
    assert first == sl.draw_sample(list(reversed(entries)), limit=60)
    assert len(first) == 60
    assert {sl.infra_class(e) for e in first} == {"ci", "dockerfile", "helm", "k8s", "compose"}
    per_repo: dict[str, int] = {}
    for e in first:
        per_repo[e.repo] = per_repo.get(e.repo, 0) + 1
    assert max(per_repo.values()) <= sl.MAX_PER_REPO
    labelled = sl.draw_label_sample(first, 10)
    assert len(labelled) == 10 and set(labelled) <= set(first)


def test_a_real_run_hides_the_other_side_and_scores_it(tmp_path: Path) -> None:
    repo, base, patch = _repo_with_fix(tmp_path, BEFORE, AFTER)
    entry = sl.qualify({"instance_id": "o__r-1", "repo": "o/r", "base_commit": base, "language": "python", "patch": patch})
    assert entry is not None
    row = entry.as_manifest_row()
    coupled = {
        "deploy/docker-compose.yml": {"coupled": True, "indirect": False},
        "app/settings.py": {"coupled": True, "indirect": False},
    }

    forward = wf.evaluate_direction(row, patch, "code_to_infra", repo, 8000, coupled)
    assert forward["status"] == "ok"
    assert forward["hidden"] == ["deploy/docker-compose.yml"]
    assert forward["n_seed_files"] == 2
    assert forward["recall"] == 1.0  # a five-file repository: the compose file that sets the env var is selected
    assert forward["coupled_recall"] == 1.0

    backward = wf.evaluate_direction(row, patch, "infra_to_code", repo, 8000, coupled)
    assert backward["status"] == "ok"
    assert backward["hidden"] == ["app/settings.py"]
    assert backward["n_seed_files"] == 1

    # the worktree is back at the base commit, so the next direction starts clean
    assert _git(repo, "rev-parse", "HEAD").strip() == base
    assert _git(repo, "status", "--porcelain").strip() == ""


def test_the_ranked_list_orders_by_relevance_and_drops_seeds(tmp_path: Path) -> None:
    dump = tmp_path / "dump.jsonl"
    root = tmp_path / "repo"
    root.mkdir()
    rows = [
        {"path": str(root.resolve() / "seed.py"), "relevance": 1.0},
        {"path": str(root.resolve() / "b.py"), "relevance": 0.2},
        {"path": str(root.resolve() / "a.py"), "relevance": 0.2},
        {"path": str(root.resolve() / "b.py"), "relevance": 0.5},
    ]
    dump.write_text("\n".join(json.dumps(r) for r in rows))
    assert wf._ranked_files(dump, root, {"seed.py"}) == ["b.py", "a.py"]


def test_the_committed_manifest_is_internally_consistent() -> None:
    manifest = wf.load_manifest()
    assert manifest["revision"] == sl.DATASET_REVISION
    rows = {r["instance_id"]: r for r in manifest["instances"]}
    assert len(rows) == manifest["sample"]["total"] <= sl.MAX_SAMPLE
    assert all(r["code_files"] and r["infra_files"] for r in rows.values())
    exclusions = manifest["publication_exclusions"]
    assert exclusions["instance_ids"] and not set(exclusions["instance_ids"]) & set(rows)
    labelled = manifest["labels"]["instances"]
    assert set(labelled) == set(manifest["label_sample_ids"]) <= set(rows)
    assert len(labelled) == sl.LABEL_SAMPLE
    for iid, items in labelled.items():
        assert {x["path"] for x in items} == set(rows[iid]["code_files"]) | set(rows[iid]["infra_files"])
        assert all(x["evidence"] for x in items)
    # licence: ids, paths and our own wording only, never patch text
    assert not any("patch" in key for r in rows.values() for key in r)
    assert "diff --git" not in json.dumps(manifest)


@pytest.mark.skipif(not NETWORK or "SWE_REBENCH_PARQUET" not in os.environ, reason="opt-in: needs the pinned parquet and GitHub")
def test_the_rule_reproduces_the_committed_manifest() -> None:
    assert wf.verify(wf.load_manifest(), Path(os.environ["SWE_REBENCH_PARQUET"])) == []


@pytest.mark.skipif(not NETWORK or "SWE_REBENCH_PARQUET" not in os.environ, reason="opt-in: needs the pinned parquet and GitHub")
def test_a_real_dataset_instance_end_to_end(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("SWE_REBENCH_REPOS_DIR", str(tmp_path / "repos"))
    manifest = wf.load_manifest()
    row = next(r for r in manifest["instances"] if r["instance_id"] == "whatyouhide__redix-125")
    parquet = Path(os.environ["SWE_REBENCH_PARQUET"])
    patch = wf.read_patches(parquet, {row["instance_id"]})[row["instance_id"]]
    labels = wf._labels_by_path(manifest)
    results = wf._run_one((1, row, patch, 8000, labels[row["instance_id"]], wf.repos_dir("SWE_REBENCH_REPOS_DIR")))
    assert [r["direction"] for r in results] == ["code_to_infra", "infra_to_code"]
    assert all(r["status"] == "ok" for r in results)
    assert {r["hidden"][0] for r in results} == {"docker-compose.yml", "lib/redix/uri.ex"}
