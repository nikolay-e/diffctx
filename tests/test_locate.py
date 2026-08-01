from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

PROJECT_ROOT = Path(__file__).parent.parent
SRC_DIR = PROJECT_ROOT / "src"


@pytest.fixture
def locate_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "locate_repo")
    repo.add_file("src/calc.py", "def add(a, b):\n    return a + b\n")
    repo.add_file(
        "src/main.py",
        "from calc import add\n\ndef run():\n    return add(1, 2)\n",
    )
    repo.add_file(
        "checks/test_calc.py",
        "from calc import add\n\ndef test_add():\n    assert add(1, 2) == 3\n",
    )
    base = repo.commit("initial")
    repo.add_file(
        "src/calc.py",
        "def add(a, b):\n    return a + b\n\ndef sub(a, b):\n    return a - b\n",
    )
    head = repo.commit("add sub")
    return repo, f"{base}..{head}"


def _run(cwd: Path, args: list[str]) -> subprocess.CompletedProcess[str]:
    env = {**os.environ, "PYTHONPATH": str(SRC_DIR)}
    return subprocess.run(
        [sys.executable, "-m", "diffctx", *args],
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=120,
    )


class TestLocateMode:
    def test_emits_versioned_schema_without_source(self, locate_repo):
        repo, diff_range = locate_repo
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "-q"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)
        assert doc["schema"] == "diffctx.locate.v1"
        assert doc["item_count"] == len(doc["items"]) > 0
        for item in doc["items"]:
            assert {"path", "lines", "kind", "score", "tokens", "reasons"} <= item.keys()
            assert item["reasons"], "every ranked item carries >=1 provenance reason"
        assert "def add" not in result.stdout

        summary = doc["summary"]
        assert summary["changed"] == sum(1 for i in doc["items"] if i.get("role") == "changed")
        assert summary["context"] == doc["item_count"] - summary["changed"]
        assert summary["files"] == len({i["path"] for i in doc["items"]})
        test_items = [i for i in doc["items"] if i.get("group") == "test"]
        assert summary["tests"] == len(test_items)
        assert any(i["path"].endswith("test_calc.py") for i in test_items), "covering test file must be flagged group=test"

    def test_changed_items_and_pack_output_unaffected(self, locate_repo):
        repo, diff_range = locate_repo
        locate = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "-q"])
        doc = json.loads(locate.stdout)
        changed = [i for i in doc["items"] if i.get("role") == "changed"]
        assert changed
        assert all(r["type"] == "changed" for i in changed for r in i["reasons"])

        pack_default = _run(repo.path, [".", "--diff", diff_range, "-q", "-f", "yaml"])
        pack_again = _run(repo.path, [".", "--diff", diff_range, "-q", "-f", "yaml"])
        assert pack_default.stdout == pack_again.stdout
        assert "fragments:" in pack_default.stdout

    def test_locate_rejects_full_and_warns_on_format(self, locate_repo):
        repo, diff_range = locate_repo
        conflict = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--full"])
        assert conflict.returncode == 2
        assert "--mode locate" in conflict.stderr

        warned = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "-f", "yaml", "-q"])
        assert warned.returncode == 0
        assert "ignored with --mode locate" in warned.stderr
        json.loads(warned.stdout)

    def test_native_build_locate_treats_an_empty_range_as_the_working_tree(self, locate_repo):
        """`build_diff_context` maps an empty diff_range to the working tree;
        `build_locate` forwarded `Some("")` into range validation instead, so the
        two entry points into the same pipeline disagreed about what an
        unspecified range means."""
        from diffctx._native.pipeline import build_locate

        repo, _ = locate_repo
        (repo.path / "src" / "calc.py").write_text("def add(a, b):\n    return a + b + 0\n")

        payload = json.loads(build_locate(repo.path, "", budget_tokens=8000))
        assert payload["schema"] == "diffctx.locate.v1"
        assert "src/calc.py" in payload["changed_files"]


