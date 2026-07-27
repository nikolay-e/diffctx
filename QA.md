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

- `pytest` — integration-only, ~600 tests, ~10 s. Known valid skip:
  `test_clipboard.py` (Windows only).
- `cargo test --lib` in `crates/diffctx-native` — inline units, ~171.
- YAML corpus: `cargo test --test yaml_cases` (CI runs a 20-case sample
  via `DIFFCTX_YAML_CASES_LIMIT=20`; the full 2725-case corpus is gated
  per-case against `known_below_threshold.txt`, enforced
  bidirectionally, nightly).
- E/Q discipline (CLAUDE.md): Q-class (output-changing) fixes are
  frozen during an eval cycle — bugs like the `IntervalIndex::overlaps`
  asymmetry stay documented + pinned, not fixed in-pass.

## Diff-context review

- Use the uv-tool binary `~/.local/bin/diffctx` (pipx-equivalent),
  never `.venv/bin/diffctx`.
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
  deps — suppressed with NOSONAR.
