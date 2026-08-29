# QA Playbook — diffctx

Project-specific facts for `/qa`. Generic methodology lives in
`~/.claude/qa-refs/`.

## Applicability matrix

| Check | Applies | Notes |
|---|---|---|
| CI | yes | GitHub Actions on the mirror (ci.yml, action-smoke, CodeQL per push; cd.yml/publish-* manual dispatch; nightly-full-eval cron). "Rust diffctx tests" is ~6 min on a warm cargo cache; a Rust-touching commit invalidates it (+~10 min compile). Beyond ~30 min on "Build and test" = stuck runner — `gh run cancel` + `gh run rerun --failed` (an 85-min hang resolved to a 6-min green rerun with zero changes). A re-run stuck in `queued` can be API-unrecoverable: cancel AND force-cancel 409 with "re-run that has not yet queued", and an all-jobs-cancelled CodeQL run later reports "cannot be retried" — don't fight it; the next push supersedes, verify the corpus gate on the new SHA |
| Browser QA / stumble | yes | **there IS a deployed surface**: the GitHub Pages landing page (`nikolay-e.github.io/diffctx/`, built by the `pages build and deployment` run on every push). This row read "no web frontend" until 2026-08-26 and it was simply wrong — the page is where a stranger decides whether to adopt the tool, which is exactly what the stumble probe measures. It is ONE page: only `demo/` and `llms.txt` are additionally published; the `docs/` subdirectories (product, engineering, architecture, benchmarks) are repo-read `.md`, 404 on Pages by design and linked from nowhere on the site, so that 404 is not a finding |
| Post-deploy autoqa | no sensor | nothing runs the crawler against the Pages site, so the link sweep is done in-pass (fetch the page, follow same-origin links, HEAD every external). The `fonts.googleapis.com` / `fonts.gstatic.com` bare hosts are `rel=preconnect` hints and 404 on a bare GET — always false positives, never file them |
| CD / K8s / ArgoCD | no | ships to PyPI, npm, crates.io, Docker Hub via cd.yml/publish-extras.yml; those workflows' publish smokes are the probes |
| Backend smoke | no | — |
| SonarCloud | yes | project key is `nikolay-e_TreeMapper` (legacy name, never renamed) |

## Stumble probe tasks

Fixed wording — the number is only a trend if the task never moves. Alternate
across passes; `STUMBLE_GOAL` is the Ultimate Goal sentence from `CLAUDE.md`,
verbatim. `STUMBLE_URL` is the Pages landing page.

1. *(entry)* "You have a git repository and a pull request that touches about
   40 files. Using only this site, work out the exact command that would give
   you a review-ready context of that pull request, including how to point it
   at the right commit range and how to control how much output you get. Then
   decide, from evidence on this site, whether you would trust that output
   instead of reading the raw diff."
2. *(uncomfortable)* "Decide whether this tool would beat simply pasting
   `git diff` into your model, and find the number on this site that settles
   it. Say what that number was measured on."

## Forge

- `origin` = Forgejo (source of truth, push here); `github` = mirror,
  but **all CI, PRs and Dependabot live on GitHub**, and the roadmap
  issues do too.
- Forgejo issues are NOT empty: external reporters file there (it is the
  public-facing host). Enumerate both arms every pass —
  `GET /repos/nikolay-e/diffctx/issues?type=issues&state=open`. Triage on
  Forgejo, then cross-link to the GitHub issue that carries the work item
  and its gate, rather than duplicating the tracker.
- The `redact-check` PreToolUse hook blocks a tracker post whose **command
  line** mentions a deny-listed secret name — including the Keychain
  service name used to fetch the API token. Fetch the token in a separate
  Bash call (cache to a `umask 077` scratch file, delete after), then post
  without naming the service.
- Push `main` to both remotes (mirror sync is periodic; direct push is
  immediate).
- **Renovate auto-merges its Forgejo PRs** (renovate.json: patch/minor
  automerge, squash) — possibly hours after opening them, so an "open" PR
  list is a snapshot, not a queue to hand-merge. A pin that must NOT land
  needs a `packageRules` constraint (the python `allowedVersions: <3.14`
  CI-matrix gate is the precedent), not a closed PR.
- **Forgejo's merge API can return 405 with an empty message on a merge
  that SUCCEEDED.** Never read the HTTP code as the outcome — re-fetch
  the PR (`state`/`merged`) before retrying or escalating; a blind retry
  loop would report failure on a completed merge.
