# diffctx

## Ultimate Goal

**Maximize the speed and depth of understanding textual information — for any
reader, in any scenario.**

Whether the consumer is an LLM filling a context window or a human reviewing a
change, the job is to extract the maximum signal from a codebase in the densest
clear form. The single lens for every trade-off is **comprehension-per-token**:
understanding gained per unit of attention spent. That is why Markdown is the
default output — ~7% cheaper than YAML on real diffs (#104).

The theory behind diff-context selection is the paper
([source in `paper/v2/`, snapshot at tag `paper-v2`](https://doi.org/10.5281/zenodo.18824579)).
Setup, suite shape and the per-case YAML corpus gate
(`crates/diffctx-native/tests/known_below_threshold.txt`, bidirectional: a
listed case that starts passing also fails) are in `CONTRIBUTING.md`.

## History is append-only

`main` is never rewritten: forks, contributors' pull requests, release tags and
the provenance of published wheels all name its commits, and a rewrite hides
nothing — every pushed commit stays reachable through pull-request refs and
forks. The maintainers' work reaches `main` one epoch at a time; until then it
is a single commit on `next` above `main`, never a pile of agent commits.
`scripts/epoch-push.sh` folds local commits into it and pushes `next` (the open
epoch PR runs CI and the review context); `scripts/epoch-push.sh land`
fast-forwards `main` once the epoch is done. A contributor's commit lands on
`main` as itself — never folded, amended or restamped: the open epoch lands
first and their commit goes on top of it, keeping its author and date.
Dependency bumps land on `main` and the epoch rebases over them.

## Output-changing changes are judged by correctness

A change either keeps selection output identical (E-class) or moves it
(Q-class). Neither is frozen: the algorithm is meant to improve, and the
project is not bound to the paper's numbers. The question for an
output-changing fix is whether the new output is right — shown on a fixture
whose right answer is known, and by reading each corpus case that flips,
never by its byte distance from the old output.

- **A claimed pure refactor** cites `scripts/bitcheck.sh` (`record` on the
  pre-change build, `check` after: 30 cells against a worktree pinned at a
  fixed SHA — pack and locate per range and scoring mode, impact per range,
  three `--symbol` lookups). It drives the native binary only; `writer.py`,
  `_native/` and `mcp/` need their own byte-diff through the installed wheel.
- **The full corpus** (`cargo test --release --test yaml_cases`) is the only
  net over the per-language edge and parser tables; a fix that flips cases
  reads them and edits the bidirectional baseline in the same commit.
- **Per release, optionally:** `python -m eval equivalence --a <old> --b
  <new>` on the stratified 40-instance sample (needs two full eval outputs and
  a local `results/` tree, so no CI job runs it).

## Reading test and oracle results

- **A test never times the run.** A wall-clock bound is green on a laptop and
  red on a loaded CI runner (a 2.5 s gate measured 2.8 s on Linux and 5.5 s on
  Windows). Stop a run with `DIFFCTX_TEST_DEADLINE_EXPIRED=1` or
  `DIFFCTX_TEST_DEADLINE_AFTER_POLLS=n` and assert what it read or named.
- **A test that clones a bare remote names the branch** (`clone -b main`):
  CI's git defaults to `master`, so the clone comes up empty and the push is
  rejected, while a laptop's `init.defaultBranch=main` hides it. Prove such a
  test under `GIT_CONFIG_GLOBAL=/dev/null`.

- **Garbage injection is opt-in.** Only the few YAML cases that set
  `fixtures.auto_garbage` get unrelated `GARBAGE_*` files and the leak assertion
  in `yaml_cases.rs`. The relevance filter is actually guarded by the Python
  corpus in `tests/garbage_data.py`, injected by `test_mcp.py` and
  `test_e2e_cli_scenarios.py`. Turning the flag on broadly is an unpulled lever,
  not existing coverage.
- `changed_files` in a case YAML is the **final state of every file**, not the
  diff; a path listed there is not evidence the tool must emit it.
- `forbidden_rate` saturates: one wrong file trips its path entry plus several
  anchor-only entries and drives `score = recall * (1 - forbidden_rate)` to
  zero. 100% means "at least one wrong file" (median: one extra file), not
  "emitted all the garbage".
- The harness passes `DEFAULT_PPR_ALPHA` and an unspecified tau (`None`) so the
  gate measures the shipped configuration — it once passed `tau=0.0`, which hid
  119 cases in both directions (#175). Never replace either with a literal or
  with the tau constant: an unspecified tau resolves per scorer (the ungated
  ones ship at a raised threshold), so naming the constant pins them wrong.
