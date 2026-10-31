import json
import re

import corpus
from corpus import ROOT, load_corpus


def test_every_repository_is_pinned_to_a_full_commit_and_a_permissive_licence():
    loaded = load_corpus()

    assert {r.language for r in loaded.repos} == {"python", "typescript"}
    for language in ("python", "typescript"):
        assert len([r for r in loaded.repos if r.language == language]) == 3
        assert len([r for r in loaded.repos if r.language == language and r.fast]) == 1
    for repo in loaded.repos:
        assert re.fullmatch(r"[0-9a-f]{40}", repo.sha), repo.name
        assert repo.url.startswith("https://"), repo.name
        assert repo.license in {"MIT", "BSD-3-Clause", "Apache-2.0", "BSD-2-Clause"}, repo.name
    assert set(loaded.slots) == {"rust", "kotlin", "go"}


def test_the_scip_cli_has_a_checksum_for_every_platform():
    loaded = load_corpus()

    assert set(loaded.scip_sha256) == {"darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64"}
    assert all(re.fullmatch(r"[0-9a-f]{64}", digest) for digest in loaded.scip_sha256.values())
    assert "{platform}" in loaded.scip_url and "{version}" in loaded.scip_url


def test_indexers_are_pinned_exactly_and_the_lock_agrees():
    versions = corpus.indexer_versions()
    lock = json.loads((ROOT / "tools" / "package-lock.json").read_text())["packages"]

    assert set(versions) == set(corpus.INDEXERS.values())
    for name, version in versions.items():
        assert re.fullmatch(r"\d+\.\d+\.\d+", version), name
        assert lock[f"node_modules/{name}"]["version"] == version
        assert lock[f"node_modules/{name}"]["integrity"].startswith("sha512-")
    assert not any(entry.get("hasInstallScript") for entry in lock.values())


def test_the_fast_subset_is_a_prefix_of_the_full_draw():
    loaded = load_corpus()

    assert loaded.sampling.fast_commits < loaded.sampling.commits
    assert [r.name for r in loaded.select(fast=True)] == [r.name for r in loaded.repos if r.fast]
