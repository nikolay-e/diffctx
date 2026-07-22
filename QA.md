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
| SonarCloud | yes | Project key `nikolay-e_TreeMapper` (legacy pre-rebrand name — don't be thrown by the repo-name mismatch). Never re-trust an "N/A" note here without checking `gh pr view <N> --json statusCheckRollup` for a `SonarCloud Code Analysis` entry first |
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
- To compile-check the Python bridge use `.venv/bin/python -m maturin develop
  --release` — `cargo build --features python` fails at the link step
  (`extension-module` expects the host interpreter to provide symbols). For
  pure-Rust paths: `cargo build` WITHOUT `--features python` / `cargo test --lib`.
- Fast pipeline-debug cycle: env-gated `eprintln!` in Rust + `cargo build
  --release --bin diffctx` (~35s incremental); pay the maturin rebuild only when
  the Python surface is involved. Revert tracing before committing.

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

### Hypothesis Deadline Flakes Can Unmask Real Bugs

`tests/test_properties.py` property tests that write into a TemporaryDirectory
per example flake with `DeadlineExceeded` (200ms default) whenever a concurrent
cargo release build saturates the cores. Fix is `deadline=None` on the
I/O-bound tests (assertions untouched; deadline is a per-example perf
health-check, not a functional one) — precedent already in the file. After
silencing deadlines, RE-RUN the whole module: the freed example budget can
surface a genuine falsifying example the deadline abort was masking (this is
how the YAML block-scalar chomping bug was found; deterministic regression:
`test_yaml_roundtrip_trailing_newlines`).

## CLI Smoke Recipes

```bash
# Tree mode (CLI is also called `diffctx`):
diffctx src/diffctx/mcp --no-content -f yaml

# Diff mode, explicit range:
diffctx --diff HEAD~3..HEAD -f yaml

# Diff mode, bare --diff (defaults to HEAD):
diffctx --diff -f yaml
```

- Format flag is `-f / --format`, NOT `--output-format` (common typo).
- Empty-diff result (docs-only HEAD, binary-only, clean tree) is **rc=4**
  (`_EXIT_EMPTY_DIFF`) + a `diff produced no semantic context` warning + an
  ~11-token YAML skeleton. That is the actionable-error contract, not a
  regression — do NOT treat rc=4 as failure or expect rc=0 on an empty result.
- Local `which diffctx` trap (see `/qa` skill: Packaging QA): with this
  project's venv active, `.venv/bin` is first on `$PATH`, so bare `diffctx`
  runs the working-tree build. For QA code-review smoke always use
  `/Users/nikolay/.local/bin/diffctx`; tests/builds/pre-commit need the venv
  binary, only the user-facing smoke needs the pipx one.

## Pre-commit Caveats

See `/qa` skill: Packaging QA for `language: system` venv-shebang rot (recover
with `rm -rf .venv && python3 -m venv .venv && pip install "maturin>=1.10,<1.14"
&& pip install -e ".[dev,full,mcp]" --no-build-isolation`). CI is unaffected
(fresh venv per run).

diffctx-specific hygiene: stale `src/treemapper.egg-info/` from a rebrand-era
`pip install` is gitignored but may linger — delete on hygiene pass.

### Secret-Handling Test Fixtures Break the Secret Hooks

The private-key exclusion tests (`test_secret_ignores_diff.py`,
`test_default_ignores.py`) assert that diffctx drops key/keystore files. Both the
Rust `is_secret_path` and the Python `ignore.py` match **by filename only** —
fixture content is irrelevant to what they test. So fixtures must NOT embed a
literal PEM `BEGIN…PRIVATE KEY` banner: `detect-private-key` (no pragma support)
and `detect-secrets` both flag it, and a file committed past local hooks turns
`--all-files` red while a 20-case CI YAML subset stays green. Use inert content
(`"private-key-material <MARKER>\n"`) plus `# pragma: allowlist secret` for the
entropy detector; keep distinctive leak markers (`LEAK_RSA`, …) so leakage stays
detectable. High-entropy base64 findings come from concatenating tokens with no
separator — keep a space. Catch this class only with the FULL local suite:
`pre-commit run --all-files` (a staged-files commit run skips clean files). When
backgrounding it, the shell exit is the trailing `echo`'s — grep the log for
`Failed`, don't trust the reported exit.

## Diff-Mode Self-Eat

`diffctx --diff <range>` runs on this repo's own history. The tool is its own
test fixture. Use it during code review to surface the same semantic context an
external user would see.

**Do NOT dismiss a large output as "normal for a big commit" — measure it.** A
big *changed* diff is fine; a big *context* expansion is the over-selection bug
(#65/#59). Triage discriminators on every diff-review run:

- `role: "changed"` fragment count vs total fragment count;
- distinct files in the output vs `git diff <range> --stat` file count;
- **byte-share**: if the top files by rendered bytes are NOT in the changed
  set, that is #65-class over-selection even at a tame file ratio.

If most fragments are context and the output spans far more files than the
diff touched, that is over-selection, not a big diff. The full `yaml_cases`
suite quantifies the same bug: the large majority of its failures are
`forbidden_rate=100%` (pure over-selection), not recall misses. Fixing it is a
benchmark-validated recalibration coupled to the research paper — track on #65,
do not blind-edit edge weights mid-QA.

## Real-World Test-Repo Sweep (every QA round, MANDATORY)

`test-repos/` (git-ignored, local-only) holds ~27 real upstream clones across
many languages; `TOANALYZE.md` is the curated commit "todo" (SHAs verified),
and the clones carry live upstream HEADs. This is the dogfood that synthetic
`yaml_cases` can't replace: real diffs, real over-dump, real crashes.

**The 12 newer clones are `--filter=tree:0` partial clones** (`spark`,
`gitlab-foss`, `neovim`, `tokio`, `polars`, `aspnetcore`, `plausible`,
`firefox-ios`, `nextcloud`, `kubernetes`, `home-assistant`, `envoy`). Two
consequences: (1) git operations needing old trees/blobs lazy-fetch over the
network, so **wall-clock timing is NOT a clean diffctx signal there** — before
judging "slow/hang", re-run with a warmed object cache; a double rc=142 with
warm cache IS a real hang. (2) First diff runs need network; offline sweeps
should use the original full clones.

**Every QA round, sweep the test repos against a NEW commit each** (one not
exercised before — `git -C test-repos/<repo> pull` to fetch fresh upstream
history, then diff its newest commit). Run the **pipx** binary, not a venv
shadow:

```bash
cd test-repos/<repo>
git pull --ff-only
# Hard-cap runtime. `--timeout` (true wall-clock watchdog, exit 124; Python side
# is `_call_with_wall_clock_deadline` in src/diffctx/main.py — daemon worker +
# os._exit because a runaway pyo3 call cannot be cancelled) exists on dev builds
# of BOTH the standalone Rust binary and the Python CLI, but NOT in released
# 1.11.0. Until a release ships it, the perl cap is MANDATORY for pipx smokes
# (then `--timeout 200` replaces it). macOS has no `timeout`; use:
perl -e 'alarm 200; exec @ARGV' /Users/nikolay/.local/bin/diffctx . --diff HEAD~1
# (on the dev build, just pass `--timeout 200`.)
```

**A cap-hit IS the issue.** rc=142 (SIGALRM) or rc=137 (SIGKILL) with no output =
diffctx hung on that repo — file it and stop the sweep (don't run the remaining
giant repos like `linux`, they'll hang too and burn hours). Known hang repros:
`gitpod` / `pytorch` on a trivial HEAD~1, `aspnetcore` on a 17-file HEAD~1 —
all #70. **#70 is CLOSED (fix in dev, NOT in released 1.11.0)** — a hang on the
pipx binary in one of those repos is expected until a release ships the fix;
verify any new hang against the dev standalone binary
(`cargo build --release --bin diffctx`) before reopening #70 or filing new.
The #70 scope is broader than its title: the unbounded hang also fires on
**large diffs** (60–100+ files, e.g. onnxruntime / elasticsearch), not only
"large repo + trivial diff" — correlates with diff/graph size.

**Bash-tool timeout must exceed the perl alarm.** The Bash tool's default
120s kills the command BEFORE the 200s alarm fires (rc=143, looks like a
mystery kill). Pass `timeout: 260000` on every sweep invocation so the perl
cap stays the binding constraint and rc=142 keeps its meaning.

**Per repo, judge:** did it (a) finish without panic / non-zero exit / hang,
(b) honor the range — `changed_files` matches `git diff HEAD~1 --stat`, not a
whole-tree dump, (c) avoid gross over-selection (measure as in Diff-Mode
Self-Eat), (d) leak no secrets/garbage, (e) return in reasonable time. On any
non-empty diff also assert `grep -c 'role: "changed"'` ≥ 1 AND that the
output's max line range reaches the diff's changed lines (see #103 below).
Any of these failing = an issue.

**Stopping condition (the whole point):** sweep repos one at a time **until the
first issue**.

- On the first issue → `gh issue create -R nikolay-e/diffctx` with a generic,
  reproducible report (repo name, commit SHA / range, observed vs expected,
  token/fragment/file counts; **no sensitive repo contents**) → **stop the
  sweep** and move on to the rest of the QA round.
- If you get through **all** repos clean → also move on.

Either outcome — **all clean OR ≥1 issue filed** — is a valid completion of
this step. Do not let a found issue halt the round, and do not keep sweeping
after one is filed.

### Known #65 Repro Shapes (comment on #65, do not re-file)

- **Release/version-bump commits** (`chore: prepare vX.Y.Z`: CHANGELOG +
  README + manifest bumps — tokio, polars): 4–8 changed files expand to 60–70
  output files via prose/lexical edges into unrelated tests, while token cost
  stays deceptively modest (mostly stubs). Judge by file-level precision, not
  tokens.
- **Wide flat directory of peer files** (llama.cpp `src/models/`, ~100 peer
  .cpp files): a single-file change fans out via sibling/structural edges to
  60+ declaration stubs of the siblings. Any repo with a large flat
  plugin/model/handler directory reproduces it.
- **Byte-share facet** (neovim): moderate file expansion that the file-count
  discriminator under-rates — two UNCHANGED files pulled via symbol edges
  carried the largest fragments of the whole output (~37% of bytes). Hence the
  byte-share check in Diff-Mode Self-Eat.

### #103: EOF-Append to a Flat Data-List File Loses the Change Signal

A diff whose ONLY change is a few lines appended at EOF of a large flat
YAML/data-list file (elasticsearch `muted-tests.yml` class) can produce an
output with **zero `role: "changed"` fragments** — the changed lines never
surface — while the whole file is exploded into per-entry `definition` context
fragments. Two separable defects: (A) change signal lost (correctness — worse
than over-selection), (B) whole-file fragmentation of a flat list (overlaps #65).
Tracked on #103, primarily for (A). A clean rc=0 with a plausible token
count still hides this class — `role:`-absence is the tell.

### Whole-File-Chunk-as-Core Defect Class (#103/#105/#107)

When fragmentation yields no narrow fragment covering a hunk (flat data files,
non-tree-sitter languages, parse degradation), `find_core_for_hunk` falls back
to a whole-file chunk as the core; chunk kinds have no signature stub, so at
auto budget the core silently drops — change signal lost. The three issues are
one class; the sketched fix (hunk-window excerpt core) is on #103.
Selection-guarantee gaps of this shape are found fastest by comparing
`--budget 999999` output against auto-budget output.

### Confirmed-Correct Diff Shapes (don't re-file)

Probe **diff shapes**, not just `HEAD~1` — the shape is what breeds new bugs.
These are verified CORRECT; beware a naive sweep classifier flagging them:

- **Deletion-only / delete-heavy diffs**: output has **no `fragments:`**
  (content is gone) but a fully-populated **`deleted_files:`** list. Zero
  fragments here is correct, NOT #103 — consult `deleted_files` before
  flagging "empty output".
- **Binary-only diffs** (all `-\t-` in `git diff --numstat`): correct **rc=4**
  plus the `no semantic context` message. Confirm binary-only via numstat
  (`awk '$1=="-"&&$2=="-"'`) before treating rc=4 as a bug. BSD grep has no
  `-P` on macOS — don't use `grep -P '^-\t-'`.
- **Binary+text mixed**: binary ignored, text fragments clean, no
  mojibake/control-char leakage into YAML.
- **Large multi-file diffs that complete**: all changed files represented.

### Mini-Repro Pattern for Sweep Findings

A hand-written synthetic file may NOT reproduce a real-repo finding (a
synthetic flat YAML list was clean while the real `muted-tests.yml` reproduced #103
— tree-sitter parse degradation depends on exact content like `{...}`
flow-scalar braces). Extract the real file instead:
`git -C test-repos/<repo> show <rev>:<path> > file` into a fresh throwaway
repo, commit base + change, then iterate there — seconds per run instead of
minutes on the full clone.

## Real-World Quality Benchmark (`benchmarks/real_world_diff_bench/`)

Beyond "does it crash": a committed 109-commit benchmark scoring the QUALITY
of extracted context against gold `should_include`/`should_not_include` labels
(hand-labeled over react-native/gitpod/sentry real commits). Re-score after
any selection change with `scripts/score.py` against the frozen
`gold_labels.json` (gold labels are the stable contract, diffctx's rendered
file set is the variable). The score formula `100·recall·(1−forbidden)` does
NOT penalize over-selection — add a precision/F1 term before trusting a high
score. Feeds issues #65 (over-selection), #70 (hang), #103 (lost-change)
and #104 (MD default).

## YAML Cases: CI Runs Only the First 20

`ci.yml` sets `DIFFCTX_YAML_CASES_LIMIT: "20"`, so `cargo test --test yaml_cases`
in CI runs only the first 20 discovered cases (sorted by filename). Everything
alphabetically after that **never runs in CI** — a green CI does NOT mean the
full case suite passes. On every QA pass, run the FULL suite locally
(`cd diffctx && cargo test --release --test yaml_cases`) and triage cases below
the score threshold (default `min_score=10`, `DIFFCTX_YAML_MIN_SCORE` to
override). Standing baseline: ~2262 passed / ~463 failed (the
`forbidden_rate=100%` over-selection class, tracked on #65); the
nightly-full-eval.yml gate is `MIN_PASS_COUNT=2260` — compare against that,
not against zero failures.

## Edge-Builder Regexes: Compile-Probe, Not Pipeline Coverage

The diff pipeline does NOT force every edge-builder `Lazy<Regex>` for small
test inputs (PPR can pull a fragment in via a structural edge without ever
invoking the k8s selector/label extraction). So a YAML case that "passes" does
not prove the edge regexes even compile. A nested bounded repetition of a
Unicode negated class — `(?:...[^\n:]{1,200}\n){1,50}` — compiles past regex's
default 10 MiB limit and `.unwrap()` aborts the process the first time it's
forced. The deterministic guard is a `#[cfg(test)]` probe that forces every
regex in the module to compile
(`kubernetes::tests::all_kubernetes_regexes_compile`). Unbound the value class
(`[^\n:]+`) instead of bounding it — the regex crate is linear-time, no ReDoS.

## SonarCloud — diffctx specifics

Key `nikolay-e_TreeMapper`; CI-integrated (scans on each `diffctx CI` run), so
the fix loop is: push → wait for that run → re-fetch issues → mark remaining
FPs → gate. Run the standard `/qa` SonarCloud step every pass.

- `benchmarks/**` scripts are analyzed like any other source — a new
  `.sh`/`.py` there can flip the gate to ERROR on its own.
- **`python:S7494` and `python:S7500` are mutually contradictory** on the same
  code (`dict(genexpr)` ↔ dict comprehension). Do NOT ping-pong — rewrite as
  an explicit `for` loop (with `with open(...)`), which satisfies both.
- **`pythonsecurity:S8707`/`S8705`** (path/arg injection) are a bulk
  false-positive class for THIS project: a local CLI tool whose entire job is
  reading/writing whatever path the invoking user names crosses no privilege
  boundary (same category as `cat`/`cp` taking a user path). Verified
  repo-wide zero `shell=True`/`os.system`/`os.popen` (all subprocess calls are
  list-argv) before bulk-resolving as `falsepositive` via
  `POST /api/issues/do_transition` (taint rules ignore NOSONAR), with a
  one-line comment naming the trust boundary. Re-triage only if a NEW instance
  crosses an actual trust boundary (e.g. a future HTTP-exposed surface).
- `python:S5443` (hardcoded `/tmp` default) → drop the default, require the
  dir as an explicit CLI arg. Shell scripts trip `shelldre` basics
  (`[[ … ]]`, local-var + explicit `return`) — cheap mechanical fixes.
- Cognitive-complexity (`python:S3776`) refactors in `benchmarks/*.py` are
  safe as PURE extract-a-helper moves: verify via `python -m py_compile`, the
  full `pytest -q` suite, and `pre-commit run --all-files`. Do not mix in
  behavior-changing "simplifications".

## Bench Image (ghcr.io/nikolay-e/diffctx-bench)

- **A renamed ghcr package starts PRIVATE.** The old package's public
  visibility does not carry over, and there is NO API to flip it (web UI only:
  package settings → Danger Zone). Symptom: `docker pull` → "not found" (not
  "denied"). After any image rename, verify ANONYMOUS access:
  `curl -sf "https://ghcr.io/token?scope=repository:<owner>/<pkg>:pull"` →
  bearer GET `/v2/<owner>/<pkg>/manifests/latest` must be 200.
- **Every COPY source in Dockerfile.bench must be git-tracked** — a local
  untracked dir makes local buildx succeed while CI fails. Guarded by
  `tests/test_bench_image_inputs.py`; keep it in sync with the Dockerfile.
- `build_script | tail` masks the build's exit code — capture rc explicitly or
  `set -o pipefail` in the calling shell.
- The image is auto-published by `.github/workflows/bench-image.yml` on pushes
  touching its inputs; bench-sweep's provision job fail-fasts on a missing
  manifest BEFORE paying for the Hetzner server.

## Sweep Result Layout (post-#52)

- Full-sweep cells for ppr/ego/bm25 are multi-budget: artifact
  `cell-<method>-bALL-L<depth>-<test_set>` with per-budget checkpoints at
  `<test_set>_budget_sweep/b<budget>.checkpoint.jsonl` and summaries
  `cell_summary_b<budget>.json`. Aider stays one-budget-per-cell.
  `aggregate_sweep.collect_cells` expands bALL artifacts into one record per
  budget; legacy artifacts still parse.
- **Depth 0 is a real value, not "absent."** `cell.get("depth") or -1` coerces
  a legitimate `depth=0` into the -1 bucket — use an explicit
  `isinstance(d, int)` check (see `_depth_of` in `aggregate_sweep.py`). Same
  trap for any falsy-but-valid value (budget=0 has the identical shape).
- **The `aggregate` job runs `if: always()`** and uploads a real
  `sweep-aggregated-*` artifact even when the parent run is cancelled or
  partially failed — check for it before manually re-aggregating from
  `cell-*` artifacts.
- **`gh run download --pattern 'cell-*'` can silently under-fetch** on a large
  artifact set with no error. Cross-check `ls <dir> | wc -l` against
  `gh api .../artifacts --jq .total_count` (minus non-cell artifacts) before
  trusting a partial reaggregation.
- Self-hosted bench runners are started via bare `nohup ./run.sh &` in
  cloud-init — a host reboot kills them permanently and orphans queued cells
  with no retry (#98, systemd `svc.sh install` is the fix). A sweep stuck with
  jobs `queued` for a long time means the host is gone — check
  `hcloud server list` / SSH before assuming the queue is merely backed up.

## Concurrent-Session Hazard: /qa While Another Session Develops in This Checkout

If `git status` shows substantive modifications you didn't make: (1) STOP
before any commit/revert — check mtimes (`stat -f "%m %Sm %N"`) and
`ps aux | grep claude` to confirm a live concurrent writer; (2) never revert
or blanket-`git add` — stage YOUR files by explicit path only; (3) treat all
local cargo/pre-commit results as invalid for HEAD (they compiled the other
session's half-written code) — CI on the pushed commit is the only honest
verifier; (4) skip/void the full local `yaml_cases` run for the round and say
so in the report. A commit whose hook run timed out mid-restore may still have
landed — verify with `git log` before retrying, don't double-commit. Root fix:
parallel work belongs in `git worktree`s (workspace convention), one session
per checkout.

## Default Output Is Markdown (post-#104) — Grep Traps

Without `-f yaml`, both CLIs emit Markdown: files are `## \`path:lines\``
headers, roles are `— **changed**`, and`- path:`/`role: "changed"` greps
silently return 0. Either pass `-f yaml` in sweep/self-eat recipes or count
with `grep -cE '^## '` and `grep -c '\*\*changed\*\*'`. A "0 fragments" result
from a YAML-shaped grep over MD output is a tooling artifact, not a finding.

## Mass-Added-Data Diffs Hang Both Binaries (#121)

A range dominated by newly ADDED plain-text data files (the dcbench instance
import: ~372 `patch.diff` files, ~39 MB) exceeds even the dev binary's 600s
watchdog — GenericStrategy fragments every added data file and the graph goes
near-dense (#116 mechanism) before any budget applies. For self-eat reviews of
such ranges, run code-only subranges; the data-file-policy gap is tracked
on #121 (overlaps #112).

## tiktoken Re-Encoding of diffctx Output Needs disallowed_special

Fragment contents from real repos can contain literal special-token text
(`<|endoftext|>`). Any script that re-tokenizes diffctx output with tiktoken
must pass `encode(text, disallowed_special=())` or it dies mid-batch.

## Generated API-Dump Files Are a Lockfile-Class Cost Sink (#105)

Gradle/Kotlin-multiplatform repos running binary-compatibility-validator commit
`*.api` / `*.klib.api` dumps. A public-API change touches a handful of lines in
them, but the chunk fallback renders 200-line chunks: ktor `a400de87` turned 9
changed lines into 57 KB (35% of the whole output). Correctness holds (hunks land
inside the rendered chunks, `role: "changed"` present) — it is purely the #105
granularity cost, same policy class as the lockfiles on #112. Treat any
`*.api`-heavy diff as a #105 repro, not a new finding.

## LEVERAGE.md Is Lint-Gated Like Any Repo Markdown

`/review-leverage` writes `LEVERAGE.md` at prose width, but the repo's
markdownlint hook enforces MD013 at 80 columns repo-wide, so an uncommitted
audit log blocks the next `git add -A` commit. Reflow before committing (wrap
prose only — tables and fenced blocks are already exempt via `.markdownlint.yaml`).
Note the trap when checking width: `awk 'length > 80'` counts BYTES, so the
`·`/`→`/`—` characters these logs are full of produce phantom violations —
markdownlint counts characters. Verify with `pre-commit run markdownlint --files
LEVERAGE.md`, not with awk.

## dcbench Annotation Scripts: `--single` Rebuilds the Whole Project Graph

`annotate_hops.py --single <inst>` on an instance whose gold entries lack `hop`
builds the full project graph of a large repo — minutes, or a #116-class hang.
Only the already-annotated path returns fast (`skip (done)`). For a smoke test of
these scripts, pick an instance that is already done, and exercise the failure
path with a bogus `--repos-root` instead of waiting on a real graph build.

## Pre-commit False-Fail from Concurrent Background Writers

A hook reported as `Failed ... files were modified by this hook` can mean a
concurrent process (own background measurement writing artifacts) touched the
tree mid-run, not that the hook found anything. Re-run `--all-files` when the
tree is quiet before treating it as a finding.

## Sonar docker/githubactions Rule Family (2026-07 activation)

`docker:S85xx`/`githubactions:S85xx` (pip `--only-binary`, hash-locking,
curl|sh) activated as a family and instantly produced 40+ findings + gate
ERROR. Resolution split: Dockerfile.bench = real supply-chain surface → fix
properly (version+sha256-pinned rustup-init, `--require-hashes` on the lock,
`--only-binary :all:`, exact-pinned uv/maturin); workflow findings on
ephemeral CI runners building our own artifacts (incl. `pip install -e .`,
which cannot be hash-locked) → bulk `accept` transition with a rationale
comment. bench-image CI verifies `--only-binary` compatibility of the lock.

Sonar keeps doing this: a rule activation lands findings on code nobody touched,
so the verification-loop re-fetch after a push surfaces issues that were not in
the initial pass (2026-07: `python:S8997` on `tests/test_tokens.py`, manual
`sys.stderr` swap → `capsys`). Budget for one extra loop iteration whenever the
first Sonar fetch is non-empty, and never treat a clean initial fetch as proof
the post-push fetch will be clean.

## Rust Binary vs Python CLI Render Differences (grep traps)

The standalone Rust binary (`diffctx/target/release/diffctx`) renders YAML
plainly (`role: changed`, `lines: 1-791`), while the Python CLI quotes
(`role: "changed"`, `lines: "1-791"`). A `grep 'role: "changed"'` against
standalone output silently reports 0 — use `grep -E 'role: "?changed"?'` in
any check that may see either producer. Also: the standalone binary's clap
parser rejects `--budget -1` (`use '-- -1'`); pass a large N instead.

## Harness / gh Traps Seen Here (fleet-generic — candidates for `~/.claude/qa-refs/`)

- Bash-tool cwd does not reliably persist across calls mid-session: after
  `cd test-repos/<repo>`, later `gh issue`/`gh pr` calls can resolve against
  the wrong repo's `origin` (a same-numbered foreign issue). Pass
  `-R nikolay-e/diffctx` explicitly on every `gh issue`/`gh pr` call in any
  step that also touches `test-repos/*`; `pwd && git remote -v` when in doubt.
- `gh issue view <N> --comments` prints NOTHING (rc 0) when the issue has zero
  comments — not a failure; check `comments:` in the default view first.
- `status` is a read-only zsh special variable — a monitor script using
  `status=$(...)` dies with "read-only variable". Use `run_state=`/`st=`.
- Dependabot squash-merges via `gh pr merge` land on **GitHub only** — Forgejo
  (`origin`) is source of truth and push-mirrors outward, so the squash commit
  never exists on `origin/main`. If local `main` also gained commits,
  `github/main` diverges: needs a real `git merge github/main` (not
  `--ff-only`), pushed to **both** remotes; Forgejo branch-protection flags
  the merge commit but admin-bypasses it — expected. Prefer rebase over merge
  when the diffs don't overlap, to avoid tripping that rule.

---

Generic QA patterns live in the `/qa` skill — do not duplicate here.
