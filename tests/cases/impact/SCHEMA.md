# Impact and symbol case schema v1

A case builds a real git repository, runs the shipped binary on it and
compares its JSON answer with a set of typed relations. The harness is
`crates/diffctx-native/tests/yaml_cases.rs` with `tests/common/oracle/`; the
cases sit in `tests/cases/impact/<language>/` and
`tests/cases/symbol/<language>/`. `<language>` is one of `python`,
`typescript` (JS included), `rust`, `kotlin`, `java`, `swift`, `go`, `shell`,
`config`, `web`, `sql`, `other`.

A case's trial name is `<mode>/<language>/<file stem>`; its stratum is
`<mode>/<language>`, so `DIFFCTX_YAML_CASES_LIMIT` keeps every language alive.

```yaml
name: py_body_only_change_reaches_callers  # must equal the file stem
mode: impact        # impact | symbol; must equal the directory
source: gh#888      # gh#N | fj#N | synthetic (one; more issues in tags)
issue_state: closed # open | closed at seeding; an open one is expected red
tags: [python, body-only, smoke]  # `smoke`: the post-release subset
repo:
  initial_files: {path: content}
  changed_files: {path: content}
  deleted_files: [path]
  renamed_files: {old: new}  # applied first; changed_files overlays the move
  untracked_files: {path: content}  # written after the commit, never added
  worktree: false   # true: the change stays uncommitted (`--diff HEAD`)
  commit_message: "..."
query:
  symbol: total     # symbol mode: a bare name, PATH:NAME, or SQL schema.name
  args: ["--budget", "4000"]  # never --diff, --mode, -f/--format, --symbol, -q
expect: {}          # below
dispute: "one line" # the harness skips the case until the orchestrator decides
```

Impact mode runs `--diff <base>..HEAD --mode impact` (`--diff HEAD` for
`worktree: true`); symbol mode runs `--symbol NAME`. Both read `-f json -q`. A
symbol case may carry only `initial_files`. A `worktree: true` case with only
`untracked_files` is valid. Unknown YAML keys are an error.

## Binary

`DIFFCTX_BIN=<path>` runs that binary (the released `diffctx` from PyPI, npm, a
plugin install); without it the suites' own binary runs (`DIFFCTX_NATIVE_BIN`
or the cargo-built one). The post-release smoke runs the `smoke` cases this
way.

## Selecting cases

- `DIFFCTX_CASE_TAG=<tag>` runs only the impact and symbol cases carrying that
  tag and skips the in-process `diff` corpus (and `modes_shipped_have_cases`).
  With `DIFFCTX_BIN` it is the post-release smoke:
  `DIFFCTX_CASE_TAG=smoke DIFFCTX_BIN=... cargo test --test yaml_cases`.
- `-- --list` prints `[yaml] <name>: test` per case; `<name>` is the path
  without extension, relative to `tests/cases/` for impact and symbol
  (`impact/python/<stem>`) and to `tests/cases/diff/` for diff.
- `DIFFCTX_YAML_IGNORE_BASELINE=1` judges every case on its own: a red case
  fails whether or not it is listed (the class-test gate uses it).

## Expectation

```yaml
expect:
  changed: [shop/pricing.py::total]  # definitions touched, body-only included
  relations:
    - {from: shop/checkout.py::charge, to: shop/pricing.py::total,
       kind: calls, resolution: resolved, site: 7}
    - {from: tests/test_checkout.py, to: shop/checkout.py::charge,
       kind: tests_reach}
  exact: true    # default; false: listed relations must be present
  decoys: {shop/pricing.py::total: [legacy/pricing.py, docs/notes.md]}
  coverage: {shop/pricing.py::total: {calls: complete}}
  unknowns: {run: not_modelled}
  definitions: [shop/pricing.py::total]  # symbol mode: definitions resolved
  callers: {total: [shop/checkout.py::charge]}
  tested_by: {shop/checkout.py: tests/test_checkout.py}
  untested: [total]
  not_untested: [run]
  stale: [ops.md]
  not_stale: [README.md]
  named_by: [Dockerfile]
  not_named_by: [notes/transcript.md]
  contracts: [api/schema.py]
  not_contracts: [tests/test_api.py]
  removed: [legacy]
  never_empty: true
  discloses: true
  no_negative_claims: true
```

Everything is optional; an absent field asserts nothing.

### Ids

- A definition is `path::name`; a member may be written
  `path::Container.name` (the engine names a member by its bare name, and
  either spelling matches). A file is a bare `path`. Top-level code of a file
  is the bare path in `from`.
- A bare path covers every definition in the file; a name the expectation
  gives must be matched by a definition (an import line is never
  `path::function`).
- In the convenience maps (`callers`, `decoys`, `coverage`, `unknowns`) and in
  `changed`, `untested`, `not_untested`, `removed`, a bare identifier is a
  symbol wherever it is defined and anything else is a path. In `relations`,
  a bare string is a path.

### Relations

- `kind`: `calls`, `reads`, `writes`, `imports`, `reexports`, `overrides`,
  `instantiates`, `passes_as_value`, `configures` (from the configurer to the
  configured), `executes`, `references_path`, `tests_reach` (test function to
  its direct callee; transitive reach is asserted with `tested_by` or
  `not_untested`).
- `resolution`: `resolved` (default), `candidate` ("possibly called"),
  `unresolved` (named only textually: a word match, or a file that spells the
  name but was not fully analysed).
- `site`: a line of the reference in the `from` file; asserted whenever
  present. Omit it when a caller has several sites. Several relations with the
  same definition `from` and `to` pool their sites into one.
