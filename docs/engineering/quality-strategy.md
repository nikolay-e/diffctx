# diffctx — engineering QA notes

Deep per-topic gotchas for working on diffctx. The entry playbook is the
`/qa` skill plus root `QA.md` (applicability matrix, Sonar key, binary
choice, triage discipline) — nothing from there is duplicated here.

## Build & Install Layout

- Python + Rust hybrid wheel built via maturin (PEP 660). Editable
  install: `pip install -e ".[dev,full,mcp]" --no-build-isolation` after
  `pip install "maturin>=1.10,<1.16"`.
- Cargo operates from the root workspace; the native crate lives in
  `crates/diffctx-native/`, Python sources in `src/diffctx/`. Extension
  module name: `diffctx._diffctx`.
- To compile-check the Python bridge use `python -m maturin develop
  --release` — `cargo build --features python` fails at the link step
  (`extension-module` expects the host interpreter). For pure-Rust
  paths: `cargo build` WITHOUT `--features python` / `cargo test --lib`.
- Fast pipeline-debug cycle: env-gated `eprintln!` in Rust +
  `cargo build --release --bin diffctx` (~35s incremental); pay the
  maturin rebuild only when the Python surface is involved.

## Test-Suite Traps

- **Test-gating trap:** `[mcp]` must be in the CI install extras (else
  `importorskip("mcp")` silently skips all of `test_mcp.py`), AND
  `pytest-asyncio` + `asyncio_mode = auto` must be present (else async
  tests collect but never await — they "pass" by never running).
  Re-check when adding any `[<extra>]` that ships its own tests.
- **Hypothesis deadline flakes can unmask real bugs.** I/O-heavy
  property tests flake with `DeadlineExceeded` under concurrent cargo
  builds; fix with `deadline=None` (precedent in the file), then RE-RUN
  the module — the freed example budget can surface a genuine
  falsifying example the abort was masking (this is how the YAML
  block-scalar chomping bug was found).
- **YAML corpus gating:** CI runs the full corpus per-case against
  `crates/diffctx-native/tests/known_below_threshold.txt`,
  bidirectionally — a listed case that starts passing also fails.
  Command: `cargo test --release --test yaml_cases`.
- **Secret-fixture hooks:** private-key exclusion tests match by
  FILENAME only, so fixtures must not embed a literal PEM banner —
  `detect-private-key` has no pragma support and a file committed past
  staged-files hooks turns `--all-files` red. Use inert content plus
  `# pragma: allowlist secret`; keep distinctive leak markers. Only the
  FULL local run (`pre-commit run --all-files`) catches this class.
- **Pre-commit false-fail:** `files were modified by this hook` can
  mean a concurrent background process touched the tree mid-run.
  Re-run when the tree is quiet before treating it as a finding.

## CLI Smoke Recipes

```bash
diffctx src/diffctx/mcp --no-content -f yaml   # tree mode
diffctx --diff HEAD~3..HEAD -f yaml            # diff mode, range
diffctx --diff -f yaml                         # working tree vs HEAD
```

- Format flag is `-f / --format`, NOT `--output-format`.
- Empty-diff result (docs-only, binary-only, clean tree, `--budget 0`)
  is **rc=4** plus a skeleton carrying the commit message and
  `changed_files` — the actionable-error contract, not a regression.
  If the skeleton ever shrinks to name/type only, look at
  `_has_diff_metadata` in `writer.py`.

## Diff-Mode Self-Eat

`diffctx --diff <range>` on this repo's own history is part of every
review. **Do NOT dismiss a large output as "normal for a big commit" —
measure it**:

- `role: "changed"` fragment count vs total fragment count;
- distinct output files vs `git diff <range> --stat` file count;
- **byte-share**: if the top files by rendered bytes are NOT in the
  changed set, that is #65-class over-selection even at a tame ratio.

Over-selection fixes are Q-class (benchmark-validated recalibration) —
add repro data to #65, do not blind-edit edge weights mid-QA. Known #65
shapes (release/version-bump commits, wide flat peer directories, i18n
locale edits, symbol-edge byte-share) are catalogued on the issue —
comment there, do not re-file.

