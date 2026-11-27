"""The oracle's silent-loss candidates, read against real code in a real git range: a definition the answer names at another
grain than the oracle's is not lost, and one it never mentions still is."""

import subprocess
from pathlib import Path

import gitrepo
from changed import changed_definitions
from compare import make_resolver, missing_definition, parse_answer, unlisted_named
from enclosure import uncovered
from measure import Prepared
from scipindex import Index
from truth import Snapshot

SCHEMA = "diffctx.impact.v2"
ENV = {**gitrepo.git_env(), "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}
IDENTITY = ["-c", "user.name=t", "-c", "user.email=t@example.invalid"]


def commit(repo: Path, files: dict[str, str | None]) -> str:
    for name, text in files.items():
        if text is None:
            (repo / name).unlink()
        else:
            (repo / name).parent.mkdir(parents=True, exist_ok=True)
            (repo / name).write_text(text)
    subprocess.run(["git", "add", "-A"], cwd=repo, env=ENV, check=True)
    subprocess.run(["git", *IDENTITY, "commit", "-q", "-m", "m"], cwd=repo, env=ENV, check=True)
    return subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=repo, env=ENV, check=True, capture_output=True, text=True
    ).stdout.strip()


def a_range(tmp_path: Path, base: dict, head: dict, language: str) -> Prepared:
    repo = tmp_path / "repo"
    repo.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, env=ENV, check=True)
    before, after = commit(repo, base), commit(repo, head)
    git_dir = repo / ".git"
    snapshots = [
        Snapshot(gitrepo.export_tree(git_dir, sha, tmp_path / "trees" / sha), Index(), language) for sha in (before, after)
    ]
    changes = gitrepo.diff_changes(git_dir, before, after)
    changed = changed_definitions(
        changes, snapshots[0].definitions, snapshots[1].definitions, snapshots[0].inert_lines, snapshots[1].inert_lines
    )
    return Prepared(snapshots[0], snapshots[1], changes, changed)


def silent_candidates(prepared: Prepared, definitions: list[dict], **extra) -> tuple[list[str], list[str]]:
    """The targets lost before and after the grains are reconciled, for an answer naming `definitions`."""
    data = {"schema": SCHEMA, "definitions": definitions, **extra}
    resolve = make_resolver(prepared.head.definitions, prepared.base.definitions)
    answer, filed = parse_answer(data, resolve), unlisted_named(data, resolve)
    scored = [(item, missing_definition(item.target, answer, filed)) for item in prepared.changed if item.status != "layout"]
    return [r["target"] for _, r in scored if r], [r["target"] for r in uncovered(scored, answer, filed, prepared)]


def entry(path: str, symbol: str, lines: str, **change) -> dict:
    return {"path": path, "symbol": symbol, "lines": lines, "kind": "function", "change": {"kind": "edited", **change}}


def test_a_local_of_a_function_the_answer_names_is_the_functions_change_and_one_of_an_unnamed_function_is_lost(tmp_path: Path):
    """click 0b7a9acc63, got 1e157c43c4: the product holds no definition of its own for a local."""
    base = "def make():\n    def show():\n        return 1\n\n    return show\n"
    head = base.replace("return 1", "return 2")
    prepared = a_range(tmp_path, {"m.py": base}, {"m.py": head}, "python")

    assert silent_candidates(prepared, [entry("m.py", "make", "1-5")]) == (["m.py::make.show"], [])
    assert silent_candidates(prepared, []) == (["m.py::make.show"], ["m.py::make.show"])


def test_a_container_added_whole_is_named_by_naming_it_or_by_naming_every_member(tmp_path: Path):
    """click 948d7a707a: a TypedDict is added and the answer names it, not its fields; hyperfine c72a4c764a: the reverse."""
    head = "class Colour:\n    RED = 1\n    GREEN = 2\n"
    prepared = a_range(tmp_path, {"m.py": "X = 1\n"}, {"m.py": "X = 1\n" + head}, "python")
    members = ["m.py::Colour.GREEN", "m.py::Colour.RED"]

    assert silent_candidates(prepared, [entry("m.py", "Colour", "2-4")])[1] == []
    both = [entry("m.py", "Colour.RED", "3-3"), entry("m.py", "Colour.GREEN", "4-4")]
    assert silent_candidates(prepared, both)[1] == []
    one = silent_candidates(prepared, [entry("m.py", "Colour.RED", "3-3")])
    assert sorted(one[1]) == sorted(["m.py::Colour", "m.py::Colour.GREEN"])  # a member and the container it leaves out are lost
    assert sorted(silent_candidates(prepared, [])[1]) == sorted(["m.py::Colour", *members])