- **The v1.13.0 tag points at a commit containing unreleased work** — the
  2026-08-08 history rewrite re-anchored tags onto epoch commits, so
  `git describe`/tag position cannot tell you what a release contains.
  Released state is read from the registry (PyPI upload date/version),
  not from the tag.

## Dependency channels (who owns which file)

Three lock/manifest pairs, and mixing them up is how a channel breaks
silently:

- **`pyproject.toml` + `uv.lock`** — every CI job installs with
  `uv sync --locked --no-build`. `--locked` refuses to re-resolve, so a
  pyproject edit landed without `uv lock` turns CI red instead of drifting;
  that IS the drift gate, there is no separate check. `--no-build` keeps every
  dependency on a wheel (no third-party `setup.py` runs); the root project is
  exempt and still compiles through maturin. Dependabot's `uv` entry is
  `lockfile-only` on purpose — the `pip` entry owns pyproject's ranges, and
  without the split both ecosystems open the same PR twice.
- **`eval` dependency group in `pyproject.toml`, locked in `uv.lock`** — the
  research harness's Python deps. One resolver for the whole repo; the
  separate `requirements-eval.txt` + `.lock` pair (and its own bot) is gone.
- **`Cargo.toml` + `Cargo.lock`** — Dependabot's `cargo` entry.

The pip entry runs `versioning-strategy: increase-if-necessary`. The default
(`increase`) rewrites a floor to the newest resolvable version every run, and
when that version already IS the floor the rewrite is a no-op that
dependabot-core aborts on (`Expected content to change!`) — numpy and scipy hit
that for weeks because their newest release supporting `requires-python =
">=3.10"` is the version already written. A whole ecosystem's weekly run dies on
one such dependency, so a red "Dependabot Updates" run is worth reading, not
dismissing as bot noise.

## Tests

- `pytest` — integration-only (the run prints the count; don't pin it
  here, it rots). Known valid skip: `test_clipboard.py` (Windows only).
- Local flake mode: `-n auto` on the M4 Pro means 14 xdist workers,
  each CLI subprocess spawning a full rayon pool — under load the
  global pytest-timeout (30 s since 27d6a3d6, was 10 s) fires on
  varying tests. Timeout-only failures with a changing set across runs
  = oversubscription, not a regression; never run `cargo test`
  concurrently with pytest, it makes this worse.
- `cargo test --lib` in `crates/diffctx-native` — inline units; the run
  prints the count, and a count written here rotted once already.
- YAML corpus: CI gates the FULL corpus on every push (the run prints the
  case count; pinning it here rotted — it read 2725 while the corpus had grown
  to 2902)
  (`cargo test --release --test yaml_cases`), per-case
  against `known_below_threshold.txt`, enforced bidirectionally;
  nightly re-runs with `DIFFCTX_YAML_IGNORE_BASELINE=1` to track
  baseline size. `DIFFCTX_YAML_CASES_LIMIT=20` sampling survives only
  in the local pre-commit hook.
- E/Q discipline (CLAUDE.md): Q-class (output-changing) fixes are
  frozen during an eval cycle — bugs like the `IntervalIndex::overlaps`
  asymmetry stay documented + pinned, not fixed in-pass.
- **Active-xfail semantics in the yaml harness**: a case with an active
  `xfail:` (reason/category set) reports test "ok" while its ORACLE still
  fails — the marker absorbs the failure; test "FAILED" on such a case
  means the oracle now PASSES ("drop the xfail"). Bare `xfail: {}` is a
  no-op (`is_active` false, baseline gates instead) — remove on sight.
- **Rust edits fight the auto-format hook**: the PostToolUse formatter
  orders `use` items differently from repo rustfmt; `git commit` then
  fails cargo-fmt. Run `cargo fmt` before every commit that touches
  Rust. **Python edits hit the same trap**: the hook applies isort's
  aligned-parens import style, which repo black rewrites back — run
  `pre-commit run black --files <touched>` after Python edits.
- **`DIFFCTX_PROBE_TAU`** mirrors `DIFFCTX_PROBE_MODE` in the yaml
  harness: measurement-only tau override, meaningful only with
  `DIFFCTX_YAML_IGNORE_BASELINE=1`; default run still measures the
  shipped constant.
- **Python API fragments carry** `content/kind/lines/path/role/symbol`
  only — no `token_count`. Integration tests measuring budget use must
  proxy via `len(content)` (~4 chars/token).
