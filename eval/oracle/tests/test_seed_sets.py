import json
import re
import tomllib

import pytest

import seeds
from corpus import LANGUAGES, ROOT, load_corpus

EARLIER_SEEDS = {20261031, 20261112}  # the seeds corpus.toml's header names as spent or read
PLAN = tomllib.loads(seeds.DRAW_FILE.read_text(encoding="utf-8"))
POOLS = json.loads(seeds.POOL_FILE.read_text(encoding="utf-8"))
SETS = {entry["number"]: load_corpus(ROOT / "seeds" / f"seed-{entry['number']}.toml") for entry in PLAN["set"]}
CORPUS = load_corpus(ROOT / "corpus.toml")


def owner(url: str) -> str:
    return url.split("/")[3].lower()


def full_name(repo) -> str:
    return "/".join(repo.url.removesuffix(".git").split("/")[3:5])


def test_there_are_three_sets_each_as_large_per_language_as_corpus_toml():
    assert sorted(SETS) == [2, 3, 4]
    for loaded in SETS.values():
        for language in LANGUAGES:
            assert [r.language for r in loaded.repos].count(language) == [r.language for r in CORPUS.repos].count(language)
        assert [r.fast for r in loaded.repos].count(True) == len(LANGUAGES)


def test_no_repository_or_owner_is_shared_between_sets_or_with_corpus_toml():
    everything = [(number, repo) for number, loaded in SETS.items() for repo in loaded.repos]
    names = [full_name(repo) for _, repo in everything]

    assert len(set(names)) == len(names)
    assert len({repo.name for _, repo in everything}) == len(everything)
    corpus_owners = {owner(repo.url) for repo in CORPUS.repos}
    assert not [full_name(repo) for _, repo in everything if owner(repo.url) in corpus_owners]
    assert not {repo.name for _, repo in everything} & {repo.name for repo in CORPUS.repos}


def test_every_repository_is_pinned_permissively_licensed_and_was_feasible_in_the_frozen_pool():
    feasible = {e["full_name"]: e for entries in POOLS["languages"].values() for e in entries if e["feasible"]}
    for loaded in SETS.values():
        for repo in loaded.repos:
            assert re.fullmatch(r"[0-9a-f]{40}", repo.sha) and repo.url.startswith("https://github.com/"), repo.name
            assert set(repo.license.split(" OR ")) <= seeds.PERMISSIVE_LICENSES, repo.name
            assert feasible[full_name(repo)]["license"] == repo.license
            assert repo.name == seeds.repo_name(full_name(repo))


def test_the_set_seeds_are_distinct_recorded_with_their_date_and_never_an_earlier_seed():
    recorded = [entry["seed"] for entry in PLAN["set"]]

    assert len(set(recorded)) == len(recorded) and not {*EARLIER_SEEDS, CORPUS.sampling.seed} & set(recorded)
    assert all(re.fullmatch(r"\d{4}-\d{2}-\d{2}", entry["drawn"]) for entry in PLAN["set"])
    assert {n: loaded.sampling.seed for n, loaded in SETS.items()} == {e["number"]: e["seed"] for e in PLAN["set"]}
    assert {n: loaded.holdout for n, loaded in SETS.items()} == {2: False, 3: True, 4: True}
    assert all(loaded.spent == {} and loaded.tools == CORPUS.tools for loaded in SETS.values())


def test_drawing_again_from_the_frozen_pool_and_seeds_gives_the_recorded_sets():
    pins = {repo.url: repo.sha for loaded in SETS.values() for repo in loaded.repos}

    again = seeds.draw(POOLS, PLAN, pins.__getitem__, "2026-11-27")

    for path, text in again.items():
        drawn = tomllib.loads(text)
        recorded = tomllib.loads(path.read_text(encoding="utf-8"))
        assert drawn["repo"] == recorded["repo"] and drawn["sampling"] == recorded["sampling"], path.name


def test_the_pool_is_the_documented_size_ranked_by_stars_and_free_of_corpus_owners():
    assert POOLS["pool_size"] == seeds.POOL_SIZE and POOLS["pool_limit"] == seeds.POOL_LIMIT
    corpus_owners = {owner(repo.url) for repo in CORPUS.repos}
    for language, entries in POOLS["languages"].items():
        assert seeds.POOL_SIZE <= len(entries) <= seeds.POOL_LIMIT, language
        assert [e["stars"] for e in entries] == sorted((e["stars"] for e in entries), reverse=True), language
        assert not [e["full_name"] for e in entries if owner(e["url"]) in corpus_owners], language
        assert all(seeds.permissive(e["license"]) and e["feasible"] == (e["reason"] == "") for e in entries), language


def test_a_set_that_cannot_be_drawn_is_refused_before_a_file_is_written():
    shrunk = {**POOLS, "languages": {**POOLS["languages"], "kotlin": POOLS["languages"]["kotlin"][:0]}}

    with pytest.raises(SystemExit, match="kotlin"):
        seeds.draw(shrunk, PLAN, lambda url: "0" * 40, "2026-11-27")
