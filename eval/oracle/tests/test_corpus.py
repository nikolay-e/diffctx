import json
import re

import corpus
from corpus import ROOT, load_corpus


def test_every_repository_is_pinned_to_a_full_commit_and_a_permissive_licence():
    loaded = load_corpus()

    assert {r.language for r in loaded.repos} == set(corpus.LANGUAGES)
    for language in corpus.LANGUAGES:
        assert 2 <= len([r for r in loaded.repos if r.language == language]) <= 3
        assert len([r for r in loaded.repos if r.language == language and r.fast]) == 1
    for repo in loaded.repos:
        assert re.fullmatch(r"[0-9a-f]{40}", repo.sha), repo.name
        assert repo.url.startswith("https://"), repo.name
        assert set(repo.license.split(" OR ")) <= {"MIT", "BSD-3-Clause", "Apache-2.0", "BSD-2-Clause", "Unlicense"}, repo.name


def test_every_release_binary_has_a_checksum_for_each_build_it_names():
    loaded = load_corpus()

    assert set(loaded.tools) == {"scip", "rust-analyzer", "scip-go", "scip-java"}
    for tool in loaded.tools.values():
        assert tool.url.startswith("https://") and "{version}" in tool.url
        assert tool.sha256 and all(re.fullmatch(r"[0-9a-f]{64}", digest) for digest in tool.sha256.values()), tool.name
        assert tool.archive in {"tar.gz", "gz", "file"} and tool.platforms in {"os-arch", "triple", "any"}
        assert ("{platform}" in tool.url) == (tool.platforms != "any")
    assert set(loaded.tools["scip"].sha256) == {"darwin-amd64", "darwin-arm64", "linux-amd64", "linux-arm64"}
    assert set(loaded.tools["rust-analyzer"].sha256) == set(corpus.TRIPLES.values())
    assert set(corpus.INDEXER_TOOLS.values()) <= set(loaded.tools)


def test_the_index_cache_is_keyed_by_the_exact_indexer_version():
    loaded = load_corpus()

    assert corpus.indexer_pin(loaded, "python") == ("scip-python", corpus.indexer_versions()["@sourcegraph/scip-python"])
    assert corpus.indexer_pin(loaded, "rust") == ("rust-analyzer", loaded.tools["rust-analyzer"].version)
    assert corpus.indexer_pin(loaded, "go")[0] == "scip-go" and corpus.indexer_pin(loaded, "kotlin")[0] == "scip-java"
    assert (
        loaded.tools["rust-analyzer"]
        .download_url()
        .endswith("rust-analyzer-" + loaded.tools["rust-analyzer"].build_name() + ".gz")
    )


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