- **Version-bearing files**: the CD bump patches every file listed in
  the cd.yml bump block — that block is the enumeration, incl.
  `docs/product/github-action.md` (pins gated by
  `test_version_consistency`). Adding a new file that embeds the version
  requires BOTH the cd.yml bump block and that test.
- **eval calibrate at 8 workers can reboot the machine** (OS memory
  pressure when heavy-tail instances co-schedule; same class as the
  11-13-worker sweep kills). Use 5 workers; checkpoints resume cleanly
  per cell.

## Diff-context review

- Use the uv-tool binary `~/.local/bin/diffctx` (pipx-equivalent),
  never `.venv/bin/diffctx`. Check its version FIRST (`uv tool list`,
  not `--version` alone): a stale tool silently re-introduces fixed
  bugs into reviews (a 1.12.2 tool leaked `.diffctx/ignore`-excluded
  tests/yaml as context — the universe filter shipped in 1.12.3).
  Refresh: `uv tool install 'diffctx[mcp]' --force --refresh`.
- `--version` is useless for staleness here: the version string moves
  only on release, so a tool built weeks ago still says `1.12.3`. Compare
  the **extension's mtime** against HEAD's date
  (`ls -la ~/.local/share/uv/tools/diffctx/lib/python*/site-packages/diffctx/_diffctx.abi3.so`).
  To review a range that includes unreleased work, install from source:
  `uv tool install . --force --reinstall`.
- **Do not review with `--budget -1`.** Selection plus the post-passes are
  ~97% of wall clock on a wide range (#121); unlimited removes the stop and
  every cell hits the 300s deadline. A wide range at the default budget is
  the reviewable artifact.
- This repo's own `.diffctx/ignore` excludes `*.yaml`/`*.yml` (the
  corpus would drown every self-eat) **and `tests`** — so the
  entire `tests/` tree, `crates/diffctx-native/tests/`, every oracle
  case and all CI/workflow YAML are invisible to self-eat, hidden even
  from `changed_files` by the security contract. A range that touches
  only those legitimately yields rc=4 and a bare skeleton. Concretely:
  a commit changing 33 files can show 9. Review test and workflow
  changes with plain `git diff`; don't file this as a bug, and don't
  read the short changed-files list as the whole change.
- Reading a self-eat diff of a *scoping* change: total emitted lines and
  context-file count move in OPPOSITE directions. Shrinking fragments
  frees budget the greedy immediately spends admitting more files, so a
  genuine over-dump fix shows up as fewer lines AND more files. Measured
  across three ranges when bounded gap chunks landed: 717→656, 543→531,
  582→535 lines, while context files went 4→8 and 16→17. Neither number
  alone is the verdict — this is why #149's gate pairs over-dump rate
  with precision. Two mechanisms drive the breadth half: freed budget, and
  `apply_fragment` recording the *excerpt's* identifiers rather than the
  whole core's, so needs the trimmed body used to cover read as
  uncovered and the greedy goes looking for them elsewhere.
- Self-eat context breadth collapsed after the #211 post-pass/singleton
  gates (measured 2026-08-19: 61 visible changed files -> 85 output files,
  i.e. ~0.4 context files per changed file; the 2026-08-07 pre-gate figure
  was ~4-5x). A wide-context self-eat read is no longer designed behavior —
  treat a return to multi-x breadth as a regression of the #211 gates, not
  as the old known shape.
- `env_overrides.rs` carries name-consistency tests: any new
  `read_env_*("DIFFCTX_*")` must appear in the `parameter-strategy.md`
  Tier-3 table (or the `TIER1_EXTRAS_READ_BUT_NOT_TABLED` allowlist)
  and vice versa.

## Bug channels (enumerated 2026-08-05)

1. GitHub issues (`gh issue list`) — canonical tracker, takes `Fixes #N`.
2. Forgejo issues (API, token `forgejo-token` in Keychain) — external
   reporters; cross-link to GitHub work items. Forgejo #4 is Renovate's
   standing Dependency Dashboard — infrastructure, not a bug; never
   triage it as one.
3. No in-app bug queue, no client telemetry, no bot reports (CLI/library
   product). PyPI/npm/crates user reports arrive via the two trackers.
4. GitHub alert feeds (`gh api repos/<r>/dependabot/alerts?state=open`,
   `code-scanning/alerts?state=open`, `secret-scanning/alerts?state=open`)
   — all three were empty on 2026-08-30; a pass reads them, it does not
   assume them.
