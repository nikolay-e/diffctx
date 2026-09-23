from __future__ import annotations

import json

import pytest

import diffctx
from diffctx._diffctx import count_tokens
from tests.framework.pygit2_backend import Pygit2Repo

from .conftest import run_diffctx_subprocess

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
    # 2000 and 3000 are the cells that bind: the artifact renders ~4.3k when
    # everything fits, so a budget above that cannot go red however the
    # envelope is accounted. 8000 stays as the "does not over-trim" control.
    for budget in (2000, 3000, 4000, 8000):
        md, _ = _rendered(repo, budget)
        assert count_tokens(md) <= budget, f"budget {budget} produced {count_tokens(md)} rendered tokens"


@pytest.mark.parametrize("fmt", ["md", "yaml", "json", "txt"])
@pytest.mark.parametrize("budget", [4000, 6000])
def test_the_stdout_artifact_of_every_format_stays_within_the_budget(tmp_path, fmt, budget):
    """What the shell user receives is the artifact: the JSON one used to
    carry a `latency` block that no other format rendered and that pushed the
    document past `--budget` by hundreds of tokens."""
    repo = _wide_repo(tmp_path)
    result = run_diffctx_subprocess([".", "--diff", "HEAD~1", "--budget", str(budget), "-f", fmt, "-q"], cwd=repo.path)
    assert result.returncode == 0, result.stderr
    assert count_tokens(result.stdout) <= budget, f"{fmt} at budget {budget} wrote {count_tokens(result.stdout)} tokens"


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
    # Without an actual omission there is no second list to find, and the
    # assertion below would hold on the pre-#241 build too.
    represented = {f["path"] for f in result.get("fragments") or []}
    assert set(result["changed_files"]) - represented, "the budget must force at least one omission"
    assert "not represented in the output" not in md
    for path in result["changed_files"]:
        assert md.count(f"`{path}`") == 1, f"{path} appears more than once in the artifact"


def _rewritten_list_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "list")
    repo.add_file("vals.py", "VALUES = [\n" + "".join(f"    {i},\n" for i in range(300)) + "]\n")
    repo.commit("base")
    repo.add_file("vals.py", "VALUES = [\n" + "".join(f"    {i * 7},\n" for i in range(300)) + "]\n")
    repo.commit("rewrite every value")
    return repo


@pytest.mark.parametrize("fmt", ["md", "yaml", "json", "txt"])
def test_a_larger_budget_never_shows_less_of_the_change(tmp_path, fmt):
    import json

    repo = _rewritten_list_repo(tmp_path)
    shown = []
    for budget in (600, 900, 1200, 1500, 2000, 3000):
        result = run_diffctx_subprocess([".", "--diff", "HEAD~1..HEAD", "--budget", str(budget), "-f", fmt, "-q"], cwd=repo.path)
        assert count_tokens(result.stdout) <= budget
        doc = json.loads(
            run_diffctx_subprocess(
                [".", "--diff", "HEAD~1..HEAD", "--budget", str(budget), "-f", "json", "-q"], cwd=repo.path
            ).stdout
        )
        assert [c["represented"] for c in doc["changes"]] == [True], doc.get("coverage")
        start, end = (int(n) for n in doc["fragments"][0]["lines"].split("-"))
        shown.append(end - start + 1)
    assert shown == sorted(shown), shown


def test_budget_fitting_drops_context_whole_and_never_marks_it_as_the_change(tmp_path):
    from diffctx.writer import fit_to_budget

    repo = Pygit2Repo(tmp_path / "ctx")
    body = "".join(f"    acc_{i} = rows[{i}] * {i}\n" for i in range(120))
    repo.add_file("lib/helpers.py", f"def compute_totals(rows):\n{body}    return acc_0\n")
    repo.add_file("app/main.py", "from lib.helpers import compute_totals\n\ndef run(rows):\n    return compute_totals(rows)\n")
    repo.commit("base")
    repo.add_file(
        "app/main.py", "from lib.helpers import compute_totals\n\ndef run(rows):\n    return compute_totals(rows) + 1\n"
    )
    repo.commit("change")

    tree = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1..HEAD", budget_tokens=20_000)
    context = [f for f in tree["fragments"] if f.get("role") != "changed"]
    assert any(f["path"] == "lib/helpers.py" and len(f["content"].splitlines()) > 50 for f in context), context

    changed_tokens = sum(count_tokens(f["content"]) for f in tree["fragments"] if f.get("role") == "changed")
    tree["provenance"]["selection"]["budget_tokens"] = count_tokens(json.dumps({**tree, "fragments": []})) + changed_tokens + 60
    fitted, _ = fit_to_budget(tree, "json")
    for fragment in fitted["fragments"]:
        if fragment.get("role") != "changed":
            assert "more lines of this change" not in fragment["content"], fragment
