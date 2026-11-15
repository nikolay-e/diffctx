"""The class-test gate (T9): a fix ships with a test that is red on the base and
green on the head, or says in the PR body why it needs none.

Real git repositories and the real script. Only `cargo` is replaced: building
the engine is outside this test's reach, so a stand-in answers the way cargo
would for a tree whose engine is fixed (`src/lib.rs` holds `v2`) or not
(`v1`): a case or suite that reads `needs_fix` fails on a tree that is not.
"""

import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parent.parent / ".github" / "scripts" / "class-test.sh"
TEMPLATE = Path(__file__).parent.parent / ".github" / "PULL_REQUEST_TEMPLATE.md"
CRATE = "crates/diffctx-native"
ENGINE = f"{CRATE}/src/lib.rs"

pytestmark = pytest.mark.skipif(
    sys.platform == "win32", reason="bash on a Windows runner may resolve to WSL's; the gate runs on Linux"
)

FAKE_CARGO = """#!{python}
import os, pathlib, sys

args = sys.argv[1:]
crate = pathlib.Path.cwd()
root = crate.parent.parent
engine = (crate / "src" / "lib.rs").read_text()
MARKER = "diffctx-case: assertion failed"

if args[0] == "build":
    if "nobuild" in engine:
        sys.exit(1)
    if "nobinary" not in engine:
        binary = pathlib.Path(os.environ["CARGO_TARGET_DIR"]) / "release" / "diffctx"
        binary.parent.mkdir(parents=True, exist_ok=True)
        binary.write_text(engine)
        binary.chmod(0o755)
    sys.exit(0)
if "--no-run" in args:
    harness = (crate / "tests" / "yaml_cases.rs").read_text()
    sys.exit(1 if "yaml_cases" in args and "uses_new_api" in harness and "v2" not in engine else 0)
cases = sorted(p for p in (root / "tests" / "cases").rglob("*.yaml"))

def trial(path):
    parts = path.relative_to(root / "tests" / "cases").with_suffix("").parts
    return "/".join(parts[1:] if parts[0] == "diff" else parts)

if "--list" in args:
    for path in cases:
        print(f"[yaml] {{trial(path)}}: test")
    sys.exit(0)
if "--exact" in args:
    [path] = [p for p in cases if trial(p) == args[args.index("--exact") + 1]]
    text = path.read_text()
    # An impact or symbol case asks the binary DIFFCTX_BIN names; a selection
    # case asks the engine its own tree carries.
    engine = pathlib.Path(os.environ["DIFFCTX_BIN"]).read_text() if "DIFFCTX_BIN" in os.environ else engine
    if "crash" in engine or "setup_error" in text:
        print("thread panicked before any assertion")
        sys.exit(101)
    if "needs_fix" in text and "v2" not in engine:
        print(MARKER)
        sys.exit(101)
    sys.exit(0)
suite = args[args.index("--test") + 1]
text = (crate / "tests" / f"{{suite}}.rs").read_text()
if "crash" in text:
    print("process aborted")
    sys.exit(134)
if "needs_fix" in text and "v2" not in engine:
    print("test result: FAILED. 0 passed; 1 failed")
    sys.exit(101)
sys.exit(0)
"""


def _env(tmp_path: Path) -> dict[str, str]:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.com",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.com",
    )
    return env


def _git(repo: Path, env: dict[str, str], *args: str) -> str:
    return subprocess.run(["git", *args], cwd=repo, check=True, capture_output=True, text=True, env=env).stdout.strip()


def _commit(repo: Path, env: dict[str, str], subject: str, files: dict[str, str | None]) -> str:
    for name, content in files.items():
        path = repo / name
        if content is None:
            path.unlink()
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
    _git(repo, env, "add", "-A")
    _git(repo, env, "commit", "-q", "-m", subject)
    return _git(repo, env, "rev-parse", "HEAD")


