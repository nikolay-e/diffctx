from __future__ import annotations

import diffctx
from diffctx._diffctx import count_tokens
from tests.framework.pygit2_backend import Pygit2Repo

# `--budget` is documented as a cap on the artifact, and until #241 it bounded
# only the fragments: the changed-file list rendered for free and was printed a
# second time as a "not represented" footer, so a wide range produced 3.15x its
# budget with the selection dutifully under it. The list below is wide on
# purpose — the envelope, not the code, is what used to escape.
FILE_COUNT = 60


def _wide_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    for i in range(FILE_COUNT):
        repo.add_file(f"src/package_{i}/module_with_a_long_name_{i}.py", f"def fn_{i}(x):\n    return x + {i}\n")
    repo.commit("initial")
    for i in range(FILE_COUNT):
        repo.add_file(
            f"src/package_{i}/module_with_a_long_name_{i}.py",
            f"def fn_{i}(x):\n    y = x + {i}\n    return y * 2\n",
        )
    repo.commit("widen every module")
    return repo


def _rendered(repo, budget):
    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", budget_tokens=budget)
    return diffctx.to_markdown(result), result


def test_the_rendered_artifact_stays_within_the_budget(tmp_path):
    repo = _wide_repo(tmp_path)
    for budget in (4000, 8000, 16000):
        md, _ = _rendered(repo, budget)
        assert count_tokens(md) <= budget, f"budget {budget} produced {count_tokens(md)} rendered tokens"


def test_a_budget_smaller_than_the_summary_yields_the_summary_alone(tmp_path):
    repo = _wide_repo(tmp_path)
    md, result = _rendered(repo, 100)

    # Deliberate: a changed path is never dropped to fit, so the summary can
    # exceed a budget that cannot hold it — but nothing else is spent.
    assert not result.get("fragments")
    assert len(result["changed_files"]) == FILE_COUNT
    assert count_tokens(md) > 100


def test_the_changed_file_list_is_printed_once(tmp_path):
    repo = _wide_repo(tmp_path)
    md, result = _rendered(repo, 4000)
    for path in result["changed_files"]:
        assert md.count(f"`{path}`") == 1, f"{path} appears more than once in the artifact"
