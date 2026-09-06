"""The shipped stopping rule must be observable through the public API (#175).

`tau` gates the greedy loop: once a candidate's marginal density falls below
`tau * peak_density` the loop stops instead of spending the rest of the budget.
Every entry point — CLI, Python, MCP — ships `tau=0.12`, but the oracle corpus
passed a hand-written `0.0`, which makes the predicate unreachable. So the rule
that decides how much context real users get was gated by a corpus that never
ran it: 103 corpus cases were recorded as broken while passing at the shipped
value, and 16 failed without ever being listed.

These tests pin the two properties that made that possible: the shipped default
must change real output (an inert default is indistinguishable from a deleted
rule), and the defaults must not drift apart between entry points.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from tests.framework.pygit2_backend import Pygit2Repo

CONSUMER_COUNT = 8


@pytest.fixture
def stopping_repo(tmp_path):
    """A hub with many peripheral consumers of escalating size — the shape that
    makes marginal density decay, which is the only thing the rule reacts to.
    A repo with one tiny consumer cannot distinguish any value of tau."""
    repo = Pygit2Repo(tmp_path / "stopping_repo")
    repo.add_file(
        "src/logger.py",
        "class Logger:\n"
        "    def __init__(self, channel):\n"
        "        self.channel = channel\n\n"
        "    def info(self, message):\n"
        "        return f'[{self.channel}] INFO: {message}'\n",
    )
    for i in range(CONSUMER_COUNT):
        filler = "".join(f"        step_{j} = {j} * {i + 1}\n" for j in range(i * 4))
        repo.add_file(
            f"src/service_{i}.py",
            f"from logger import Logger\n\n\n"
            f"class Service{i}:\n"
            f"    def __init__(self, logger: Logger):\n"
            f"        self.logger = logger\n\n"
            f"    def run(self, payload):\n"
            f"{filler}"
            f"        return self.logger.info(payload)\n",
        )
    repo.commit("initial")

    repo.add_file(
        "src/logger.py",
        "class Logger:\n"
        "    def __init__(self, channel):\n"
        "        self.channel = channel\n"
        "        self.context = {}\n\n"
        "    def with_context(self, context):\n"
        "        self.context = {**self.context, **context}\n"
        "        return self\n\n"
        "    def info(self, message):\n"
        "        return self._format('INFO', message)\n\n"
        "    def warning(self, message):\n"
        "        return self._format('WARNING', message)\n\n"
        "    def _format(self, level, message):\n"
        "        suffix = f' {self.context}' if self.context else ''\n"
        "        return f'[{self.channel}] {level}: {message}{suffix}'\n",
    )
    repo.commit("add context and warning level to Logger")
    return repo


def _run(repo_path, tau):
    from diffctx._native.pipeline import build_diff_context

    return build_diff_context(repo_path, "HEAD~1..HEAD", budget_tokens=8000, tau=tau)


def _shipped_tau():
    from diffctx.cli import _DEFAULT_TAU

    return _DEFAULT_TAU


def _content_tokens(result):
    import tiktoken

    enc = tiktoken.get_encoding("o200k_base")
    return sum(len(enc.encode(f["content"])) for f in result["fragments"])


def test_the_shipped_default_is_the_greedy_loops_real_operating_point(stopping_repo):
    """Disabling the rule must change what comes out. If these agree, the corpus
    is gating an algorithm nobody ships and a deleted stopping rule passes.

    The measurable effect is tokens, not fragment count: the rule stops the
    greedy before it admits whole bodies for peripheral files, and a post-pass
    still represents those files — with a cheaper fragment. Same files, same
    count, a fraction of the tokens. Asserting on the count would pass with the
    rule deleted."""
    shipped = _content_tokens(_run(stopping_repo.path, _shipped_tau()))
    disabled = _content_tokens(_run(stopping_repo.path, 0.0))

    assert shipped < disabled, (
        f"tau={_shipped_tau()} emitted {shipped} content tokens, no fewer than "
        f"tau=0.0 ({disabled}) — the shipped stopping rule is inert on a repo built "
        f"to trigger it, so no test distinguishes it from no rule at all"
    )


def test_the_stop_trades_resolution_on_peripheral_files_not_coverage(stopping_repo):
    """What the rule gives up is how finely peripheral files are represented,
    never whether they appear. A file dropping out entirely is a different and
    worse trade than a file appearing at coarser resolution, so the two must not
    be allowed to blur into one 'fewer tokens' number."""
    shipped = _run(stopping_repo.path, _shipped_tau())
    disabled = _run(stopping_repo.path, 0.0)

    shipped_paths = {f["path"] for f in shipped["fragments"]}
    disabled_paths = {f["path"] for f in disabled["fragments"]}

    assert shipped_paths == disabled_paths, (
        f"the stopping rule changed which files are represented, not just how "
        f"finely: only without the stop {sorted(disabled_paths - shipped_paths)}, "
        f"only with it {sorted(shipped_paths - disabled_paths)}"
    )


def test_the_stopping_certificate_only_exists_when_the_rule_fires(stopping_repo):
    """The certificate is `tau * peak_density`, the bound on utility the unspent
    budget could still have bought. At tau=0.0 there is nothing to certify."""
    shipped = _run(stopping_repo.path, _shipped_tau())["latency"]
    disabled = _run(stopping_repo.path, 0.0)["latency"]

    assert shipped["stopping_certificate"] > 0.0, (
        "the shipped tau produced no stopping certificate: the loop ran to "
        "budget exhaustion, so the adaptive stop never engaged"
    )
    assert disabled["stopping_certificate"] == 0.0


def test_no_entry_point_carries_a_tau_of_its_own():
    """The corpus drifted to its own tau and stayed there unnoticed. Defaults
    that disagree between entry points are how that happens again — and a
    wrapper COPYING the right default is still a second copy, one edit away
    from disagreeing.

    So no wrapper carries a value at all: an unspecified tau reaches the engine
    as `None` and the engine resolves it, which it does differently for a
    scorer that builds no graph and therefore has no admission gate. Copying
    the default here also made that unnameable — a caller asking for exactly
    0.05 was indistinguishable from a caller asking for nothing, and an ungated
    sweep cell silently got the raised threshold instead of the one it named.
    """
    import inspect

    from diffctx import _diffctx
    from diffctx._native.pipeline import build_diff_context

    native_default = inspect.signature(build_diff_context).parameters["tau"].default

    assert native_default is None, (
        f"the native wrapper carries its own tau ({native_default}); an "
        "unspecified threshold must reach the engine as None so the engine "
        "resolves it per scorer"
    )
    # The engine's own constant is still the single source of the shipped
    # value, and the CLI help and the corpus both quote it.
    assert _shipped_tau() == pytest.approx(_diffctx.DEFAULT_TAU)


def _tau_defaults_in(source_path):
    """Every literal assigned to a parameter or attribute named `tau`, read from
    the source rather than by importing it.

    The eval package pulls in numpy, which the test environment deliberately
    does not carry — importing these modules made this guard fail in CI while
    passing locally. Skipping on ImportError would have been worse: the guard
    would go quiet in exactly the environment that gates merges. A static read
    needs no dependency and still sees a hardcoded number."""
    import ast

    tree = ast.parse(Path(source_path).read_text(encoding="utf-8"))
    found = []
    for node in ast.walk(tree):
        if isinstance(node, ast.arguments):
            for arg, default in zip(node.args[-len(node.defaults) :], node.defaults, strict=False):
                if arg.arg == "tau" and isinstance(default, ast.Constant) and isinstance(default.value, (int, float)):
                    found.append(default.value)
            for arg, default in zip(node.kwonlyargs, node.kw_defaults, strict=False):
                if arg.arg == "tau" and isinstance(default, ast.Constant) and isinstance(default.value, (int, float)):
                    found.append(default.value)
        elif isinstance(node, ast.AnnAssign):
            target = node.target
            if isinstance(target, ast.Name) and target.id == "tau" and isinstance(node.value, ast.Constant):
                if isinstance(node.value.value, (int, float)):
                    found.append(node.value.value)
    return found


def test_the_measurement_harnesses_score_the_shipped_tau():
    """Three harnesses drifted to private taus — the yaml corpus to 0.0, the
    in-memory runner to 0.05, contextbench to 0.08 — so all three scored an
    algorithm nobody runs. A harness measuring its own operating point produces
    numbers that look valid and mean nothing, which is why this is pinned here
    rather than left to review."""
    shipped = _shipped_tau()
    repo_root = Path(__file__).resolve().parent.parent

    for rel in ("eval/workflows/contextbench.py", "eval/harness/adapters/runner.py"):
        for literal in _tau_defaults_in(repo_root / rel):
            assert literal == pytest.approx(shipped), (
                f"{rel} hardcodes tau={literal} against a shipped {shipped}; "
                f"resolve it from the engine instead of restating it"
            )

    # The Rust harnesses have the same failure mode and no Python surface to
    # inspect, so they are pinned by the constant's absence: a bare literal
    # passed to the in-memory runner is what made it score tau=0.05.
    harness = (repo_root / "crates/diffctx-native/src/test_harness.rs").read_text(encoding="utf-8")
    assert "DEFAULT_STOPPING_THRESHOLD" in harness, (
        "the in-memory harness stopped using the shipped constant; it once ran "
        "tau=0.05 and reported numbers for a selector nobody ships"
    )


def test_the_shipped_tau_keeps_the_changed_file(stopping_repo):
    """The stop must never cost the change itself. Whatever it prunes, the
    fragments carrying the diff are the one thing the output cannot omit."""
    result = _run(stopping_repo.path, _shipped_tau())
    paths = {f["path"] for f in result["fragments"]}
    assert any(
        p.endswith("logger.py") for p in paths
    ), f"the changed file is absent from the output at the shipped tau: {sorted(paths)}"


def test_the_shipped_defaults_have_exactly_one_source():
    """Every entry point now reads tau, alpha and the scoring mode from the
    extension. Restating any of them as a literal is what let the CLI, the MCP
    server and two harnesses disagree, and it is what would make a future flip
    of the default scoring mode reach some callers and not others — Python
    always passes `scoring_mode` explicitly, so a stale literal here silently
    overrides a changed engine default."""
    from diffctx import _diffctx
    from diffctx._native.pipeline import build_diff_context
    from diffctx.cli import _DEFAULT_ALPHA, _DEFAULT_SCORING, _DEFAULT_TAU

    assert _DEFAULT_TAU == pytest.approx(_diffctx.DEFAULT_TAU)
    assert _DEFAULT_ALPHA == pytest.approx(_diffctx.DEFAULT_ALPHA)
    assert _DEFAULT_SCORING == _diffctx.DEFAULT_SCORING

    import inspect

    params = inspect.signature(build_diff_context).parameters
    assert params["scoring_mode"].default == _diffctx.DEFAULT_SCORING
    # tau is deliberately absent from this list: it defers rather than copies,
    # which `test_no_entry_point_carries_a_tau_of_its_own` is what asserts.
    assert params["tau"].default is None
    assert params["alpha"].default == pytest.approx(_diffctx.DEFAULT_ALPHA)