- A relation set is compared per (target, family), the families being
  references (code kinds and `references_path` together, on from, to and
  resolution: today's engine does not say which kind it found, and its
  `named_by` rows are always path references) and tests (`tests_reach`). A
  test's `from` is judged by its file: the engine names the test file that
  reaches a caller, so `tests/x.rs::test_fn` matches a `tests/x.rs` row.
  Caller rows that are only word matches (`evidence: name`) are the answer's
  textual section and yield no relation; a contract row of kind `removed_api`
  counts as a removal; `collection_reads` yield `reads` from the reading file
  to the changed files under `content/<collection>/`.
- With `exact: true`, an extra or missing relation to a listed target fails
  and the failure shows the relation diff. A listed target with no relation
  (`callers: {x: []}`) asserts there are none. A `from` naming a definition
  stands for one caller; a `from` naming a file stands for every caller in
  that file.
- Kind strictness follows the registry. No cell is `precise` yet, so `kind` is
  not compared: a mismatch is written to the oracle report as information.
  Once a cell is `precise`, `kind` is strict for its cases; no case is
  rewritten.
- Symbol mode, a name no file defines: a relation's `to` may be the bare
  queried name, resolution `candidate`, matched against the answer's
  `symbol_named.rows`.

### Lists and flags

- `changed` and `definitions` are sets: with `exact`, an extra definition
  fails. `changed: []` means exactly none. A bare file path names the file's
  own row, not the definitions in it (a new script's module-level names are
  one change: `changed: [analysis/one_off.py]`).
- `removed: []` and `unknowns: {}` mean exactly none; a non-empty list or map
  asserts membership.
- `unknowns` classes: `not_modelled`, `dispatched`, `ambiguous`,
  `common_word`, `unresolved_reads`, `overridable`. Keys are a bare name or a
  path.
- `coverage` per (target, relation kind): `complete` holds when the answer
  names no unknown, gap, limit or unanalysed file for the target (and, for
  `tests_reach`, tests are in view); `partial` otherwise. The engine has no
  per-relation coverage yet, so the kind picks only the test rule.
- `stale`, `named_by`, `contracts` and their `not_` forms are membership of
  paths in the answer's `stale_references`, `named_by` and `contracts`; for
  exact sets use `relations`.
- `untested` / `not_untested` read `changed[].untested` and the answer's
  untested lists. A definition the answer does not list at all is not
  "untested".
- `never_empty` fails when `empty` is true. `discloses` needs a named limit,
  unanalysed or withheld file, unknown or textual-only name.
  `no_negative_claims` runs `-f md` too and fails on an absence claim
  ("nothing calls them", "No resolved static callers", "No changes to
  analyse", a count of `0 caller(s)` beside a note that callers were not
  traced).

## Text form

Every impact case also renders `-f md`, and the text must say what the JSON
says: the header's "N caller(s) outside the diff" equals the number of
resolved caller rows (word matches, `evidence: name`, are no callers; candidates
are counted apart as "possible"), and a "called from" or "referenced from"
line needs a resolved row behind it. A violation fails the case.

## Baseline

`crates/diffctx-native/tests/known_below_threshold.txt` lists every red case
as `impact/<language>/<name>` (or `symbol/...`), with a trailing `# gh#N open`
or a reason. It is enforced both ways: an unlisted failure fails, and a listed
case that passes fails. A case that does not parse or build is an error
whatever the baseline says. A `dispute:` case is skipped and reported as
`disputed`.

## Report

`DIFFCTX_ORACLE_REPORT=<path>` writes
`{"schema": "diffctx.oracle-report.v1", "cases": [...]}`: per case the mode,
language, source, issue state, tags, state (`pass`, `fail`, `baselined_red`,
`stale_baseline`, `disputed`), per-target TP/FP/FN by relation, the unknowns
named, `empty`, `disclosed`, absence claims, kind mismatches and failures.

## Dump

`DIFFCTX_ORACLE_DUMP=<dir>` writes, per case,
`<dir>/<mode>/<language>/<name>.json` (the raw JSON answer), `.md` (when the
case runs the text form) and `.repo.txt` (`git log --stat` and
`git status --short` of the materialised repository).

## Running

```sh
cargo test --release --test yaml_cases -- \
  impact/ symbol/ modes_shipped_have_cases
```

`modes_shipped_have_cases` reads the shipped modes from `src/main.rs` and
fails when one has no corpus; `locate` shares the selection corpus of `pack`
until it has cases of its own.

## Examples

A body-only change (the definition is listed although no signature moved):

```yaml
name: py_body_only_change_reaches_callers
mode: impact
source: synthetic
tags: [python, body-only]
repo:
  initial_files:
    shop/pricing.py: |
      def total(items):
          return sum(i.price for i in items)
    shop/checkout.py: |
      from shop.pricing import total


      def charge(cart):
          return cart.pay(total(cart.items))
  changed_files:
    shop/pricing.py: |
      def total(items):
          return round(sum(i.price for i in items), 2)
expect:
  changed: [shop/pricing.py::total]
  relations:
    - from: shop/checkout.py::charge
      to: shop/pricing.py::total
      kind: calls
      site: 5
```

A symbol lookup with a decoy:

```yaml
name: py_same_stem_modules_symbol
mode: symbol
source: gh#343
tags: [python, decoy]
repo:
  initial_files: {...}
query: {symbol: total}
expect:
  definitions: [shop/pricing.py::total, legacy/pricing.py::total]
  decoys: {shop/pricing.py::total: [legacy/invoice.py]}
```

Real cases:
`tests/cases/impact/python/py_same_stem_modules_do_not_cross_attribute.yaml`,
`tests/cases/symbol/python/py_same_stem_modules_symbol.yaml`.
