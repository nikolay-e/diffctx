# TESTS.md — test coverage gap log

Findings log for `/review-tests`. Append-only; each run adds a dated section.
Framework: `~/.claude/review-pyramid.md`.

---

## 2026-07-27 — 7ba0672f

Scope: `src/diffctx/**`, `crates/diffctx-native/**`, `src/diffctx/mcp/**`,
`action.yml`,
`.github/workflows/**`, `packaging/**`. Topology: 9 scouts → direct grep
verification of every
🔴 claim → this verdict. Working tree carried three uncommitted `git.rs` fixes at
review time;
they are treated as production code.

**Confidence marks**: `CONFIRMED` = quote and absence-of-coverage re-verified
directly in this
session. `PLAUSIBLE` = scout-reported, quote verified as a real substring,
absence not
independently re-run.

### Structural fact behind most of this section

`grep -c 'cfg(test)'` returns **0** for `git.rs`, `pipeline.rs`, `select.rs`,
`render.rs`,
`pybridge.rs`, `interval.rs`, `filtering.rs`, `signatures.rs`, `discovery.rs`,
`languages.rs`.
`cargo test --lib` — the first command in the CI Rust job — covers none of the
git layer, none of
the orchestration layer, and none of the selection layer. All protection for
those is end-to-end
via Python subprocess tests plus **20** YAML cases.

---

## Action list

### DO

1. **Filter the discovery universe, and test it** —
   `crates/diffctx-native/src/pipeline.rs:231`
2. **Regression-test the three uncommitted `git.rs` fixes before committing
   them** — `git.rs:29`, `:88`, `:808`
3. **Run the oracle suite at the production τ** —
   `crates/diffctx-native/tests/yaml_cases.rs:235`
4. **Assert the hard budget contract `cost(C) ≤ B`** — `postpass.rs:90`
5. **Make `--no-default-ignores` fail loudly in diff mode instead of being
   dropped** — `pybridge.rs:250`
6. **Stratify the CI YAML sample; 35 of 38 language dirs never run pre-merge** —
   `ci.yml:110`
7. **Smoke-test `diffctx-mcp` and the `[mcp]` extra against the published
   wheel** — `cd.yml:498`
8. **Build `Dockerfile` on PRs** — currently first built after PyPI has already
   shipped — `cd.yml:650`
9. **Assert the npm package version before publish; execute `install.js` once**
   — `publish-extras.yml:79`
10. **Delete or arm the `cargo audit` phantom gate** —
   `.pre-commit-config.yaml:311`
11. **Validate `budget_tokens` on the MCP surface the way the CLI does** —
   `mcp/server.py:117`
12. **Add a version-consistency test across the eight manifests** — 8 files, 0
   tests
13. **Escape control characters in the YAML writer** — `writer.py:18`
14. **Pin the `BestSingleton` invariant** — `select.rs:670`
15. **Test the two-pass edge cap and the pre-cap category table** —
   `graph.rs:646`, `edges/mod.rs:260`

### VERIFY

1. **VERIFY(gate: a `#[test]` feeding a syntactically broken file per grammar
   shows no panic and valid spans)** — invalid-syntax handling,
   `parsers/mod.rs:32`
2. **VERIFY(gate: a rename-only diff test asserting `renamed_files ==
   [{"from":…,"to":…}]`)** — `render.rs:24`
3. **VERIFY(gate: a test that sets a real `DIFFCTX_*` var and observes the
   resolved field change)** — `config/env_overrides.rs:35`; note
   `scripts/sensitivity_check.py:14` sweeps a var that does not exist
4. **VERIFY(gate: two concurrent MCP tool calls on one repo both see their
   `.diffctx/ignore` rules)** — `git.rs:720` PID-keyed temp file
5. **VERIFY(gate: an `action-smoke` leg that actually passes
   `tau`/`alpha`/`timeout`/`output-path`)** — `action-smoke.yml:53`

### DON'T

1. **DON'T** add Python unit tests for any of the above (project rule:
   integration/E2E only; Rust inline `#[test]` is the correct instrument for
   the Rust findings).
2. **DON'T** treat the YAML case
   `operations_006_diffctxignore_excludes_file` (under
   `tests/cases/diff/algorithm/operations/`) as coverage — it writes
   `.diffctxignore`, a filename diffctx never reads as an ignore source
   (the real source is `.diffctx/ignore`). Rename the fixture or delete
   the case; leaving it is worse than having no case.
3. **DON'T** rely on `nightly-full-eval.yml` as the language-regression
   backstop — a single aggregate `MIN_PASS_COUNT: "2260"` cannot localize which
   language broke, and an improvement anywhere masks a regression elsewhere.

---

## Findings

### 🔴 The discovery universe is never ignore-filtered — an explicitly excluded

file ships as neighbour context

- **Location**: `crates/diffctx-native/src/pipeline.rs:231`,
  `crates/diffctx-native/src/candidate_files.rs:38`
- **Evidence**: `let all_candidate_files =
  candidate_files::collect_candidate_files(&root_dir, &included_set);` — and
  inside it, the only predicate is `.filter(|f| is_candidate_file(f, root_dir,
  included_set))`, a language check.