def _run_gate(
    tmp_path: Path,
    commits: list[tuple[str, dict[str, str | None]]],
    body: str = "",
    base_engine: str = "v1\n",
) -> subprocess.CompletedProcess:
    env = _env(tmp_path)
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, env, "init", "-q", "-b", "main")
    base = _commit(
        repo,
        env,
        "chore: base",
        {
            ENGINE: base_engine,
            f"{CRATE}/tests/yaml_cases.rs": "harness\n",
            f"{CRATE}/tests/metamorphic.rs": "// no property yet\n",
            "tests/cases/impact/python/existing.yaml": "always_green\n",
            "tests/cases/diff/algorithm/legacy.yaml": "always_green\n",
            "docs/notes.md": "notes\n",
        },
    )
    _git(repo, env, "checkout", "-q", "-b", "pr")
    head = base
    for subject, files in commits:
        head = _commit(repo, env, subject, files)
    stub = tmp_path / "bin"
    stub.mkdir()
    cargo = stub / "cargo"
    cargo.write_text(FAKE_CARGO.format(python=sys.executable), encoding="utf-8")
    cargo.chmod(0o755)
    env.update(
        PATH=f"{stub}{os.pathsep}{os.environ['PATH']}",
        BASE_SHA=base,
        HEAD_SHA=head,
        PR_BODY=body,
        CLASS_TEST_WORK=str(tmp_path / "work"),
    )
    return subprocess.run(["bash", str(SCRIPT)], cwd=repo, capture_output=True, text=True, env=env, check=False)


FIX = ("fix: stop the false clean", {ENGINE: "v2\n"})
RED_CASE = {"tests/cases/impact/python/body_only.yaml": "needs_fix\n"}
GREEN_CASE = {"tests/cases/impact/python/unrelated.yaml": "always_green\n"}


class TestAFixNeedsItsClassTest:
    def test_a_case_red_on_the_base_and_green_on_the_head_passes(self, tmp_path):
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **RED_CASE})])
        assert result.returncode == 0, result.stdout + result.stderr
        assert "case impact/python/body_only is red on the base" in result.stdout
        assert "case impact/python/body_only is green on the head" in result.stdout

    def test_a_legacy_diff_case_is_found_by_its_name_inside_the_diff_directory(self, tmp_path):
        case = {"tests/cases/diff/algorithm/shape.yaml": "needs_fix\n"}
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **case})])
        assert result.returncode == 0, result.stdout + result.stderr
        assert "case algorithm/shape is red on the base" in result.stdout

    def test_a_suite_red_on_the_base_and_green_on_the_head_passes(self, tmp_path):
        suite = {f"{CRATE}/tests/metamorphic.rs": "needs_fix\n"}
        result = _run_gate(tmp_path, [("fix(impact): the property", {ENGINE: "v2\n", **suite})])
        assert result.returncode == 0, result.stdout + result.stderr
        assert "suite metamorphic is red on the base" in result.stdout

    def test_a_fix_with_no_test_at_all_is_refused(self, tmp_path):
        result = _run_gate(tmp_path, [FIX])
        assert result.returncode == 1
        assert "no class test was added or changed" in result.stdout + result.stderr

    def test_an_unrelated_test_change_does_not_satisfy_it(self, tmp_path):
        """The case is green on the base: it would have passed without the fix."""
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **GREEN_CASE})])
        assert result.returncode == 1
        assert "expected red on the base" in result.stdout + result.stderr

    def test_a_case_the_fix_does_not_turn_green_is_refused(self, tmp_path):
        result = _run_gate(tmp_path, [("fix: a fix that fixes nothing", {ENGINE: "v1 again\n", **RED_CASE})])
        assert result.returncode == 1
        assert "expected green on the head" in result.stdout + result.stderr

    def test_a_deleted_case_is_no_class_test(self, tmp_path):
        result = _run_gate(
            tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", "tests/cases/impact/python/existing.yaml": None})]
        )
        assert result.returncode == 1


class TestWhereEachKindOfTestRuns:
    def test_an_impact_case_runs_the_heads_harness_against_a_binary_built_from_the_base(self, tmp_path):
        """A harness that needs what only the head has still reads the case: the base is a process."""
        harness = {f"{CRATE}/tests/yaml_cases.rs": "uses_new_api\n"}
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **harness, **RED_CASE})])
        assert result.returncode == 0, result.stdout + result.stderr
        assert "case impact/python/body_only is red on the base" in result.stdout

    def test_a_selection_case_runs_on_the_bases_own_harness(self, tmp_path):
        """The case file is the pull request's; a harness change of its own never reaches the base."""
        harness = {f"{CRATE}/tests/yaml_cases.rs": "uses_new_api\n"}
        case = {"tests/cases/diff/algorithm/shape.yaml": "needs_fix\n"}
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **harness, **case})])
        assert result.returncode == 0, result.stdout + result.stderr
        assert "case algorithm/shape is red on the base" in result.stdout


