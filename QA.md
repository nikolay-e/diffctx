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
  but **all CI, issues, PRs, Dependabot live on GitHub**. Forgejo
  issue/PR lists are empty by design.
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
  2725-case corpus would drown every self-eat), so CI/workflow YAML
  changes NEVER appear in self-eat output — a workflow-only range
  legitimately yields rc=4 and a bare skeleton (excluded paths are
  hidden even from `changed_files` by the security contract). Review
  workflow changes with plain `git diff`, don't file this as a bug.
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
