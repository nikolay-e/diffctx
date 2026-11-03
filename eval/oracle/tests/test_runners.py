import json
import tarfile
from pathlib import Path

import pytest

import runners
from corpus import Tool, load_corpus
from indexing import EXCLUDED_PARTS, scrubbed_env
from toolbin import _place


def context(tmp_path: Path, language: str, name: str = "x") -> runners.Context:
    corpus = load_corpus()
    repo = next(r for r in corpus.repos if r.language == language)
    return runners.Context(
        corpus, repo, "a" * 40, tmp_path / name, tmp_path / "index.scip", tmp_path, scrubbed_env(tmp_path), True, 5
    )


def test_the_environment_handed_to_indexers_carries_no_tokens_and_two_distinct_npm_configs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv("GITHUB_TOKEN", "secret")
    monkeypatch.setenv("NPM_TOKEN", "secret")

    env = scrubbed_env(tmp_path)

    assert not any("TOKEN" in key or key.startswith("AWS_") for key in env)
    assert env["npm_config_ignore_scripts"] == "true"
    assert env["npm_config_userconfig"] != env["npm_config_globalconfig"]
    assert env["HOME"] == str(tmp_path)


def test_build_output_directories_are_not_code_files_of_the_snapshot():
    assert {"target", "node_modules", "build", "vendor"} <= set(EXCLUDED_PARTS)


def test_release_archive_is_unpacked_to_the_binary(tmp_path: Path):
    tool = Tool("scip", "1", "https://x/{version}", {"any": "0" * 64})
    packed = tmp_path / "in"
    packed.mkdir()
    (packed / "scip").write_bytes(b"binary")
    with tarfile.open(tmp_path / "a.tar.gz", "w:gz") as archive:
        archive.add(packed / "scip", arcname="release/scip")
    target = tmp_path / "out" / "scip"
    target.parent.mkdir()

    _place(tool, tmp_path / "a.tar.gz", tmp_path / "scratch", target)

    assert target.read_bytes() == b"binary"


def test_a_missing_tsconfig_is_replaced_by_one_that_includes_javascript(tmp_path: Path):
    runners.cover_everything(tmp_path)

    written = json.loads((tmp_path / "tsconfig.json").read_text())
    assert written["compilerOptions"]["allowJs"] is True and written["include"] == ["**/*"] and "extends" not in written
    assert written["compilerOptions"]["moduleResolution"] == "bundler"


def test_a_narrow_tsconfig_is_kept_beside_the_oracles_and_extended(tmp_path: Path):
    (tmp_path / "tsconfig.json").write_text('{"compilerOptions": {"jsx": "react"}, "include": ["src"]}')

    runners.cover_everything(tmp_path)
    runners.cover_everything(tmp_path)  # a second run leaves the first one's files alone

    assert json.loads((tmp_path / "tsconfig.repo.json").read_text())["include"] == ["src"]
    written = json.loads((tmp_path / "tsconfig.json").read_text())
    assert written["extends"] == "./tsconfig.repo.json" and written["include"] == ["**/*"]
    assert "moduleResolution" not in written["compilerOptions"]


def test_a_solution_style_tsconfig_is_not_extended(tmp_path: Path):
    (tmp_path / "tsconfig.json").write_text('{"files": [], "references": [{"path": "./tsconfig.build.json"}]}')

    runners.cover_everything(tmp_path)

    written = json.loads((tmp_path / "tsconfig.json").read_text())
    assert "extends" not in written and written["compilerOptions"]["moduleResolution"] == "bundler"
    assert (tmp_path / "tsconfig.repo.json").exists()


def test_python_is_indexed_under_a_pyright_config_that_includes_every_file(tmp_path: Path):
    ctx = context(tmp_path, "python")
    ctx.tree.mkdir()
    (ctx.tree / "pyproject.toml").write_text('[tool.pyright]\ninclude = ["src/click"]\n')

    plan = runners.plan_python(ctx)

    assert json.loads((ctx.tree / "pyrightconfig.json").read_text()) == {"include": ["."]}
    assert plan.command is not None and "--environment=" + str(tmp_path / "environment.json") in plan.command
