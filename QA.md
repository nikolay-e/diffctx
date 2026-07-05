# diffctx — QA Playbook

Project-specific QA notes. Generic QA patterns live in the `/qa` skill — do not
duplicate here. In particular, Packaging QA (wheel + clean-venv E2E,
Test-Gating Trap, mypy hook scope, `language: system` venv-shebang rot,
`which`-vs-pipx trap, maturin editable build) lives in the skill's
**Packaging QA** section — this file only records the diffctx-specific shape of
each.

## Applicability Matrix

| Check | Applies | Notes |
|---|---|---|
| CI status (`gh run`) | yes | `diffctx CI`, `CodeQL`, `Dependency Graph` workflows |
| Test suite | yes | `pytest -q`, see Test Suite Layout below |
| Pre-commit | yes | Full suite locally; see Pre-commit Caveats |
| Code review | yes | Diff-mode of own tool: `diffctx --diff <range>` |
| CLI smoke | yes | See CLI Smoke Recipes |
| Real-world test-repo sweep | yes | MANDATORY every round — see Real-World Test-Repo Sweep |
| SonarCloud | yes | Project key `nikolay-e_TreeMapper` (legacy pre-rebrand name); see SonarCloud section below |
| autoqa pipeline | no | CLI tool, no HTTP API surface |
| K8s logs / ArgoCD | no | No deployment to a cluster |
| Browser QA / Walkthrough | no | No UI |
| Schemathesis / ZAP | no | No OpenAPI / HTTP service |
| Backend smoke | no | CLI tool |

## Build & Install Layout

- Python + Rust hybrid wheel built via maturin (PEP 660).
- Editable install: `pip install -e ".[dev,full,mcp]" --no-build-isolation`
  after `pip install "maturin>=1.10,<1.14"`.
- Rust crate lives in `diffctx/` subdir; Python sources in `src/diffctx/`.
- Rust extension module name: `diffctx._diffctx` (from `[tool.maturin]`).

## Test Suite Layout

- pytest-xdist runs tests in parallel (`addopts = "-n auto --dist worksteal"`).
- 337+ pytest tests; 16 of them live in `tests/test_mcp.py` and are gated
  by `pytest.importorskip("mcp")`. They use `pytest-asyncio` with
  `asyncio_mode = auto` (in `pyproject.toml`).
- `tests/test_mcp.py` is the ONLY async-using test module. If you add
  `@pytest.mark.asyncio` decorators elsewhere, the `auto` mode picks them
  up automatically — keep `pytest-asyncio` in `[dev]`.
- 2 legitimate (not stale) conditional skips:
  `test_clipboard.py: Windows only` (skips on macOS/Linux),
  `test_mcp.py: mcp package not installed` (skips when extra is missing).

### Test-Gating Trap — diffctx specifics

See `/qa` skill: Packaging QA (Test-Gating Trap) for the general lesson. Here
it bites at the intersection of two extras: `[mcp]` must be in the CI install
extras (else `importorskip("mcp")` silently skips all 16 `test_mcp.py` tests),
AND `pytest-asyncio` + `asyncio_mode = auto` must be present (else the async
tests collect but never await — they "pass" by never running). Re-check this
every time someone adds a new `[<extra>]` that ships a tool with its own tests.

## CLI Smoke Recipes

```bash
# Tree mode (CLI is also called `diffctx`):
diffctx src/diffctx/mcp --no-content -f yaml

# Diff mode, explicit range:
diffctx --diff HEAD~3..HEAD -f yaml

# Diff mode, bare --diff (defaults to HEAD):
diffctx --diff -f yaml
```

Format flag is `-f / --format`, NOT `--output-format` (common typo).

## Pre-commit Caveats

See `/qa` skill: Packaging QA for `language: system` venv-shebang rot (recover
with `rm -rf .venv && python3 -m venv .venv && pip install "maturin>=1.10,<1.14"
&& pip install -e ".[dev,full,mcp]" --no-build-isolation`). CI is unaffected
(fresh venv per run).

diffctx-specific hygiene: stale `src/treemapper.egg-info/` from a rebrand-era
`pip install` is gitignored but may linger — delete on hygiene pass.

## Secret-Handling Test Fixtures Break the Secret Hooks

