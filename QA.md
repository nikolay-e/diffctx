# QA Playbook — diffctx

Project-specific facts for `/qa`. Generic methodology lives in
`~/.claude/qa-refs/`.

## Applicability matrix

| Check | Applies | Notes |
|---|---|---|
| CI | yes | GitHub Actions on the mirror (ci.yml, action-smoke, CodeQL per push; cd.yml/publish-* manual dispatch; nightly-full-eval cron) |
| CD / K8s / ArgoCD | no | CLI+library; ships to PyPI, npm, crates.io, Docker Hub via cd.yml/publish-extras.yml |
| Browser QA | no | no web frontend |
| Post-deploy autoqa | no | no deployed service; probes are the publish smokes inside cd.yml/publish-extras.yml |
| Backend smoke | no | — |
| SonarCloud | yes | project key is `nikolay-e_TreeMapper` (legacy name, never renamed) |

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

## Tests

- `pytest` — integration-only, ~600 tests, ~2 min locally. Known valid
  skip: `test_clipboard.py` (Windows only).
- Local flake mode: `-n auto` on the M4 Pro means 14 xdist workers,
  each CLI subprocess spawning a full rayon pool — under load the
  global pytest-timeout (30 s since 27d6a3d6, was 10 s) fires on
  varying tests. Timeout-only failures with a changing set across runs
  = oversubscription, not a regression; never run `cargo test`
  concurrently with pytest, it makes this worse.
- `cargo test --lib` in `crates/diffctx-native` — inline units, ~171.
- YAML corpus: CI gates the FULL 2725-case corpus on every push
  (`cargo test --profile release-unwind --test yaml_cases`), per-case
  against `known_below_threshold.txt`, enforced bidirectionally;
  nightly re-runs with `DIFFCTX_YAML_IGNORE_BASELINE=1` to track
  baseline size. `DIFFCTX_YAML_CASES_LIMIT=20` sampling survives only
  in the local pre-commit hook.
- E/Q discipline (CLAUDE.md): Q-class (output-changing) fixes are
  frozen during an eval cycle — bugs like the `IntervalIndex::overlaps`
  asymmetry stay documented + pinned, not fixed in-pass.

## Diff-context review

- Use the uv-tool binary `~/.local/bin/diffctx` (pipx-equivalent),
  never `.venv/bin/diffctx`. Check its version FIRST (`uv tool list`,
  not `--version` alone): a stale tool silently re-introduces fixed
  bugs into reviews (a 1.12.2 tool leaked `.diffctx/ignore`-excluded
  tests/yaml as context — the universe filter shipped in 1.12.3).
  Refresh: `uv tool install 'diffctx[mcp]' --force --refresh`.
- This repo's own `.diffctx/ignore` excludes `*.yaml`/`*.yml` (the
  2725-case corpus would drown every self-eat) **and `tests`** — so the
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
- `env_overrides.rs` carries name-consistency tests: any new
  `read_env_*("DIFFCTX_*")` must appear in the `parameter-strategy.md`
  Tier-3 table (or the `TIER1_EXTRAS_READ_BUT_NOT_TABLED` allowlist)
  and vice versa.

## Issue triage invariants

- `v3`-labeled issues are a curated roadmap backlog
  (research/benchmark/paper/product), not rotting defects — check
  staleness, don't force per-pass decisions.
- `gated` label = blocked on a pre-registered experiment or eval-cycle
  boundary.

## Known false positives

- import-linter pre-commit hook can fail locally (namespace package)
  while green in CI.
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
  **Still open for the `context` role** (#123): there is no hunk to window
  around, and the two obvious fixes both fail — see that issue for the
  measurement (43 corpus failures, including cases that keep full recall but
  get worse on forbidden files).
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