class TestRedMeansAFailedAssertion:
    """Anything else that stops a test on the base is an error of the gate (exit 2): it proves nothing."""

    def _gate_error(self, result):
        assert result.returncode == 2, result.stdout + result.stderr
        assert "without a failed assertion" in result.stdout + result.stderr
        assert "is red on the base" not in result.stdout

    def test_a_base_binary_that_crashes_is_no_red_case(self, tmp_path):
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **RED_CASE})], base_engine="v1 crash\n")
        self._gate_error(result)

    def test_a_case_whose_setup_fails_is_no_red_case(self, tmp_path):
        case = {"tests/cases/impact/python/body_only.yaml": "needs_fix setup_error\n"}
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **case})])
        self._gate_error(result)

    def test_a_selection_case_that_dies_on_the_base_is_no_red_case(self, tmp_path):
        case = {"tests/cases/diff/algorithm/shape.yaml": "needs_fix setup_error\n"}
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **case})])
        self._gate_error(result)

    def test_a_suite_that_aborts_on_the_base_is_no_red_suite(self, tmp_path):
        suite = {f"{CRATE}/tests/metamorphic.rs": "needs_fix crash\n"}
        result = _run_gate(tmp_path, [("fix: the property", {ENGINE: "v2\n", **suite})])
        self._gate_error(result)

    def test_a_base_that_leaves_no_binary_is_a_gate_error(self, tmp_path):
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **RED_CASE})], base_engine="v1 nobinary\n")
        assert result.returncode == 2, result.stdout + result.stderr
        assert "left no binary" in result.stdout + result.stderr

    def test_a_base_that_does_not_build_is_a_gate_error(self, tmp_path):
        result = _run_gate(tmp_path, [("fix: stop the false clean", {ENGINE: "v2\n", **RED_CASE})], base_engine="v1 nobuild\n")
        assert result.returncode == 2, result.stdout + result.stderr
        assert "does not build its binary" in result.stdout + result.stderr


class TestWhereTheGateDoesNotApply:
    def test_a_fix_that_touches_no_engine_source_is_not_gated(self, tmp_path):
        result = _run_gate(tmp_path, [("fix: a typo in the notes", {"docs/notes.md": "better notes\n"})])
        assert result.returncode == 0, result.stdout + result.stderr
        assert "nothing to gate" in result.stdout

    def test_a_commit_that_is_not_a_fix_is_not_gated(self, tmp_path):
        result = _run_gate(tmp_path, [("feat: a new flag", {ENGINE: "v2\n"})])
        assert result.returncode == 0, result.stdout + result.stderr

    def test_a_fix_among_commits_gates_the_whole_pull_request(self, tmp_path):
        result = _run_gate(tmp_path, [FIX, ("test: tidy", GREEN_CASE)])
        assert result.returncode == 1


class TestTheReviewedException:
    @pytest.mark.parametrize(
        "line",
        [
            "Class test: none — truth gh#12",
            "Class test: none - leak fj#7",
            "Class test: none -- delivery gh#340",
            "  Class test: none — truth gh#12  ",
        ],
    )
    def test_a_stated_reason_and_issue_pass_the_gate_for_the_orchestrator_to_read(self, tmp_path, line):
        result = _run_gate(tmp_path, [FIX], body=f"## Summary\n\nfixes it\n\n{line}\n\n## Testing\n")
        assert result.returncode == 0, result.stdout + result.stderr
        assert "reviewed" in result.stdout

    @pytest.mark.parametrize(
        "line",
        [
            "Class test: none",
            "Class test: none — style gh#12",
            "Class test: none — truth",
            "Class test: none — truth #12",
            "Class test:",
            "I wrote a Class test: none — truth gh#12 somewhere",
        ],
    )
    def test_anything_less_does_not(self, tmp_path, line):
        result = _run_gate(tmp_path, [FIX], body=line)
        assert result.returncode == 1

    def test_the_template_left_as_it_is_exempts_nothing(self, tmp_path):
        result = _run_gate(tmp_path, [FIX], body=TEMPLATE.read_text(encoding="utf-8"))
        assert result.returncode == 1