The private-key exclusion tests (`test_secret_ignores_diff.py`,
`test_default_ignores.py`) assert that diffctx drops key/keystore files. Both the
Rust `is_secret_path` and the Python `ignore.py` match **by filename only**
(`id_rsa`, `*.pem`, `*.key`, …) — the fixture content is irrelevant to what they
test. So fixtures must NOT embed a literal PEM `BEGIN…PRIVATE KEY` banner:
`detect-private-key` (no pragma support) and `detect-secrets` both flag it, and a
file committed past local hooks (e.g. `--no-verify`) then turns `Pre-commit
hooks` + `Lint & Type Check` red on `--all-files` while a 20-case CI YAML subset
stays green. Use inert content (`"private-key-material <MARKER>\n"`) plus
`# pragma: allowlist secret` for the entropy detector; keep distinctive leak
markers (`LEAK_RSA`, …) so leakage is still detectable. High-entropy base64
findings come from concatenating tokens with no separator — keep a space.

Catch this class only with the FULL local suite: `pre-commit run --all-files`
(NOT a staged-files commit run, which skips clean files). When backgrounding it,
note the shell exit code is the trailing `echo`'s, not pre-commit's — grep the
log for `Failed`, don't trust the reported exit.

## Diff-Mode Self-Eat

`diffctx --diff <range>` runs on this repo's own history. The tool is its own
test fixture. Use it during code review to surface the same semantic context an
external user would see.