class TestCoverageAndOverflow:
    """#136: what the run could not see, and what it could not fit.

    The point is trust rather than completeness — an agent told honestly where
    the selection is thin can go grep the gap itself, while one told nothing has
    to distrust the whole answer. So the contract is that these fields are
    honest and cheap, not that they are always present.
    """

    @staticmethod
    def _crowded_repo(tmp_path):
        """A repo with far more admissible context than a small budget can hold,
        so the overflow path is genuinely exercised rather than trivially empty."""
        repo = Pygit2Repo(tmp_path / "crowded_repo")
        for i in range(25):
            repo.add_file(
                f"src/mod_{i}.py",
                f"from core import shared\n\n\ndef helper_{i}(x):\n    return shared(x) + {i}\n",
            )
        repo.add_file("src/core.py", "def shared(x):\n    return x * 2\n")
        base = repo.commit("initial")
        repo.add_file("src/core.py", "def shared(x):\n    return x * 3\n")
        head = repo.commit("change shared")
        return repo, f"{base}..{head}"

    def test_overflow_names_what_did_not_fit_without_source(self, tmp_path):
        repo, diff_range = self._crowded_repo(tmp_path)
        result = _run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--budget", "200", "-q"])
        assert result.returncode == 0, result.stderr
        doc = json.loads(result.stdout)

        assert doc["overflow"], "a 200-token budget on 25 callers must leave something behind"
        assert doc["overflow_count"] >= len(doc["overflow"])
        selected = {(i["path"], i["lines"]) for i in doc["items"]}
        for entry in doc["overflow"]:
            assert {"path", "lines", "score", "tokens", "why"} <= entry.keys()
            assert entry["why"], "an overflow entry the caller cannot interpret is noise"
            assert (entry["path"], entry["lines"]) not in selected, "overflow must not repeat the selection"
        # No bodies: the overflow list is a pointer, and paying source cost for
        # what was skipped would defeat the budget it reports on.
        assert "def helper_" not in result.stdout

        scores = [e["score"] for e in doc["overflow"]]
        assert scores == sorted(scores, reverse=True), "overflow is a ranking, not a set"

    def test_overflow_is_capped_but_the_total_is_not(self, tmp_path):
        """A capped list with no true total would understate the gap in exactly
        the runs where the gap is largest."""
        repo, diff_range = self._crowded_repo(tmp_path)
        doc = json.loads(_run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--budget", "200", "-q"]).stdout)
        assert len(doc["overflow"]) <= 50
        if doc["overflow_count"] > 50:
            assert len(doc["overflow"]) == 50

    def test_an_unlimited_budget_reports_no_budget_pressure(self, tmp_path):
        """`next_up` answers "would paying more change the answer". At
        `--budget -1` nothing was crowded out by tokens, so the answer is no —
        and a coverage block with nothing to disclose is omitted entirely rather
        than spending tokens to say "fine"."""
        repo, diff_range = self._crowded_repo(tmp_path)
        doc = json.loads(_run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--budget", "-1", "-q"]).stdout)
        assert doc.get("coverage", {}).get("next_up", 0) == 0
        assert not doc.get("overflow"), "nothing is left behind when the budget is unlimited"

    def test_a_tight_budget_reports_pressure_and_a_lower_confidence(self, tmp_path):
        repo, diff_range = self._crowded_repo(tmp_path)
        tight = json.loads(_run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--budget", "200", "-q"]).stdout)
        loose = json.loads(_run(repo.path, [".", "--diff", diff_range, "--mode", "locate", "--budget", "-1", "-q"]).stdout)
        assert tight["coverage"]["next_up"] > 0
        assert tight["coverage"]["confidence"] < loose.get("coverage", {}).get("confidence", 1.0)

    def test_an_unparseable_changed_file_is_named_as_a_blind_spot(self, tmp_path):
        """The acceptance case: a file whose language diffctx claims to parse, but
        which yields no symbol-level structure. Nothing else in the output says
        the parser came back empty there — the fragments look like ordinary
        context — so this is the only signal the caller gets."""
        repo = Pygit2Repo(tmp_path / "unparseable_repo")
        repo.add_file("src/app.py", "def run():\n    return helper()\n")
        # Valid-Python-file-shaped garbage: the extension promises structure, the
        # content has none, which is exactly the case a coverage block exists for.
        repo.add_file("src/broken.py", "!!!! this is (((not python at all ][ \n" * 40)
        base = repo.commit("initial")
        repo.add_file("src/broken.py", "!!!! this is (((not python at all ][ \n" * 45)
        head = repo.commit("touch the unparseable file")

        doc = json.loads(_run(repo.path, [".", "--diff", f"{base}..{head}", "--mode", "locate", "-q"]).stdout)
        assert "src/broken.py" in doc["coverage"]["unparsed_files"]
        assert doc["coverage"]["confidence"] < 1.0

    def test_a_documentation_file_is_not_reported_as_a_blind_spot(self, tmp_path):
        """Markdown has no symbols to find, so naming it would be true and
        useless: there is nothing for the caller to grep for. `Section` is the
        markdown parser's real output, not a parse degradation."""
        repo = Pygit2Repo(tmp_path / "docs_repo")
        repo.add_file("README.md", "# Title\n\n## One\nBody one.\n")
        repo.add_file("src/app.py", "def run():\n    return 1\n")
        base = repo.commit("initial")
        repo.add_file("README.md", "# Title\n\n## One\nBody one.\n\n## Two\nBody two.\n")
        head = repo.commit("extend the docs")

        doc = json.loads(_run(repo.path, [".", "--diff", f"{base}..{head}", "--mode", "locate", "-q"]).stdout)
        assert "README.md" not in doc.get("coverage", {}).get("unparsed_files", [])

    def test_pack_output_is_unchanged_by_the_coverage_fields(self, locate_repo):
        """The coverage block is a locate-mode disclosure. If it moved a single
        byte of pack output it would be a Q-class change to the shipped default."""
        repo, diff_range = locate_repo
        first = _run(repo.path, [".", "--diff", diff_range, "-q", "--budget", "4000"])
        second = _run(repo.path, [".", "--diff", diff_range, "-q", "--budget", "4000"])
        assert first.returncode == 0, first.stderr
        assert first.stdout == second.stdout
        assert "coverage" not in first.stdout
        assert "overflow" not in first.stdout