- **Problem**: `changed_files` **is** filtered (`pipeline.rs:195-197`:
  `!is_secret_path(f)` … `!is_ignored_path(...)`). The candidate universe is
  not. Because `.diffctx/ignore` is not a gitignore, an excluded file is still
  listed by `git ls-files -z` (`candidate_files.rs:39`), so it enters
  `all_candidate_files`, the graph, and the rendered output as related context.
  The user wrote a rule telling diffctx to exclude that file, exit code is 0,
  and nothing warns.
- **Scenario**: monorepo with `.diffctx/ignore` containing `secrets/config.py`;
  user changes `app.py`, which imports it; `diffctx . --diff main..HEAD` renders
  `secrets/config.py` as neighbour context.
- **No coverage**: `grep -n "is_secret_path\|is_ignored_path"
  crates/diffctx-native/src/candidate_files.rs` → no output. Every ignore test
  in the corpus (`test_custom_ignores_diff.py`, `test_secret_ignores_diff.py`)
  changes the excluded file, exercising only the `changed_files` path.
- **Recommendation**: YAML case — `.diffctx/ignore` excludes an *unchanged* file
  that a changed file imports; assert it appears in `forbidden`.
- **Verification**: the case fails today; passes once the filter moves into
  `collect_candidate_files`.
- **Confidence**: CONFIRMED

### 🔴 Three uncommitted `git.rs` fixes have zero regression tests — reverting

any of them keeps CI green

- **Location**: `crates/diffctx-native/src/git.rs:29`, `:88`, `:808`
- **Evidence**:
  - `:29` — `const SAFE_DIFF_FLAGS: &[&str] = &[` … `"--no-color",
    "--src-prefix=a/", "--dst-prefix=b/"`
  - `:88` — `let separator = if trimmed.contains("...") { "..." } else { ".."
    };` followed by per-side `validate_rev`
  - `:808` — `let (rule, path) = line.rsplit_once('\t')?;`
- **Problem**: each fix closes a real, already-observed defect — a user's
  `diff.noprefix`/`diff.mnemonicPrefix`/`color.ui=always` producing zero
  fragments; `a..--ext-diff` slipping past a documented argv-injection gate; an
  ignore pattern containing a tab causing an ignored file to be reported as not
  ignored. None is locked in. `grep -c 'cfg(test)'
  crates/diffctx-native/src/git.rs` → `0`, and no test file references any of
  these helpers.
- **No coverage**: `grep -rniE 'noprefix|mnemonicPrefix|color\.ui|src-prefix'
  tests/ crates/diffctx-native/tests/` → zero hits. The only injection test,
  `tests/test_mcp.py:349`, is parametrized on *bare* option strings
  (`--ext-diff`, `-p`, …), never the `a..-x` range form. `grep -rn 'rsplit_once'
  tests/ crates/` → only the production line.
- **Recommendation**: three inline `#[cfg(test)]` in `git.rs` — (a) run the
  pipeline in a repo with `git config diff.noprefix true` and assert fragments
  are non-empty; (b) `validate_diff_range("HEAD..--ext-diff")` is an error while
  `@{-1}..HEAD` and `HEAD~2...origin/main` still pass; (c)
  `parse_verbose_ignore_match` on a line whose pattern contains a tab.
- **Verification**: reverting each hunk turns exactly one test red.
- **Confidence**: CONFIRMED

### 🔴 The τ adaptive-stop rule is disabled in all 2725 oracle cases

- **Location**: `crates/diffctx-native/tests/yaml_cases.rs:235`, rule at
  `crates/diffctx-native/src/select.rs:463`
- **Evidence**: the oracle runner passes `0.0,` as τ; the rule reads `} else if
  peak_density > 0.0 && best_density < tau * peak_density {`
- **Problem**: shipped default is `DEFAULT_STOPPING_THRESHOLD: f64 = 0.12`
  (`config/limits.rs:50`). With `tau = 0.0` the predicate is `best_density <
  0.0`, unreachable for densities already gated `> 0` — so `StoppedByTau` never
  fires and `stopping_certificate` is always 0. Delete the entire stopping rule
  (greedy then runs to budget exhaustion, shipping far more context per query)
  and all 2725 cases still pass.
- **No coverage**: `grep -rn "stopping_certificate" tests/
  crates/diffctx-native/tests/` → no output. `grep -rn "tau" tests/*.py
  crates/diffctx-native/tests/*.rs` → one hit, `test_diffctx_invariants.py:337`,
  on the deletion-only case whose assertion is `fragment_count == 0`.
- **Recommendation**: run the oracle at `DEFAULT_STOPPING_THRESHOLD`; add a
  `run_greedy_loop_heap` unit test with densities `[1.0, 0.5, 0.05]` asserting
  the third is skipped at τ=0.12 and taken at τ=0.
- **Verification**: setting τ to 0.0 in production fails the new unit test; the
  oracle suite still passes at 0.12.