def test_a_member_of_a_container_that_is_not_added_whole_is_never_covered_by_naming_the_container(tmp_path: Path):
    base = "class Shop:\n    def total(self):\n        return 1\n\n    def tax(self):\n        return 2\n"
    head = base.replace("return 2", "return 3")
    prepared = a_range(tmp_path, {"m.py": base}, {"m.py": head}, "python")

    assert silent_candidates(prepared, [entry("m.py", "Shop", "1-6")]) == (["m.py::Shop.tax"], ["m.py::Shop.tax"])


def test_a_container_whose_changed_lines_all_lie_in_members_the_answer_names_changed_nothing_itself(tmp_path: Path):
    """express 323a38965a, jsoup f062a8abb0: the object or enum constant holds the changed method; the answer names it."""
    base = "var users = {\n  get: function(req, res){\n    res.send(req.uid);\n  },\n  list: 1\n};\n"
    head = base.replace("res.send(req.uid)", "res.send(escape(req.uid))")
    prepared = a_range(tmp_path, {"m.js": base}, {"m.js": head}, "javascript")

    assert silent_candidates(prepared, [entry("m.js", "get", "2-4")]) == (["m.js::users"], [])
    elsewhere = base.replace("list: 1", "list: 2")
    moved = a_range(tmp_path / "other", {"m.js": base}, {"m.js": elsewhere}, "javascript")
    assert silent_candidates(moved, [entry("m.js", "get", "2-4")]) == (["m.js::users"], ["m.js::users"])  # its own line moved


def test_a_container_whose_own_tokens_are_the_same_on_both_sides_changed_nothing_beyond_its_members(tmp_path: Path):
    """jsoup 91b5a56c63: a test is added and the file's last line gains its newline; the class itself is the same."""
    base = "class A {\n  void f() {}\n}"
    head = "class A {\n  void f() {}\n  void g() {}\n}\n"
    prepared = a_range(tmp_path / "same", {"A.java": base}, {"A.java": head}, "java")

    assert silent_candidates(prepared, [entry("A.java", "g", "3-3")]) == (["A.java::A"], [])
    renamed = head.replace("class A {", "class A extends B {")
    changed = a_range(tmp_path / "extends", {"A.java": base}, {"A.java": renamed}, "java")
    assert silent_candidates(changed, [entry("A.java", "g", "3-3")]) == (["A.java::A"], ["A.java::A"])


def test_a_renamed_entry_accounts_for_one_removed_definition_of_its_file_and_no_more(tmp_path: Path):
    """cli 596e7c5a1a: the answer states `origin: renamed` and never the old name."""
    base = "def old_name():\n    return 1\n\n\ndef gone():\n    return 9\n"
    head = "def new_name():\n    return 2\n"
    prepared = a_range(tmp_path, {"m.py": base}, {"m.py": head}, "python")

    before, after = silent_candidates(prepared, [entry("m.py", "new_name", "1-2", origin="renamed")])

    assert sorted(before) == ["m.py::gone", "m.py::old_name"]
    assert len(after) == 1  # one of the two removals is the rename, the other is lost


def test_a_loss_no_grain_explains_stays_silent(tmp_path: Path):
    """click 77bd81f3f0: assignments added beside a removal, named nowhere, are the engine's loss."""
    base = "Old = 1\n\n\ndef f():\n    return 1\n"
    head = "A = 3\nB = 2\n\n\ndef f():\n    return 2\n"
    prepared = a_range(tmp_path, {"m.py": base}, {"m.py": head}, "python")

    before, after = silent_candidates(prepared, [entry("m.py", "f", "5-6")])

    assert sorted(after) == sorted(before) == ["m.py::A", "m.py::B", "m.py::Old"]