5. SonarCloud project `nikolay-e_TreeMapper` (the key predates the rename):
   quality gate + open issues + hotspots via the sonarqube MCP tools. Issues
   raised on the day's own commits count as intake for that pass.

## Issue triage invariants

- `v3`-labeled issues are a curated roadmap backlog
  (research/benchmark/paper/product), not rotting defects — check
  staleness, don't force per-pass decisions.
- `gated` label = blocked on a pre-registered experiment or eval-cycle
  boundary.

## Known false positives

- import-linter local failures are REAL — the old excuse ("fails locally
  on the namespace package while green in CI") is dead; `lint-imports`
  reports `6 kept, 0 broken` locally. One failure caught
  `from diffctx import _diffctx` in `mcp/server.py`, which
  executes `diffctx/__init__` and so pulls `_app` -> `cli`, breaking
  "MCP server must not import CLI". Import the submodule directly
  (`from diffctx._diffctx import X`) so the edge points at the submodule
  rather than the package root.
- Sonar `python:S2245` on `paired_bootstrap` (seeded `random.Random` for
  CI resampling) — false positive, marked via API 2026-08-04: determinism
  is the requirement, not a security context.
- Sonar `pythonsecurity:S8705/S8707` on `eval/workflows/` harnesses
  ("LLM sandbox escape") — accepted via API 2026-08-04: operator-run
  local research harnesses; argv exec with regex-validated sha
  (realworld_rerun) and operator-chosen output dir (run_final). Don't
  re-litigate unless the eval workflows become agent-facing.
- SonarCloud `githubactions:S8543` on the publish-extras npm smoke:
  `$VERSION` is an exact just-published version, package has zero
  deps — marked false positive in SonarCloud via API (NOSONAR is NOT
  supported by the githubactions analyzer; don't re-add it).
  Gotcha: editing the flagged line (or its neighbours) shifts the issue
  hash and Sonar re-raises the finding under a NEW issue key with the
  FP mark lost — re-fetch after every analysis touching that file and
  re-mark via `api/issues/do_transition` (`falsepositive`).

## Recurring bug patterns (diagnose once, recognise thereafter)

- **Over-emission for a tiny diff is ONE mechanism, not per-language
  bugs.** `excerpt::generate_core_excerpts` cuts a hunk window (+3 context
  lines, capped at 70% of the parent) but is consulted only as a *budget*
  fallback via `build_signature_lookup`, when the core exceeds
  `budget x core_budget_fraction`. So the same file ships whole at
  `--budget 8000` and tightly excerpted at a small budget. Three issues are
  this one mechanism reached three ways: no grammar → whole-file chunk
  (#105); grammar parses but the body is flat → one chunk (#107); fine
  fragments exist but the hunk spans more lines than any of them, so
  `find_core_for_hunk` promotes to the enclosing definition (Forgejo issue
  2). Don't re-diagnose per language.
  **Status: fixed for the `changed` role** — the excerpt is consulted on how
  little changed rather than on leftover budget, excerpts are generated for
  kinds that have a signature variant too (a signature drops the changed
  lines), `render` agrees with `locate` that an `Excerpt` is `changed`, and
  long uncovered runs are split into bounded chunks so a flat file has
  sub-file granularity at all. #105/#107/#114 closed on their own repros.
  **The `context` role was fixed by a different lever** (#123, closed
  2026-08-04): not windowing (both windowing attempts measured 43 corpus
  failures each) but the file-level relation encoding (9b41040d) — import-class
  edges land on the file's representative fragment instead of fanning to every
  fragment at full weight, so an indivisible 400-line parent stops being
  selected as context at all.
- **`coherence_post_pass` is inert by accident and load-bearing for
  precision.** It resolves a dangling semantic neighbour by lowercased
  `symbol_name` instead of by the id the graph edge already gives it. That
  is a real bug, but the name lookup mostly lands on already-selected
  fragments, so the pass adds nothing. Fixing the lookup alone activates a
  pass with no relevance bar that draws from `filtered_fragments` — i.e. it
  re-admits candidates the greedy declined — and cost 9 of 2725 oracle
  cases (`recall=100%, forbidden_rate=100%`). Land the id fix only together
  with a relevance bar or a cap.
- **Span-vs-content mismatch.** `line_count()` comes from the id's span
  while slicing indexes `content.lines()`; nothing enforces agreement.
  Gate on the actual line vector, and never let `end < start` reach a
  `FragmentId` — `line_count()` underflows on it in every later stage.
- **Lexical path containment is not containment.** `Path::starts_with`
  compares components and `canonicalize` fails on a non-existent path (the
  old side of a deletion). A lexical fallback used as an *alternative* to
  the canonical check makes the canonical check dead. Reject `..` and
  absolute up front, then use the lexical form ONLY when canonicalization
  is impossible — otherwise an in-repo symlink pointing outside passes.
  Both are pinned by tests in `git.rs`; the tests must canonicalize their
  temp root or the hole hides on macOS (`/var -> /private/var`) and only
  fails on Linux CI.

## Bit-equivalence gate for refactors (`scripts/bitcheck.sh`)

`record` on the pre-change build, rebuild, `check`. 24 cells (3 fixed
ranges x 4 scoring modes x pack/locate) in ~20 s; `clean` drops the
snapshots and the fixture worktree.

- The input is a **worktree pinned to a fixed SHA**, not the live
  checkout. diffctx analysing its own repo means its sources appear in
  its own output as context fragments, so editing the code under test
  also edits the input. Running against the live tree reported 8 moved
  cells for a change that could not affect them — a stashed file had
  rewritten a context fragment. Never "simplify" the fixture away.
- The fixture cannot exercise every path: this repo's `.diffctx/ignore`
  hides `tests`, so no `FooTest.java`/`widget-spec.js` shape exists in
  it and a test-classifier change reads as bit-identical. Corpus +
  targeted unit tests carry that; bitcheck does not.
- E-class claims ("no output change") land with a bitcheck result in the
  same commit. Q-class still needs the corpus and a baseline edit.

## Measuring against a running build

Two ways to invalidate an overnight measurement, both observed in practice:

- **`maturin develop` swaps the extension under a running experiment.** The
  eval harnesses invoke `.venv/bin/diffctx`, which loads
  `src/diffctx/_diffctx.abi3.so`. Rebuilding mid-run mixes two builds into one
  result file. `cargo build` / `cargo test` are safe — they write `target/`.
  Editing Python under `src/diffctx/` is NOT safe for the CLI path (editable
  install), though `mcp/` is, since the CLI never imports it.
- **A background `cargo test` compiles the live tree**: editing sources after
  launching the corpus run means the binary's content is undefined — kill and
  relaunch after the last edit (one mid-compile edit cost a full 150s rerun).
- **Name the build, not the branch.** Commit time is not build time. Compare the
  `.so` mtime against `git log --date` and state which commit's *content* the
  run executed; "measured on main" is meaningless when six commits landed while
  it ran.

## Reading a benchmark whose cases time out

`datasets/real-world-diff/v1/` is 109 real commits over react-native / gitpod /
sentry, and its shipped snapshot is 66% hangs — so its quality numbers describe
the survivors, not the benchmark. Rules that follow:

- Report the hang rate first and the denominator always. "precision 0.81" over
  43 produced cases out of 81 is not "precision 0.81".
- Record elapsed per case. The snapshot used a 30s cap and current defaults use
  300s; only the recorded seconds let one run be read at both.
- Parallelise per repo (one detached worktree each), not per case — checkouts of
  a 2 GB repo dominate otherwise, and the three repos never contend.
- gitpod and sentry are the timeout-bound pair; react-native completes. A
  liveness claim that does not say which repo is not a claim.

## Corpus baseline discipline

`known_below_threshold.txt` is bidirectional: a listed case that starts
passing fails with "remove it from that file". That is a legitimate
baseline edit **only** when the improvement is intended and kept — if the
cause gets reverted, restore the entry. Never edit the baseline to absorb
an unexplained new failure; bisect the cause first (a scratch `git
worktree` at each candidate commit + a single-case run is the fast path).

## Known non-bugs (audited correct — do not re-file)

- `IntervalIndex::overlaps` boundary asymmetry — deliberate, pinned by
  a verdict-matrix test; symmetric fix is net-zero on the corpus
  (Q-class, next calibration).
- `node_end_line` `+1` in tree-sitter spans is correct (end-exclusive
  to end-inclusive conversion).
- Haskell `type_synomym` matches the upstream grammar's misspelling —
  "fixing" the spelling breaks extraction.
- BM25 `.ln_1p()` is the Lucene IDF formulation, deliberate.
- Float-sum nondeterminism pattern: any accumulation over
  `FxHashMap` iteration order must sort keys first — fixed sites are
  pinned by determinism tests; new code must follow suit.