- **Confidence**: CONFIRMED
- **Note**: this is a **Q-class** change under `CLAUDE.md`'s E/Q discipline —
  running the oracle at the production τ will move scores. Land it at a cycle
  boundary, not mid-calibration.

### 🔴 The hard budget contract `cost(C) ≤ B` is asserted nowhere

- **Location**: `crates/diffctx-native/src/postpass.rs:90`
- **Evidence**: `// Nothing fits: the budget cap is a hard contract (cost(C) <=
  B). The`
- **Problem**: four independent paths gate on the budget (`select.rs:216`,
  `:248`, `:306`, `postpass.rs:86`, `:212`). `pick_smallest_fitting` returning a
  non-fitting candidate is one `if` away — and it is the direction the
  change-covering preference pushes. The oracle can never catch it:
  `calculate_budget` in `tests/common/mod.rs:226` is `((content_tokens +
  n_frags*overhead) * 5 / 2).max(500)` over *all* files, i.e. always ≥2.5× the
  whole repo. The one tight-budget Python test (`--budget 50`) asserts only the
  exit code and `len(small.stdout) <= len(large.stdout)`, which survives a 2×
  overrun.
- **Recommendation**: Rust test running `lazy_greedy_select` + all three
  post-passes at `budget=200` asserting the summed token count ≤ 200; plus a
  Python test at `--budget 300` summing `count_tokens` over the JSON fragments.
- **Verification**: removing the `cand.token_count <= budget_left` check in
  `pick_smallest_fitting` turns the test red.
- **Confidence**: PLAUSIBLE

### 🔴 `--no-default-ignores` is accepted by the CLI and silently discarded by

the Rust diff backend

- **Location**: `crates/diffctx-native/src/pybridge.rs:250`
- **Evidence**: `tracing::warn!("no_default_ignores is not yet implemented in
  Rust backend, ignored");`
- **Problem**: `src/diffctx/_native/pipeline.py` raises `NotImplementedError`
  for the two siblings `ignore_file` and `whitelist_file` — the comment there
  calls silently dropping them "a security-adjacent footgun" — but forwards
  `no_default_ignores` to Rust, which drops it. `tracing_subscriber` is
  initialised only in the native binary (`main.rs:13`, `:163`), never in the
  extension module, so the warning goes nowhere. The user gets exit 0 and a
  result computed with the default ignore set still applied.
- **No coverage**: `grep -rn -- "--no-default-ignores" tests/` →
  `tests/test_ignore.py:196`, tree-map mode only, no `--diff`.
- **Recommendation**: mirror the sibling guard — raise from Python — and add a
  test asserting `--diff … --no-default-ignores` either changes the selected
  file set or fails loudly.
- **Verification**: flipping the guard breaks exactly one test.
- **Confidence**: CONFIRMED

### 🔴 CI gates 20 alphabetically-first YAML cases; 35 of 38 language directories

never run pre-merge

- **Location**: `.github/workflows/ci.yml:110`,
  `crates/diffctx-native/tests/yaml_cases.rs:266`
- **Evidence**: `DIFFCTX_YAML_CASES_LIMIT: "20"`; the full run at `ci.yml:124`
  is `run: ../../target/release/examples/diffctx-test || true`