**Do NOT dismiss a large output as "normal for a big commit" — measure it.** A
big *changed* diff is fine; a big *context* expansion is the over-selection bug
(#65/#59). Triage discriminator on every diff-review run:

- `role: "changed"` fragment count vs total fragment count, and
- distinct files in the output vs `git diff <range> --stat` file count.

If most fragments are context (not `changed`) and the output spans far more
files than the diff touched, that is over-selection, not a big diff. (Observed:
a docs-heavy range with ~12 changed files / 18 changed fragments expanded to
160+ files / 540+ fragments — context, mentioned-path, and lexical edges from
prose pulling in unrelated code.) The full `yaml_cases` suite quantifies the
same bug: the large majority of its failures are `forbidden_rate=100%` (pure
over-selection), not recall misses. Fixing it is a benchmark-validated
recalibration coupled to the research paper — track on #65, do not blind-edit
edge weights mid-QA.

## Real-World Test-Repo Sweep (every QA round, MANDATORY)

`test-repos/` (git-ignored, local-only) holds ~15 real upstream clones across
many languages — `linux`, `pytorch`, `react-native`, `sentry`, `gitpod`,
`elasticsearch`, `numpy`, `ocaml`, `llama.cpp`, `onnxruntime`, `libc`,
`luajit2`, `monitoring-observability`, `builder`, `vision`. `TOANALYZE.md` is
the curated commit "todo"; the clones also carry live upstream HEADs. This is
the dogfood that synthetic `yaml_cases` can't replace: real diffs, real over-
dump, real crashes.

**Every QA round, sweep the test repos against a NEW commit each** (one not
exercised before — `git -C test-repos/<repo> pull` to fetch fresh upstream
history, then diff its newest commit). Run the **pipx** binary, not a venv
shadow:

```bash
cd test-repos/<repo>
git pull --ff-only
# Hard-cap runtime. As of 3725f51b the CLI enforces `--timeout` as a true total
# wall-clock bound (worker thread + watchdog) and fast-fails rc=124 with the
# actionable-error contract instead of hanging to OOM (#70) — BUT the external
# perl cap is still needed when smoking the **pipx** binary until that fix ships
# in a release (pipx 1.10.0 has no watchdog). macOS has no `timeout`; use:
perl -e 'alarm 200; exec @ARGV' /Users/nikolay/.local/bin/diffctx . --diff HEAD~1
# (or, on the dev build, just pass `--timeout 200` — the watchdog bounds it.)
```

**A cap-hit IS the issue.** rc=142 (SIGALRM) or rc=137 (SIGKILL) with no output =
diffctx hung on that repo — file it and stop the sweep (don't run the remaining
giant repos like `linux`, they'll hang too and burn hours). Known so far:
`gitpod` and `pytorch` hang on a trivial HEAD~1 diff (#70).

**Per repo, judge:** did it (a) finish without panic / non-zero exit / hang,
(b) honor the range — `changed_files` matches `git diff HEAD~1 --stat`, not a
whole-tree dump, (c) avoid gross over-selection (mostly `role: changed` plus
tight context, not 100+ unrelated files — measure as in Diff-Mode Self-Eat),
(d) leak no secrets/garbage, (e) return in reasonable time. Any of these
failing = an issue.

**Stopping condition (the whole point):** sweep repos one at a time **until the
first issue**.

- On the first issue → `gh issue create -R nikolay-e/diffctx` with a generic,
  reproducible report (repo name, commit SHA / range, observed vs expected,
  token/fragment/file counts; **no sensitive repo contents**) → **stop the
  sweep** and move on to the rest of the QA round.
- If you get through **all** repos clean → also move on.

Either outcome — **all clean OR ≥1 issue filed** — is a valid completion of
this step; the QA round proceeds to its remaining items either way. Do not let
a found issue halt the round, and do not keep sweeping after one is filed.

## Local `which diffctx` Trap — diffctx specifics

See `/qa` skill: Packaging QA (`which`-vs-pipx). Concretely: when this project's
venv is active, `/Users/nikolay/diffctx/.venv/bin` sits FIRST on `$PATH`, so a
bare `diffctx ...` inside the working tree runs the working-tree build, NOT the
pipx-published binary. For QA code-review smoke, always use the absolute path
`/Users/nikolay/.local/bin/diffctx`. Tests, builds, and pre-commit need the venv
binary; only the user-facing smoke / review step needs the pipx one.

## YAML Cases: CI Runs Only the First 20

`ci.yml` sets `DIFFCTX_YAML_CASES_LIMIT: "20"`, so `cargo test --test yaml_cases`
in CI runs only the first 20 discovered cases (sorted by filename). Everything
alphabetically after that — `kubernetes_*`, later `bal2_*`, etc. — **never runs
in CI**. A green CI does NOT mean the full case suite passes. On every QA pass,
run the FULL suite locally (`cd diffctx && cargo test --release --test yaml_cases`)
and triage cases below the score threshold (default `min_score=10`, override via
`DIFFCTX_YAML_MIN_SCORE`). A failing case scores `100 * recall * (1 - forbidden_rate)`;
`forbidden_rate=100%` means the selection pulled in every "unrelated" manifest.

## Edge-Builder Regexes: Compile-Probe, Not Pipeline Coverage

The diff pipeline does NOT force every edge-builder `Lazy<Regex>` for small test
inputs (PPR can pull a fragment in via a structural edge without ever invoking
the k8s selector/label extraction). So a YAML case that "passes" does not prove
the edge regexes even compile. A nested bounded repetition of a Unicode negated
class — `(?:...[^\n:]{1,200}\n){1,50}` — compiles past regex's default 10 MiB
limit and `.unwrap()` aborts the process the first time it's forced (a real k8s
repo via the CLI hits it; the test suite did not). The deterministic guard is a
`#[cfg(test)]` probe that forces every regex in the module to compile
(`kubernetes::tests::all_kubernetes_regexes_compile`). Unbound the value class
(`[^\n:]+`) instead of bounding it — the regex crate is linear-time, so no ReDoS.

## Building the Extension: maturin, Not `cargo build --features python`

`cargo build --features python` fails at the link step (`linking with cc failed`,
undefined Python symbols) because the `extension-module` pyo3 feature expects the
host interpreter to provide symbols at import time. To compile-check the Python
bridge, use `.venv/bin/python -m maturin develop --release` (or `cargo build`
WITHOUT `--features python` / `cargo test --lib` for the pure-Rust paths).

## Empty-Diff Warning Is Expected on Docs-Only HEAD

`diffctx --diff` (bare, no range → defaults to HEAD) on a docs-only HEAD prints
`diffctx: diff produced no semantic context (clean working tree, binary-only, or
all files exceeded size cap); output empty.` and emits an 11-token YAML
skeleton with **exit code 4** (`_EXIT_EMPTY_DIFF`, since 065398c2 — was 0). Not
a regression — this is the actionable-error contract. CLI smoke check should
accept the warning, the empty `fragments:` list, and rc=4; do NOT treat rc=4
as a failure, and do NOT expect rc=0 on an empty diff result.

## SonarCloud IS Registered (project key `nikolay-e_TreeMapper`)

The applicability matrix previously said "not registered" — wrong, and it sat
uncorrected across at least one QA pass while every PR's `SonarCloud Code
Analysis` check quietly passed unread. The project key still carries the
pre-rebrand `TreeMapper` name; don't be thrown by the mismatch with the repo
name. Run the standard `/qa` skill SonarCloud step (Keychain token, HTTP Basic,
`projectKeys=nikolay-e_TreeMapper`) every pass — do not re-trust a stale
"N/A" note in this matrix without checking `gh pr view <N> --json
statusCheckRollup` for a `SonarCloud Code Analysis` entry first.

## `pythonsecurity:S8707` / `S8705` — Bulk False-Positive for This Project

New Sonar rule pair ("Agentic workflows should not be vulnerable to path/
argument injection") flags any subprocess/file-path call built from CLI-args,
framed around an LLM-orchestrator-with-adversarial-input threat model. For a
local CLI tool whose entire job is reading/writing whatever path the invoking
user names (diffctx itself), plus internal benchmark/dev scripts run directly
by a trusted local operator, there is no privilege boundary being crossed —
same category as `cat`/`cp`/`rm` taking a user-supplied path. Verified
repo-wide zero `shell=True`/`os.system`/`os.popen` (all subprocess calls use
safe list-argv form) before bulk-resolving 27 instances as `falsepositive` via
`POST /api/issues/do_transition` (see `/qa` skill SonarCloud section for the
token/auth pattern). Re-triage only if a NEW instance appears in code that
crosses an actual trust boundary (e.g. a future HTTP-exposed surface).

## Cognitive-Complexity Refactors Are Safe Mechanical Extractions Here

`benchmarks/*.py` has no dedicated unit tests for its analysis/report
functions (by design — no-unit-tests policy, and these are integration-style
CLI scripts). When cutting Sonar's `python:S3776` cognitive-complexity
findings there, verify behavior preservation via: `python -m py_compile` on
every touched file, the full `pytest -q` suite (covers `benchmarks.adapters`
indirectly), and `pre-commit run --all-files` (mypy strict + ruff + pylint
duplicate-code all catch a surprising fraction of extraction mistakes). Pure
extract-a-helper-function refactors (no logic change) are safe without
dedicated coverage; do not attempt behavior-changing "simplifications" in the
same pass.

## Bench Image (ghcr.io/nikolay-e/diffctx-bench)

- **A renamed ghcr package starts PRIVATE.** First push under a new name
  creates a fresh private package — the old package's public visibility does
  not carry over, and there is NO REST/GraphQL API to flip it (web UI only:
  package settings → Danger Zone). Symptom chain: cloud-init `docker pull`
  → "not found" (not "denied"), provision's verify step fails the same way.
  After any image rename, verify ANONYMOUS access:
  `curl -sf "https://ghcr.io/token?scope=repository:<owner>/<pkg>:pull"` →
  bearer GET `/v2/<owner>/<pkg>/manifests/latest` must be 200.
- **Every COPY source in Dockerfile.bench must be git-tracked.** A local
  untracked dir (stale `diffctx/python` build artifact) made the local buildx
  succeed while the same build failed in CI/bench-image. Guarded by
  `tests/test_bench_image_inputs.py`; keep it in sync with the Dockerfile.
- **`build_script | tail` masks the build's exit code** — the pipeline exit is
  tail's. Capture rc explicitly (`...; echo rc=$?` on the script itself) or
  `set -o pipefail` in the *calling* shell.
- The image is auto-published by `.github/workflows/bench-image.yml` on pushes
  touching its inputs; bench-sweep's provision job fail-fasts on a missing
  manifest BEFORE paying for the Hetzner server.

## Sweep Result Layout (post-#52)

- Full-sweep cells for ppr/ego/bm25 are multi-budget: artifact
  `cell-<method>-bALL-L<depth>-<test_set>` with per-budget checkpoints at
  `<test_set>_budget_sweep/b<budget>.checkpoint.jsonl` and summaries
  `cell_summary_b<budget>.json`. Aider stays one-budget-per-cell
  (`cell-aider-b<budget>-...`). `aggregate_sweep.collect_cells` expands bALL
  artifacts into one record per budget; legacy artifacts still parse.
- **Depth 0 is a real value, not "absent."** Any grouping key built as
  `cell.get("depth") or -1` coerces a legitimate `depth=0` cell into the -1
  ("depth doesn't apply") bucket, silently mislabeling every L0 row in the
  rendered tables as depth-less. Use an explicit `isinstance(d, int)` check
  (see `_depth_of` in `aggregate_sweep.py`) — same trap as any `or`-based
  default for a falsy-but-valid value (budget=0 has the identical shape;
  it currently escapes this because callers always compare `is not None`).
- **The `aggregate` job runs `if: always()` and uploads a real
  `sweep-aggregated-*` artifact even when the parent run is cancelled or
  partially failed.** Before manually re-aggregating from `cell-*`
  artifacts, check whether this artifact already exists — it's the
  authoritative one-shot summary of whatever completed before the failure.
- **`gh run download --pattern 'cell-*'` can silently under-fetch on a
  large artifact set** — a first pass grabbed 123/148 cells with no error;
  only comparing the downloaded count against the run's artifact list (or
  the official `sweep-aggregated-*` row count) surfaced the gap. Always
  cross-check `ls <dir> | wc -l` against `gh api .../artifacts --jq
  .total_count` (minus non-cell artifacts) before trusting a partial
  reaggregation.
- Self-hosted bench runners are started via bare `nohup ./run.sh &` in
  cloud-init — a host reboot (Hetzner-side infra event, not app-caused)
  kills them permanently and orphans queued cells forever with no retry.
  Tracked as its own fix (#98, systemd `svc.sh install`); until fixed, a
  sweep that stalls with jobs stuck `queued` for a long time (not just
  slow) means the host is gone — check `hcloud server list` / SSH before
  assuming the job queue is merely backed up.
- The full `yaml_cases` suite standing baseline: ~2262 passed / ~463 failed
  (`forbidden_rate=100%` over-selection class, tracked on #65). Gate in
  nightly-full-eval.yml is MIN_PASS_COUNT=2260 — compare against that, not
  against zero failures.

## Monitor Scripts in This Shell (zsh)

`status` is a read-only zsh special variable — a polling monitor script using
`status=$(...)` dies instantly with "read-only variable: status" (exit 1).
Use `run_state=`/`st=` in any Monitor/background snippet.

## Bash Tool cwd Does Not Reliably Persist Across Calls Mid-Session

During the real-world test-repo sweep, `cd test-repos/<repo>` in one Bash call
followed by `gh issue`/`gh pr` commands in later calls silently resolved
against whichever repo's `origin` remote happened to be the last `cd`-ed-into
directory — `gh issue view 65` returned a *closed react-native-navigation*
issue with the same number, not diffctx#65. This self-corrected on a later
call without any explicit `cd` back, so the tool's "cwd persists between
commands" guarantee is not reliable across a long multi-step QA session (some
other cwd-resetting event fires between calls). Mitigation: pass `-R
nikolay-e/diffctx` explicitly on every `gh issue`/`gh pr` call rather than
relying on directory context, especially in any step that also touches
`test-repos/*` clones. Verify with `pwd && git remote -v` before any
repo-context-sensitive command if in doubt.

## `gh issue view <N> --comments` Prints Nothing When There Are Zero Comments

Not a failure — `gh issue view 92 --comments` (an issue with `comments: 0`)
returned empty stdout with rc 0, easy to misread as the command having failed
or hit a transient error. Drop `--comments` (or check `comments:` in the
default view output) to confirm zero-vs-broken before retrying.

## Dependabot Squash-Merges via `gh pr merge` Land on GitHub Only, Not Forgejo

Forgejo (`origin`) is source of truth and push-mirrors to GitHub, but
`gh pr merge` operates against GitHub's API directly — the squash-merge commit
only ever exists on `github/main`, never on `origin/main`. If local `main` also
gained commits in the same session (e.g. QA-pass bug fixes), `git merge
--ff-only origin/main` succeeds (local is already ahead of origin) while
`github/main` has *diverged* (different squash commits on top of the same
base) — needs a real `git merge github/main` (not `--ff-only`), then push the
merge commit to **both** `origin` and `github`. Forgejo's branch-protection
flags the resulting merge commit as a rule violation ("branch must not contain
merge commits") but admin-bypasses it through — expected, not a failure to
chase. Prefer resolving via rebase over merge when the local/dependabot diffs
don't touch the same files, to avoid tripping that rule at all.

---

Generic QA patterns live in the `/qa` skill — do not duplicate here.
