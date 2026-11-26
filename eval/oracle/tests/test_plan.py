import dataclasses
import json

import pytest

import plan
from corpus import ROOT, load_corpus


def keys(corpus) -> dict[str, str]:
    return {repo.name: plan.index_key(corpus, repo) for repo in corpus.repos}


def test_the_index_cache_key_names_the_inputs_and_never_the_run(monkeypatch: pytest.MonkeyPatch):
    corpus = load_corpus()
    monkeypatch.setenv("GITHUB_RUN_ID", "1")
    monkeypatch.setenv("GITHUB_RUN_ATTEMPT", "1")
    first = keys(corpus)
    monkeypatch.setenv("GITHUB_RUN_ID", "2")
    monkeypatch.setenv("GITHUB_RUN_ATTEMPT", "3")

    assert keys(corpus) == first
    assert len(set(first.values())) == len(first)
    assert all(key.startswith(f"{plan.CACHE_NAMESPACE}-{name}-") for name, key in first.items())


def test_the_key_moves_with_each_input_that_decides_which_indexes_exist_and_how_they_read():
    corpus = load_corpus()
    repo = next(r for r in corpus.repos if r.language == "kotlin")
    base = plan.index_key(corpus, repo)

    def moved(other_corpus=corpus, other_repo=repo) -> bool:
        return plan.index_key(other_corpus, other_repo) != base

    assert moved(other_repo=dataclasses.replace(repo, sha="0" * 40))
    assert moved(dataclasses.replace(corpus, sampling=dataclasses.replace(corpus.sampling, seed=1)))
    assert moved(dataclasses.replace(corpus, sampling=dataclasses.replace(corpus.sampling, history=1)))
    assert moved(dataclasses.replace(corpus, spent={repo.name: frozenset({"a" * 40})}))
    assert moved(dataclasses.replace(corpus, min_file_ratio=0.5))
    tools = {**corpus.tools, "scip-java": dataclasses.replace(corpus.tools["scip-java"], version="9.9.9")}
    assert moved(dataclasses.replace(corpus, tools=tools))
    other = next(r for r in corpus.repos if r.language == "python")
    assert plan.index_key(corpus, other) == plan.index_key(
        dataclasses.replace(corpus, spent={**corpus.spent, "unrelated": frozenset({"b" * 40})}), other
    )


def test_a_toolchain_pin_moves_only_a_jvm_key(monkeypatch: pytest.MonkeyPatch):
    corpus = load_corpus()
    before = keys(corpus)
    monkeypatch.setattr(plan, "jvm_toolchain_digest", lambda: "another")
    after = keys(corpus)

    assert {name for name in before if before[name] != after[name]} == {
        r.name for r in corpus.repos if r.language in ("kotlin", "java")
    }


def test_the_matrix_names_each_corpus_repository_once_with_its_key_and_the_legacy_prefix_only_for_corpus_toml():
    corpus = load_corpus()
    entries = plan.plan(corpus, "full")["include"]

    assert [e["repo"] for e in entries] == [r.name for r in corpus.repos]
    assert all(e["corpus"] == "corpus.toml" and e["legacy_prefix"] == f"oracle-index-{e['repo']}-" for e in entries)
    seeded = plan.plan(corpus, "full", "seeds/seed-2.toml")["include"]
    assert all(e["corpus"] == "seeds/seed-2.toml" and e["legacy_prefix"] == "" for e in seeded)


SEED_FILE = """extends = "../corpus.toml"
{holdout}
[sampling]
seed = 5

[[repo]]
name = "o--r"
url = "https://github.com/o/r.git"
language = "python"
license = "MIT"
sha = "{sha}"
"""


@pytest.fixture
def seed_file():
    written = []

    def write(name: str, holdout: bool) -> str:
        path = ROOT / "seeds" / name
        path.parent.mkdir(exist_ok=True)
        path.write_text(SEED_FILE.format(holdout="holdout = true\n" * holdout, sha="1" * 40), encoding="utf-8")
        written.append(path)
        return f"seeds/{name}"

    yield write
    for path in written:
        path.unlink()


def test_several_corpora_are_planned_only_to_index(seed_file):
    working = seed_file("plan-test-working.toml", holdout=False)

    with pytest.raises(SystemExit, match="one corpus"):
        plan.round_plan(["corpus.toml", working], "full")
    indexing = plan.round_plan(["corpus.toml", working], "full", index_only=True)

    assert indexing["corpus"] == "" and indexing["index_only"] and not indexing["default_corpus"]
    assert {e["corpus"] for e in indexing["matrix"]["include"]} == {"corpus.toml", working}
    assert not plan.round_plan([working], "full")["blind"]
    assert plan.round_plan([working], "full", blind=True)["blind"]


def test_a_holdout_is_blind_whatever_is_asked_and_corpus_toml_is_not(seed_file):
    holdout = seed_file("plan-test-holdout.toml", holdout=True)

    assert plan.round_plan([holdout], "full")["blind"]
    assert plan.round_plan([holdout], "full", index_only=True)["blind"]
    assert not plan.round_plan(["corpus.toml"], "full")["blind"]
    assert plan.round_plan(["corpus.toml"], "full")["default_corpus"]


def test_the_command_prints_the_matrix_and_the_mode_of_the_round(capsys, seed_file):
    working = seed_file("plan-test-cli.toml", holdout=True)

    assert plan.main(["--scope", "fast"]) == 0
    default = json.loads(capsys.readouterr().out)
    assert plan.main(["--scope", "full", "--corpus", working, "--index-only"]) == 0
    seeded = json.loads(capsys.readouterr().out)

    assert [e["repo"] for e in default["matrix"]["include"]] == [r.name for r in load_corpus().select(fast=True)]
    assert default["default_corpus"] and not default["blind"] and not default["index_only"] and default["corpus"] == "corpus.toml"
    assert seeded["blind"] and seeded["index_only"] and seeded["corpus"] == working and not seeded["default_corpus"]