- **Problem**: `discover_cases()` sorts by path before `truncate(n)`, so the
  gate is deterministically `algorithm/algorithm_001..020` — not a sample. `find
  tests/cases/diff -name '*.y*ml' | sort | head -20 | xargs grep -h "lang:"` →
  17 python, 2 markdown, 1 latex. Nothing under `tests/cases/diff/languages/**`
  (768 cases, 38 dirs) runs on a PR. This is precisely the surface of the open
  memory item *"VERIFY: regex→tree-sitter benchmark after Rust/Go/C++/JS/JVM
  migration"*. The nightly backstop gates one aggregate number (`MIN_PASS_COUNT:
  "2260"`), so it cannot say which language regressed, and any improvement masks
  a regression.
- **Recommendation**: replace `truncate(20)` with a stratified pick — one case
  per `languages/*` dir (38 cases) — and have nightly gate per-directory pass
  counts.
- **Verification**: CI log shows ≥1 trial from every `languages/*` directory.
- **Confidence**: CONFIRMED

### 🔴 The MCP entry point every plugin channel resolves to is never smoke-tested

against the published wheel

- **Location**: `.github/workflows/cd.yml:498`
- **Evidence**: the release smoke installs `python -m pip install
  "diffctx==${VERSION}"` and runs `diffctx --version`, tree mode, an import, and
  `diffctx . --diff`. No `[mcp]` extra, no `diffctx-mcp`.
- **Problem**: `pyproject.toml:114` declares `scripts.diffctx-mcp =
  "diffctx.mcp.server:main"`; `server.json`, `.mcp.json` and the Claude plugin
  all resolve to `uvx --from diffctx[mcp] diffctx-mcp`. Rename `main`, or let
  the `mcp` extra stop resolving on 3.10, and the wheel still installs, `diffctx
  --version` still works, CD is green, and every MCP consumer gets a server that
  will not start. `tests/test_mcp.py` imports the module in-process from the
  editable dev install; it never spawns the console script. Additionally
  `run_server()` sets `stream=sys.stderr` — load-bearing, since stdio
  multiplexes JSON-RPC over stdout — and no test ever observes stdout
  cleanliness.
- **No coverage**: `grep -rn "diffctx-mcp\|uvx\|mcp\]" .github/workflows/
  Makefile` → only editable dev installs (`ci.yml:185`, `Makefile:25`). Zero
  hits in `cd.yml`.
- **Recommendation**: add to `smoke-pypi` — `pip install
  "diffctx[mcp]==$VERSION"`, then a stdio `initialize` handshake asserting the
  reported server version equals `$VERSION` and that stdout carries only
  JSON-RPC.
- **Verification**: renaming `diffctx.mcp.server:main` on a scratch branch fails
  the release smoke.
- **Confidence**: PLAUSIBLE (grep re-run confirmed the absence in `cd.yml`)

### 🔴 `Dockerfile` is first built during a live release, after PyPI and the

GitHub Release have shipped

- **Location**: `.github/workflows/cd.yml:650`
- **Evidence**: `needs: [prepare-version, finalize-release]` with `if:
  github.event.inputs.publish_to_pypi == 'true'`
- **Problem**: `Dockerfile:6-9` copies an explicit path whitelist, so any new
  source dir breaks `cargo build --release --locked`. No PR job builds it: `grep
  -rln "Dockerfile" .github/workflows/` → `eval-image.yml` only, which builds
  the *different* `Dockerfile.eval`. At release time the image build runs after
  the tag is pushed, the Release is created, and PyPI has uploaded — so ghcr.io,
  a documented supported channel, silently stops receiving tags while every
  other channel moves. The only `docker run` anywhere is the post-publish smoke
  at `cd.yml:744`.
- **Recommendation**: PR job `docker build -f Dockerfile .` gated on `paths:
  [Dockerfile, Cargo.*, crates/**]`, plus the two-commit `--diff` smoke.
- **Verification**: deleting a `COPY` line makes a PR go red instead of a
  release going half-published.
- **Confidence**: CONFIRMED

### 🔴 `npm publish` runs with no version assertion, and `install.js` is never

executed

- **Location**: `.github/workflows/publish-extras.yml:79`
- **Evidence**: `packaging/npm/package.json:4` is `"version":
  "0.0.0-placeholder",`, patched by an inline heredoc immediately before `npm
  publish --access public --provenance`.
- **Problem**: if that heredoc is refactored or mis-pathed, npm publishes
  literal `diffctx@0.0.0-placeholder` and exits 0. `install.js:99` then computes
  `diffctx-0.0.0-placeholder-<target>.tar.gz`, `checksums[assetName]` is
  undefined, and every `npx diffctx` (advertised in `README.md:46`) dies.
  `cd.yml:838` only `gh run watch`es the dispatch, which succeeded.
- **No coverage**: `grep -rn "install.js" .github/workflows/` → only comments
  naming the job. The script is never executed anywhere.
- **Recommendation**: assert `node -p "require('./package.json').version" ==
  $VERSION` before publish; add a post-publish job doing `npm install
  diffctx@$VERSION` in a scratch dir and running `--version`.
- **Verification**: no-op the version step locally; the pre-publish assert
  fails.
- **Confidence**: PLAUSIBLE (placeholder string confirmed)

### 🔴 `cargo audit` is a phantom gate — short-circuited to `true`, and the tool

is never installed

- **Location**: `.pre-commit-config.yaml:311`
- **Evidence**: `entry: bash -c 'command -v cargo-audit >/dev/null 2>&1 && cargo
  audit || true'`
- **Problem**: fails open twice over — `cargo-audit` is not preinstalled on
  GitHub runners and nothing installs it (`grep -rn "cargo-audit\|cargo
  install"` across all yml/toml/sh/Makefile → only the hook's own two lines),
  and the trailing `|| true` swallows a real non-zero exit. `ci.yml:41` runs
  `pre-commit run --all-files` and prints a green line for a check that has
  never once executed. Meanwhile `automerge.yml:56` auto-merges Dependabot patch
  bumps.
- **Recommendation**: install `cargo-audit` in the pre-commit CI job and drop
  `|| true`, or delete the hook. A gate that cannot fail is worse than its
  absence.
- **Verification**: pin a knowingly-vulnerable dep on a scratch branch; the job
  must go red.
- **Confidence**: CONFIRMED

### 🔴 MCP `get_diff_context` accepts any `budget_tokens`; negative means

unlimited, and there is no output ceiling

- **Location**: `src/diffctx/mcp/server.py:117`
- **Evidence**: `budget_tokens: int = 8000,` — with no `max_tokens` parameter,
  unlike `get_tree_map` (`:178`) and `get_file_context` (`:297`), both of which
  carry `max_tokens: int = _DEFAULT_MAX_TOKENS`.
- **Problem**: the value goes straight to `_normalize_budget`, where
  `budget_tokens < 0` returns `_UNLIMITED_BUDGET = 10_000_000`. An agent passing
  `-1` gets a 10M-token selection in one tool response with no ceiling; passing
  `0` gets a near-empty skeleton (`with_raw_diff` defaults to `False`) and
  concludes the diff is trivial. The CLI rejects exactly this input —
  `cli.py:68` — the MCP surface does not.
- **No coverage**: `grep -rn "budget_tokens" tests/test_mcp.py` → two positive
  values (200, 8000) whose only assertion is `len(small) <= len(large)`.
- **Recommendation**: integration test asserting `budget_tokens ∈ {-2, 0}` is
  rejected with the CLI's message or bounded, plus a `max_tokens` ceiling on
  `get_diff_context`.
- **Verification**: the negative case returns an error rather than 10M tokens.
- **Confidence**: CONFIRMED

### 🔴 The YAML writer passes most control characters through raw, producing an

unparseable document at exit 0

- **Location**: `src/diffctx/writer.py:18`
- **Evidence**: `_YAML_PROBLEMATIC_RE` matches only `\r`, `\x00`, `\x85`, ` `,
  ` `.
- **Problem**: every other C0/C1 control character — form feed, ESC, BEL, VT,
  DEL — reaches a block scalar verbatim (and the double-quoted branch too),
  making the whole YAML document unparseable while the CLI exits 0 and prints a
  token count. Reproduced with a real `python -m diffctx . -f yaml` run on a
  repo whose only file contains an Emacs page break (`\x0c`), which is common in
  older C and Lisp sources. `tests/test_properties.py` cannot catch it: its
  Hypothesis strategies blacklist the `Cc` category — exactly the class at
  fault.
- **Recommendation**: widen the regex to all of C0/C1 except `\t`/`\n`, and drop
  the `Cc` blacklist from the property strategies so the fuzzer covers it.
- **Verification**: a repo fixture containing `\x0c` produces output that
  `yaml.safe_load` parses.
- **Confidence**: PLAUSIBLE (scout reproduced via a real CLI run; regex contents
  confirmed)

### 🔴 `BestSingleton` can replace the entire selection with one fragment

dropping every changed-code fragment

- **Location**: `crates/diffctx-native/src/select.rs:670`
- **Evidence**: `best_alt = Some((vec![full.clone()], full.token_count));`
- **Problem**: `find_best_singleton_full_set` scans the full ground set against
  the full budget with an empty utility state. When one heavy fragment's
  standalone utility beats the greedy chain's — realistic when a large core is
  demoted to a signature stub — the returned selection is literally that one
  fragment and all changed fragments are discarded.
  `ensure_changed_files_represented` then has only `budget − full.token_count`
  left. `SelectionReason::BestSingleton` is computed but never rendered, so the
  output carries no hint this happened.
- **No coverage**: `grep -rn "singleton" tests/*.py
  crates/diffctx-native/tests/*.rs` → no output; `select.rs` has no `cfg(test)`.
- **Recommendation**: a `lazy_greedy_select` test constructing the case,
  asserting either that ≥1 `core_ids` fragment survives, or that the branch is
  deliberately allowed and the invariant is enforced downstream — pick one and
  pin it.
- **Verification**: changing the branch to `sel = base_selected + full` fails
  the test.
- **Confidence**: CONFIRMED (line and quote verified)

### 🔴 The two-pass edge cap has zero tests, and the category table is pre-cap

- **Location**: `crates/diffctx-native/src/graph.rs:646`,
  `crates/diffctx-native/src/edges/mod.rs:260`
- **Evidence**: the correctness claim the whole design rests on,
  `edges/mod.rs:99` — *"Any edge evicted from a per-builder heap is outranked by
  K surviving same-source edges, so the final merge + dedup + cap over the
  survivors is bit-identical to capping the full materialized universe."*
- **Problem**: two defects, one gap. (a) With `DEFAULT_MAX_OUT_EDGES_PER_NODE =
  64`, any hub — a `utils.py` referenced from 100+ places — silently discards
  its 65th..Nth neighbour, and which 64 survive is decided by a tie-break
  comparator that appears twice with *inverted* `dst` ordering (`graph.rs:652`
  vs `RankedCandidate::rank` at `:715`). Nothing checks the two agree. (b)
  `category_entries` is built in pass 1 *before* the cap and handed to the
  export unchanged, while the CSR is built post-cap — so `graph_export.rs:176`
  emits capped-away edges with `weight: 0.0`, and `edge_count` disagrees with
  `len(edges)` in the same JSON document. `filtering.rs:88` reads the same
  table, so a nonexistent edge suppresses hub-noise filtering.
- **No coverage**: no reference to `cap_out_edges_per_source`,
  `push_bounded_top_k`, or `RankedCandidate` exists outside the two definition
  sites. Every existing test reaches the cap with ≤2 edges, so `group >
  max_per_node` has never been true in a test run. The only invariant assertion,
  `project_graph.rs:220`, runs on a 5-fragment repo where the cap cannot fire.
  No Python test asserts `doc["edge_count"] == len(doc["edges"])`.
- **Recommendation**: (a) a cap test with 5 out-edges and `max_per_node=2`
  asserting survivors, tie-break direction and `edges_dropped_by_cap`; (b) an
  equivalence test between the two-pass and materialized paths; (c) an export
  test asserting `edge_count == edges.len()` and all weights > 0 on a repo where
  `edges_dropped_by_cap > 0`.
- **Verification**: reversing the `dst` tie-break in either comparator fails a
  test.
- **Confidence**: PLAUSIBLE (`edges/` module and quoted lines confirmed to
  exist)

### 🔴 A syntactically invalid file yields zero symbols but a normal-looking

fragment set

- **Location**: `crates/diffctx-native/src/parsers/mod.rs:32`
- **Evidence**: `if !fragments.is_empty() { return fragments; }`
- **Problem**: for a file mid-refactor (unbalanced brace, stray `<<<<<<< HEAD`),
  tree-sitter returns a mostly-`ERROR` tree, `extract_definitions` matches
  nothing, and `create_code_gap_fragments` emits whole-file `Chunk` fragments
  with `symbol_name: None`. Because that vector is non-empty, `GenericStrategy`
  is never consulted and no signal reaches the caller. The code never inspects
  `Node::has_error` anywhere (`grep -rn "has_error\|is_error"
  crates/diffctx-native/src/parsers/` → no output). The output looks like
  successful extraction; symbol-based graph edges and core identification for
  that file are silently gone.
- **No coverage**: `grep -rniE "syntax_error|invalid_syntax|SyntaxError"
  tests/*.py crates/diffctx-native/tests/` → no output; no YAML case feeds a
  broken file to any parser.
- **Recommendation**: a table-driven `#[test]` per registered grammar
  fragmenting a deliberately broken snippet, asserting no panic, valid
  non-overlapping spans within `lines.len()`, and that definitions *before* the
  error point keep their `symbol_name`. Consider surfacing `has_error` so a
  failed parse is observable.
- **Verification**: `cargo test --lib` covers each `ts_name`; the test count
  changes when a grammar is added.
- **Confidence**: PLAUSIBLE

### 🔴 The `renamed_files` serializer has zero assertions, and the YAML harness

cannot express a rename at all

- **Location**: `crates/diffctx-native/src/render.rs:24`
- **Evidence**: `fn serialize_renames<S>(renames: &[(String, String)],
  serializer: S) -> Result<S::Ok, S::Error>`, applied at `:52` via
  `serialize_with = "serialize_renames"` — the only field in `DiffContextOutput`
  with a hand-written serializer.
- **Problem**: dropping the attribute makes serde emit `- [old.py, new.py]`
  instead of `- from: old.py / to: new.py`. Both are valid YAML/JSON, both
  parse, `fragment_count` is unchanged, every test still passes — and every
  downstream consumer loses the labelled rename. On a pure-rename diff this
  field is the *only* signal the output carries. Structurally, `write_files` in
  `yaml_cases.rs:104` only writes and never removes, so **all 2725 cases are
  add/modify** — renames and deletions are inexpressible in the declarative
  corpus.
- **No coverage**: `grep -rn "renamed_files" tests/ crates/diffctx-native/tests/
  eval/` → one docstring, `test_diffctx_invariants.py:323`, whose assertions are
  `deleted_files` and `fragment_count` only.
- **Recommendation**: a rename-only diff test asserting
  `json.loads(stdout)["renamed_files"] == [{"from": …, "to": …}]`; separately,
  teach the YAML harness to express deletions and renames.
- **Verification**: removing `serialize_with` fails the new test.
- **Confidence**: CONFIRMED (quote and attribute verified)

### 🟡 No test proves any `DIFFCTX_*` override name reaches the parameter it

names — one knob is already inert

- **Location**: `crates/diffctx-native/src/config/env_overrides.rs:35`
- **Evidence**: the module's own doc-comment — *"tests verify the pure parser
  (`parse_*_or_default`) directly so they do not need to mutate process-global
  env state"*. All 6 inline tests pass literals like `Some("0.42".into())`; the
  env-var **name string is never in the test path**.
- **Problem**: the bug class is already live in-tree.
  `crates/diffctx-native/src/config/scoring.rs:16` reads
  `DIFFCTX_EGO_PER_HOP_DECAY`; `docs/engineering/parameter-strategy.md:132`
  documents that name; but `scripts/sensitivity_check.py:14` sweeps
  `("DIFFCTX_OP_EGO_PER_HOP_DECAY", 1.0)` — a variable that does not exist. That
  sweep has been silently measuring the default γ=0.5 at every point.
- **Recommendation**: cheapest complete fix — a test asserting the set of
  `DIFFCTX_*` literals in `crates/diffctx-native/src/` equals the set documented
  in `parameter-strategy.md` and used in `scripts/sensitivity_check.py`.
- **Verification**: the name-set test fails today on
  `DIFFCTX_OP_EGO_PER_HOP_DECAY`.
- **Confidence**: CONFIRMED

### 🟡 Version consistency across eight manifests is untested; the only guard

runs after the release is dispatched

- **Location**: `.github/workflows/cd.yml:81-146`
- **Evidence**: the bump script's guard is `assert n == 1, "action.yml:
  diffctx-version default not found"` with `re.subn(..., count=1)`.
- **Problem**: eight files carry the version — `pyproject.toml:8`,
  `src/diffctx/version.py:1`, `crates/diffctx-native/Cargo.toml:3`,
  `Cargo.lock`, `server.json:6` and `:17`, `.claude-plugin/plugin.json:4`,
  `action.yml:90`, `bucket/diffctx.json:2` — and `bucket/diffctx.json` is
  **not** in the CD bump list. `count=1` means a new unquoted-semver default
  added above `diffctx-version` gets patched instead, with the assert still
  passing. Result: the wheel ships as N+1 while the action keeps installing N,
  and Marketplace consumers stay pinned forever.
- **No coverage**: `grep -rn "version" tests/*.py | grep -i
  "server.json\|plugin.json\|Cargo.toml\|action.yml\|bucket"` → no output. The
  only version tests are `test_coverage_gaps.py:11` (`version.py` vs
  `__version__`) and `test_e2e_cli_scenarios.py:421`.
- **Recommendation**: `tests/test_version_consistency.py` parsing all eight
  files and asserting equality with `diffctx.__version__` — runs on every PR, so
  a hand-edit is caught before a release is dispatched. Fold in the `@v<semver>`
  pins in `docs/product/github-action.md:9,27`, which CD also never touches.
- **Verification**: bump `server.json` alone by hand; the test fails.
- **Confidence**: CONFIRMED (all eight currently read 1.12.2)

### 🟡 PID-keyed temp excludesFile: concurrent MCP calls silently drop all

`.diffctx/ignore` rules

- **Location**: `crates/diffctx-native/src/git.rs:720`
- **Evidence**: `let path =
  std::env::temp_dir().join(format!("diffctx-ignore-{}.tmp",
  std::process::id()));` — removed unconditionally at `:797`.
- **Problem**: the MCP server runs every tool body on a worker thread
  (`mcp/server.py:62`, `abandon_on_cancel=True`), so two overlapping
  `get_diff_context` calls — or one deadline-abandoned call plus the next —
  share one PID and one temp path. Whoever finishes first deletes the file; git
  tolerates a missing `core.excludesFile` silently, so `find_ignored_paths`
  returns a set missing every `.diffctx/ignore` match and an explicitly excluded
  file is rendered in full. The failure is further swallowed by
  `result.unwrap_or_default()`.
- **No coverage**: `grep -rn "asyncio.gather\|concurrent\|threading"
  tests/test_mcp.py` → zero hits. Every ignore test is single-call.
- **Recommendation**: key the temp file on PID + a process-local counter (or use
  a proper temp-file API), and add an integration test issuing two concurrent
  MCP calls on a repo with `.diffctx/ignore`, asserting both respect it.
- **Verification**: the concurrent test fails on today's code.
- **Confidence**: CONFIRMED (quote verified)

### 🟡 Remaining unverified paths (grouped; each has a named gap and no test)

| Area | Location | Gap |
|---|---|---|
| Nested `.diffctx/ignore` anchoring | `git.rs:646` | all fixtures are root-level; 4 reachable outputs of `anchor_diffctx_ignore_line`, 0 tests. Wrong either way is silent: too narrow leaks, too broad drops context |
| Non-ASCII / C-quoted diff paths | `git.rs:348` + `unquote_c_style` (74 lines) | with default `core.quotePath=true` a `src/café.py` change emits `--- "a/src/caf\303\251.py"`; the quoted branch is dead in every test |
| git subprocess timeout / kill | `git.rs:186` | `--timeout` is the user's only escape from a wedged `git diff`; `GitError::Timeout` appears in zero tests |
| PPR truncation flag | `ppr.rs:91` | renormalization hides truncation by design (the code says so); if `truncated = true` stops being set, seed-biased scores ship as converged |
| Self-loops / non-finite weights | `graph.rs:292` vs `:892` | the guard lives on `add_edge`, which has **no non-test caller**; the live path filters only `weight > 0.0`, admitting `INFINITY` (→ all-NaN scores → empty score map → core-only output, exit 0) |
| `IntervalIndex` boundary | `interval.rs:54` | the deliberate one-line tolerance holds in only one direction — `[10,20]` vs `[1,10]` is dropped, `[1,10]` vs `[10,20]` is kept. Identical geometry, outcome depends on greedy order. Zero tests |
| PPR / BM25 selection quality | `scoring.rs:257`, `yaml_cases.rs:238` | all 2725 cases hardcode `ScoringMode::Ego`; the only BM25 test asserts exit code and `type == "diff_context"`, so BM25 degrading to changed-files-only ships green |
| `drop_redundant_signatures` | `select.rs:98` | keyed on `(path, start_line)` with last-write-wins; a class header and the full class share a start line, so a small sibling can delete the stub that was the only affordable representation |
| Signature stubs | `select.rs:217`, `signatures.rs` | never rendered in any test (oracle budget ≥2.5× repo); 144 lines of bracket/decorator scanning with 0 tests, documented user-facing behavior at `cli.py:532` |
| Parse timeout / minified files | `tree_sitter_strategy.rs:1055` | the 2 s abort and the single-5MB-line path are untested; the changed file can vanish from output entirely with no warning |
| cmake / make grammars | `tree_sitter_strategy.rs:696,772` | compiled in but unreachable — `find_lang_config` keys on `.txt` for `CMakeLists.txt` and `""` for `Makefile`; `.cmake`/`.mk` are absent from `EXTENSION_TO_LANGUAGE` so they are never discoverable |
| `languages.rs` | `:213` | gates every candidate file in the repo; 0 tests. `.mdx` is in `MARKDOWN_EXTENSIONS` but not in `EXTENSION_TO_LANGUAGE` — two behaviors for one file type |
| `discovery.rs` | `:99`, `:213` | 0 tests. Test-file pairing, rare-identifier expansion and BM25 IDF each fail independently; the ensemble hides it and recall just drops |
| MCP over-budget guard | `mcp/server.py:214` | the branch deciding between "return 200k tokens" and "return nothing" never executes — fixtures are far under 25 000 tokens |
| MCP `clipboard=true` | `mcp/server.py:146` | hard-fails on a headless server, and `_over_token_budget_notice` actively recommends it as the remedy. Untested; the CLI degrades gracefully, MCP does not |
| MCP subdirectory rejection | `mcp/security.py:20` | `repo_path=<repo>/src` — the most natural thing an agent passes — is rejected as "Not a git repository". Bare repos too. Only tested against an empty `tmp_path` |
| `action-smoke` input coverage | `action-smoke.yml:53` | passes 4 of 9 inputs; `tau`/`alpha`/`timeout`/`output-path`/`fail-on-empty` sit behind `if [ -n … ]` guards that never fire. Also `action.yml:90 default: 1.12.2` means the smoke installs the *released* wheel, so a working-tree CLI change cannot fail this job at all |
| Scoop manifest | `cd.yml:764`, `scripts/render_packaging.py` | `render_scoop` hardcodes `"bin": "diffctx.exe"` against a 7z layout; pushed straight to `main` (which *is* the bucket) with no gate. `grep -rln "render_packaging" tests/` → no output |
| Mermaid label escaping | `analytics.rs:507` | `format!("    {nid}[\"{label}\"]")` unescaped, while the sibling GraphML exporter **has** `graphml_escapes_special_characters`. The mermaid text is a protocol — `graph_analytics.py:30` re-parses it — so a broken label degrades cycle detection into a silent false negative |
| POSIX backslash paths | `render.rs:238` | `.replace('\\', "/")` runs unconditionally, so a file literally named `src\utils.py` is reported as `src/utils.py` — a path that does not exist, which an agent may then edit at the wrong location |

### 🔵 Lower-value gaps

- **Token-cache cold/warm equivalence** (`token_corpus.rs:112`) — the stated
  contract covers symlinks, gitlinks, conflicted stages and dirty files, but the
  determinism test's fixture is all mode-100644/stage-0/clean, so all three
  bypass branches are dead. A regression here is second-run-only nondeterminism,
  i.e. never reproducible in fresh CI.
- **`filtering.rs:302`** — `result.sort_by(|a, b| a.id.cmp(&b.id));` is the
  single line making downstream tie-breaks deterministic after a `FxHashMap`
  iteration. Dropping it reintroduces hash-order selection among equal-scoring
  fragments, which the 3-file determinism fixture cannot see.
- **`generic.rs:32`** — emits a fragment for a whitespace-only file, unlike
  `markdown.rs:70` and `config_parser.rs:86` which both guard
  `snippet.trim().is_empty()`. Renders as an empty code block. Relatedly,
  `Cargo.toml:29` splits 39 grammars into `lang-core`/`lang-extra` and **no CI
  job builds `--no-default-features`**.
- **`analytics.rs:441`** — the `churn` half of the Rust hotspot score is dead
  (every caller passes `None`) and the score is discarded by Python, which
  recomputes with duplicated constants. The one Rust test asserts an ordering no
  user sees.
- **`pybridge.rs:117`** — `DiffContextResult` is registered but unconstructible
  from Python; its `to_serializable` hardcodes `commit_message: None,
  changed_files: Vec::new(), role: None`, so anyone who later wires it up ships
  silently lossy output. `docs/engineering/correctness.md:253` already notes it
  has no consumer.

---

## What I didn't touch

- `eval/**` and `tests/eval/**` — research harness, not shipped product surface.
- `paper/`, `results/`, `datasets/`, `test-repos/` — data and manuscript.
- Existing test *quality* (assertion strength inside tests that do exist) except
  where it directly explains a gap, e.g. the τ=0 oracle parameter and the
  `conftest.py` `-f yaml` injection.
- The rustfmt churn in the uncommitted `git.rs` diff — formatting only, out of
  scope for coverage.

## Self-contradiction check

No prior dated section — this is the first `/review-tests` run on this repo.
Nothing to reconcile.