**Confirmed-correct shapes (don't re-file):** deletion-only diffs emit
no `fragments:` but a populated `deleted_files:` list — correct, not
lost signal. Binary-only diffs (all `-\t-` in `git diff --numstat`)
correctly exit 4.

**Merged entries report only the first fragment's `kind`**
(`merge_file_fragments` in render.rs): `lines: 40-470, kind: chunk` can
be a chunk merged with following definitions. Diagnose fragmentation
from the parser, never from rendered `kind:`.

**Whole-file-chunk-as-core class (#105/#107/#123):** when fragmentation
yields no narrow fragment covering a hunk, the core falls back to a
whole-file chunk. The lost-signal half is fixed by the excerpt fallback
(`crates/diffctx-native/src/excerpt.rs` — excerpts live in
`ScoredState.core_excerpts`, never become graph nodes, which is what
keeps the mechanism E-class); the granularity-cost half stays on
#105/#107. Zig comptime types, C++ templates and `*.api` dumps are the
same family — check line spans, not kinds, on metaprogramming-heavy
languages. Compare `--budget 999999` output against auto-budget to find
selection-guarantee gaps fastest.

## Real-World Test-Repo Sweep (every QA round)

`test-repos/` (git-ignored) holds ~40 real upstream clones;
`TOANALYZE.md` curates commits. Every round, sweep a NEW commit per
repo with the **pipx** binary until the first issue:

```bash
cd test-repos/<repo> && git pull --ff-only
/Users/nikolay/.local/bin/diffctx . --diff HEAD~1 --timeout 200
```

- The 12 newer clones are `--filter=tree:0` partial clones — first runs
  lazy-fetch over the network, so wall-clock there is NOT a clean
  signal; re-run warm before judging "slow".
- **A cap-hit IS the issue**: rc=124/137 with no output = hang → file
  it and stop the sweep (don't burn hours on the remaining giants).
  Verify against the dev binary before reopening #70 (closed).
- Bash-tool timeout must exceed the watchdog (pass `timeout: 260000`
  for a 200s `--timeout`), or rc=143 masquerades as a mystery kill.
- Per repo judge: no panic/hang; `changed_files` matches
  `git diff --stat`; no gross over-selection (measure as above); no
  secret/garbage leakage; reasonable time. On any non-empty diff assert
  at least one changed-role fragment.
- **Stopping condition:** first issue filed OR all repos clean — either
  completes the step.
- Mass-added-data ranges (#121: hundreds of added data files) exceed
  even the 600s watchdog — sweep code-only subranges there.
- A rc=124 during a concurrent cargo build is a measurement artifact:
  re-run with the machine quiet and `DIFFCTX_TOKEN_CACHE_DIR` pointed
  at a scratch dir before filing (the cache is size-capped and sharded,
  so repeated timings over an over-cap cache compare different warm
  sets).
- A hand-written synthetic file may not reproduce a real-repo finding
  (parse degradation depends on exact content). Extract the real file:
  `git -C test-repos/<repo> show <rev>:<path>` into a throwaway repo.

## Real-World Quality Benchmark

`datasets/real-world-diff/v1/` scores extracted context against gold
labels over 109 real commits. Re-score after any selection change with
`eval/datasets/real_world_diff/score.py` (gold labels live at
`datasets/real-world-diff/v1/gold_labels.json`). The score
`100·recall·(1−forbidden)` does NOT penalize over-selection — add a
precision term before trusting a high score.

## Edge-Builder Regexes: Compile-Probe, Not Pipeline Coverage

The pipeline does not force every edge-builder `Lazy<Regex>` on small
inputs, so a passing YAML case doesn't prove the regexes compile. A
nested bounded repetition of a Unicode negated class can exceed the
10 MiB compile limit and abort at first use. Guard: a `#[cfg(test)]`
probe forcing every regex in the module
(`kubernetes::tests::all_kubernetes_regexes_compile`). Unbound the
value class instead of bounding it — the regex crate is linear-time.

## SonarCloud — diffctx specifics

Fix loop: push → wait for the `diffctx CI` run → re-fetch → mark FPs →
gate. Project key and FP-remark gotcha live in root `QA.md`.

- `eval/**` scripts are analyzed like any source — a new script there
  can flip the gate alone.
- `python:S7494`/`S7500` contradict each other on `dict(genexpr)` —
  rewrite as an explicit loop, satisfying both.
- `pythonsecurity:S8707`/`S8705` (path/arg injection) are a bulk-FP
  class here: a local CLI reading user-named paths crosses no privilege
  boundary. Bulk-resolve `falsepositive` with a one-line rationale;
  re-triage only if a surface crosses a real trust boundary.
- Rule activations land findings on untouched code (docker/githubactions
  family, `python:S8997`). Budget one extra verification-loop iteration
  whenever the first fetch is non-empty.

## Evaluation Image (ghcr.io/nikolay-e/diffctx-eval)

- **A renamed ghcr package starts PRIVATE** with no API to flip it (web
  UI only). Symptom: `docker pull` says "not found", not "denied".
  After any rename verify ANONYMOUS token + manifest GET returns 200.
- Every COPY source in `Dockerfile.eval` must be git-tracked — local
  buildx succeeds while CI fails otherwise. Guarded by
  `tests/eval/test_image_inputs.py`.
- The image auto-publishes via `eval-image.yml`; eval-sweep's provision
  job fail-fasts on a missing manifest before paying for a server.

## Concurrent-Session Hazard

If `git status` shows modifications you didn't make: stop before any
commit/revert; confirm a live concurrent writer (mtimes, `ps`); stage
YOUR files by explicit path only; treat local build/test results as
invalid for HEAD (CI on the pushed commit is the only honest verifier).
Parallel work belongs in `git worktree`s, one session per checkout.

## Output-Format Grep Traps

- Default output is Markdown (post-#104): files are `` ## `path:lines` ``
  headers, roles are `— **changed**`. YAML-shaped greps silently return
  0 — pass `-f yaml` in recipes or count with `grep -cE '^## '`.
- The standalone Rust binary renders YAML plainly (`role: changed`)
  while the Python CLI quotes (`role: "changed"`) — use
  `grep -E 'role: "?changed"?'` in checks that may see either.
- Scripts re-tokenizing diffctx output with tiktoken must pass
  `encode(text, disallowed_special=())` — real repos contain literal
  special-token text.

## The Two CLIs Have Two Independent Arg Parsers

`src/diffctx/cli.py` (argparse) and `crates/diffctx-native/src/main.rs`
(clap) define their flags separately; nothing keeps them in sync, and
the test suites pass parameters explicitly, never through clap
defaults. `crates/diffctx-native/tests/native_cli.rs` pins the covered
contract (exit codes, token summary, format validation, budget
handling); for anything newly added, **diff the two `--help` outputs
each release-QA pass and compare observable behavior** (exit codes,
stderr), not just the flag table.

## Release Channels and Freeze Policy

Channels sit in three tiers: **supported** (verified by INSTALLING
before the release is done; regression blocks release), **best-effort**
(auto-published, verified when convenient), **manifest-only** (never
advertise as an install route).

| Channel | Published by | Tier |
|---|---|---|
| PyPI | `cd.yml` (`publish-to-pypi`) | supported |
| crates.io | `publish-crate.yml`, dispatched by `cd.yml` | supported |
| GitHub Release | `cd.yml` (`finalize-release`) | supported |
| ghcr.io | `cd.yml` (`build-image` → `publish-image`) | supported |
| MCP registry | `cd.yml` (`publish-mcp-registry`, github-oidc) | supported |
| npm | `publish-extras.yml`, dispatched by `cd.yml` | best-effort |
| Docker Hub `nikolajer/diffctx` | `publish-extras.yml` | best-effort |
| Scoop `bucket/diffctx.json` | `cd.yml` (`update-packaging-manifests`) | best-effort |

Only `publish_to_pypi == 'true'` arms the crates.io and publish-extras
dispatches; a dry run publishes nowhere. **The Scoop bucket is this
repository** — pushing the regenerated manifest to `main` IS the
publication, so `update-packaging-manifests` is a real publishing job
and must not carry `continue-on-error`.

A release QA pass verifies every live channel by INSTALLING it:

| Channel | Verify with |
|---|---|
| PyPI | fresh venv, `pip install diffctx==<v>`, real `--diff` run |
| crates.io | `cargo install diffctx --version <v> --locked` |
| npm | `npm install diffctx@<v>` in a scratch dir |
| GitHub Release | assets = 4 wheels + sdist + 4 binary archives |
| ghcr / Docker Hub | `docker run --rm <img>:<v> --version` + `--diff` |
| MCP registry | registry API reports the version, `status: active` |
| Scoop | `bucket/diffctx.json` carries the release `.zip` SHA-256 |

Release rules learned the hard way (each burned at least one release):

- A channel with no dispatch silently serves the previous release —
  verify npm/Docker Hub BY INSTALLING, a green CD run is not evidence.
- CD pushes the release tag to GitHub only — after every release,
  `git push origin v<version>` (Forgejo); registry entries are
  immutable, both remotes must carry the tag first.
- A draft GitHub release does NOT create its tag; publishing an old
  draft steals the `Latest` marker (`gh release edit --latest`).
- Generated manifests trip `detect-secrets` — the hook excludes
  `^(packaging|bucket)/`; moving a manifest elsewhere turns CD red.
- OIDC in a reusable workflow carries the CALLER's filename — CD
  *dispatches* `publish-crate.yml` instead of `uses:`-calling it.
- Smoke-test binaries by asserting the FORMAT of output, not just rc=0.
