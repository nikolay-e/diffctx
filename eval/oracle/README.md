# Oracle

An instrument that measures diffctx against an independent truth. It imports
nothing from diffctx or from the rest of `eval/`; diffctx runs only as a CLI
(`DIFFCTX_BIN`). It has its own `pyproject.toml`, `uv.lock` and tree-sitter.

## What it measures

For sampled commits `C` of pinned public repositories, it

1. indexes `C~1` and `C` with SCIP (`scip-python`, `scip-typescript`);
2. maps every hunk to the innermost enclosing definition by AST containment,
   on head for added and modified lines and on base for deleted lines, so a
   body-only edit counts (a SCIP definition occurrence marks only the name).
   A definition that reappears elsewhere or under another name with the same
   body is moved or renamed, not removed; an edit that changes no token is
   layout-only and is not scored;
3. takes the relations that reach each changed definition from the index:
   `calls`, `instantiates`, `passes_as_value`, `reads`, `writes`, `imports`,
   `reexports` and `overrides`, by syntactic context and SCIP role bits;
4. runs `diffctx . --diff C~1..C --mode impact -f json` and, for sampled
   definitions, `--symbol NAME -f json`, and scores each (target, relation):
   true and false positives, false negatives, abstention (an unknown, a limit
   or an unanalysed file is named) and false-complete-negatives (nothing
   found, nothing named, truth not empty).

Callers inside the diff (the caller is itself changed, or the site is in a
hunk) are not scored in either direction, because impact reports callers
outside the diff. Evidence `resolved` counts as a claim; `candidate` and
`name` are weak candidates, reported apart.

A snapshot whose index failed or covers less than `min_file_ratio` of its code
files is excluded and counted, never scored as empty. A changed definition no
index symbol sits on is counted as unmapped.

## Running

```sh
eval/oracle/run.sh --repo click --commits 5        # one repository, five commits
eval/oracle/run.sh --fast                          # the `oracle` label subset
eval/oracle/run.sh                                 # the full corpus
```

`DIFFCTX_BIN` defaults to `target/release/diffctx`. Results are written to
`eval/oracle/out/latest`: `cells.jsonl`, `relations.jsonl`, `commits.jsonl`,
`silent_loss.jsonl`, `indexes.jsonl`, `unmapped.jsonl`, `bench.json`,
`report.md`, `report.json`.
`report.py` also merges the YAML corpus report (`DIFFCTX_ORACLE_REPORT`) and
the metamorphic and fault outcomes (`ORACLE_METAMORPHIC_REPORT`,
`ORACLE_FAULTS_REPORT`; JSON, JSON lines or a `cargo test` log).

The scheduled run draws a random metamorphic stream (`DIFFCTX_METAMORPHIC_SEED`,
`DIFFCTX_METAMORPHIC_CASES`); locally set the same two variables before
`cargo test --release --test metamorphic` and pass its log as
`ORACLE_METAMORPHIC_REPORT`. The door test needs `DIFFCTX_NATIVE_BIN`; the
oracle does not.

Tests: `uv run --directory eval/oracle pytest`. Run it from this directory or
with `--directory`: the repository root has its own pytest configuration.
`DIFFCTX_BIN=... python run.py selftest` runs the engine against fixtures with
a body-only edit, a deletion and a move.

## Thresholds and the verdict

`gates.toml` holds every threshold the oracle judges by; no number lives in code.
`report.py` prints one verdict line per threshold with the numbers behind it (a
threshold with no input in the run is `N/A`, never a pass; `--gates FILE` reads
another file, `--ac5-head` and `--ac5-base` the AC5 scorer's `totals.json`).

`--against DIR` pairs the run with another on the same commits and resamples
over commits. A cell, one (language, mode, relation), is *measured* when the
95 % intervals of its precision and recall are at most `interval_width_max`
wide (its verdict line is `info`, "N of M cells measured": it sorts cells
and can never fail); a measured cell *fails* when the lower bound of a
measured interval is under `lower_bound_min`. Every other cell is
*unmeasured* and is judged on the safety rows alone: a statistic over a
handful of commits decides nothing. Each cell row
carries its commits, sites and the head's precision and recall.

## Cell states

The engine declares each (language, relation) cell `precise`, `heuristic` or
`none` (T16), and `cellstate.py` checks that each declared state is true
against the measurement; nothing is derived from a typed list. A cell whose
declaration is not true reads `untrue` with every reason named, and so does a
measured cell no declaration names (reason `undeclared`: the facts must cover
the registry).

- `precise` holds every `[precise]` condition of `gates.toml`: in every row of
  the cell, each mode and each paired run, head precision and recall at the
  floors and no incomplete complete answer (`safety.incomplete_complete`; the
  false complete negative is its special case and has no row of its own; a run
  that predates the count is not counted, never zero); a
  measured cell that does not fail against the base; at least
  `true_relations_min` true relations per paired run (the sites
  of all its modes: W7-D's floor of 30) and `decoys_min` decoys (10) in the
  cell; a fresh seed for the language; the replaced path deleted. Dropping a
  `[precise]` condition from `gates.toml` drops it from the rule.
- `heuristic` answers as candidates: its rows break no safety row (an incomplete
  complete answer or a false fold is a claim a candidate does not make). The
  oracle cannot see a row's label, so it does not check that.
- `none` is a named unknown, never a zero: the facts name what is unknown, the
  table shows no numbers for it, and its rows break no safety row.

The facts are the engine's, not a measurement's. The orchestrator supplies them
from the engine at the head as a JSON file (`tests/fixtures/cell_facts.json` is
the example):

```json
{
  "schema": "diffctx.cell-facts.v2",
  "engine": "a label or commit of the engine the facts were read from",
  "fresh_seed": { "<language>": "the seed that is fresh, as a witness" },
  "cells": {
    "<language>/<relation>": {
      "state": "precise",
      "old_path_deleted": "the commit that deleted the path the resolver replaced",
      "decoys": 14
    },
    "<language>/<relation>": { "state": "heuristic" },
    "<language>/<relation>": { "state": "none", "unknown": "what is not traced" }
  }
}
```

`fresh_seed` lists the languages whose measured run drew only commits made
after the code freeze. A witness is a non-empty string and is never a bare
yes; `unknown` is required for `none`; `decoys` counts the decoys the cell's
corpus cases hold. A language not in `fresh_seed` is not fresh; a precise cell
without `old_path_deleted` or enough `decoys` is untrue. (v1, which had no
declared state, is refused.)

`report.py --cell-facts FILE` (with `--against`) prints the `precise.*` and
`declared.every_state_true` verdict lines from `cellstate` and a cell-state
table; without the file they stay `N/A`. A `precise.*` line is judged over the
cells declared precise: a floor shows the weakest value among them, a required
condition is true when none of them misses it.

```text
python cellstate.py --report out/report.json --facts facts.json \
    [--gates FILE] [--json states.json] [--readme [README.md]]
```

`--report` is a `report.py --against` output; without `--readme` the table
goes to stdout. `--readme` rewrites the table between the `cell-states`
markers, appending the section the first time: the table is generated (never
edited by hand) after the fresh seed.

## Safety rows read from the cells

`report.py` counts an *incomplete complete* per cell (an answer complete for a
definition and relation that lists none of a truth site it can reach; a false
complete negative is the case where it lists no caller at all) and lists every
pair. A *silent loss* is a changed definition with no entry, no fold row naming
it and no disclosure of its file; `measure.py` writes one `silent_loss.jsonl` row
per scored impact answer, counted per answer: the definitions it lost, less the
`unnamed_omitted` it says it shed without a name (never below 0). Every missing
definition is listed with that count beside it and with what classifies it, an
entry of its file or name under another key being an identity mismatch and none
an engine loss. A run without `silent_loss.jsonl` or the `incomplete_complete`
column (an older run) leaves those lines `N/A`; an answer that lost nothing
still has its row, so a zero is a witnessed zero. The silent-loss gate is `read`:
each row is read and classified.

## Pins and isolation

- `tools/package.json` pins the indexers exactly; `tools/package-lock.json`
  holds the integrity hashes; installs use `--ignore-scripts`.
- `corpus.toml` pins every repository at a commit and the `scip` CLI release
  with a SHA-256 per platform; the download is verified before it is used.
- Indexers and installers run with a scrubbed environment: no tokens, no user
  configuration, a throwaway home. `scip-python` is given an empty environment
  file, so no `pip` call and none of the repository's code runs, and a
  `pyrightconfig.json` makes it read every file (a repository's own
  `[tool.pyright] include` otherwise narrows the index to a third of it).
  TypeScript is indexed under a tsconfig the oracle writes over the
  repository's (kept as `tsconfig.repo.json`, and extended so module
  resolution stays) that includes every script file; the repository's own
  `include`, `references` or lack of a tsconfig left JavaScript and workspace
  packages out. TypeScript
  dependencies are installed with scripts off; `--no-install` skips them.
- Git trees are exported with `git archive`: no hooks, no filters.
- The workflow has `contents: read` and no secrets.

### Languages

Python, TypeScript/JavaScript, Rust and Kotlin; Go returns with its cell. A
repository that cannot be fetched or indexed is counted in `commits.jsonl` and
does not fail the round; `measure` exits non-zero only when no commit of a
selected language was measured.

Rust and Kotlin are indexed by release binaries pinned in `corpus.toml` with a
SHA-256 per build: rust-analyzer (`rust-analyzer scip`) and scip-java. Each
plan (`runners.py`) fetches dependencies in a step that runs none of the
repository's code, then runs the indexer with as little of it as the indexer
allows.

- **Rust.** `cargo fetch --locked`, then rust-analyzer offline with its
  build-script command replaced by `true` and proc macros off, so no
  `build.rs` or proc macro runs. The repository's `.cargo/` and
  `rust-toolchain*` are removed from the exported tree (a copy; no measured
  project is edited, and none gets a `[workspace]` table). The cache lives
  outside every Cargo workspace (`$ORACLE_CACHE`, default
  `~/.cache/diffctx-oracle`): cargo refuses a package that "believes it is in a
  workspace when it is not", which a cache under a checkout's root triggers. A
  failed fetch keeps the end of cargo's message in the index record. Code that build
  scripts or macros generate is missing from the index; the completeness
  record shows it, and the standard library's symbols are not in it (the
  resolved ratio of a Rust index is about a third).
- **Kotlin.** scip-java indexes Kotlin only by running the repository's Gradle
  build, which executes its build scripts, plugins and downloaded code. No
  option avoids that, so Gradle never runs on the machine that started the
  oracle: the plan starts it in a throwaway container (`docker run`, the JDK
  image pinned by digest in `runners.py`) with only the snapshot, the
  launcher, the output directory and the Gradle cache mounted and none of the
  host's home or environment. When the oracle itself runs inside a container
  and `ORACLE_SANDBOX=1` says so, it runs in place; the variable alone, where
  no container is detected (`/.dockerenv` or the cgroup), is refused outright.
  Without Docker the repository is skipped as `needs_sandbox`, counted and
  never scored as empty. scip-java
  0.14 is required: 0.13.2's Kotlin support failed on Gradle 9 builds
  ("Querying the mapped value of map(...) before task ... has completed") and
  was the "indexer exited 1" of the first CI run. A repository whose tests
  live in included builds (`includeBuild`) is indexed by scip-java without
  them and falls below the completeness ratio; the corpus avoids those.

The cache (`$ORACLE_CACHE`, default `~/.cache/diffctx-oracle`) is keyed by
repository, commit, indexer and indexer version.

## Adjudication and truth filters

`adjudicated/<language>.jsonl` holds the evidence-backed verdicts on 196
sampled relations (references only: repository, commit, caller, definition,
SCIP's kind and the decided `final`). `report.py` applies them over the raw
SCIP numbers and prints both tables, plus how often SCIP's own relation
survived: a kind relabels a relation, `none` removes it, `unresolved` takes it
out of precision and recall. `adjudicate.py` samples the next set from a run:
real disagreements (verdict from the run, not a random relation) and the hard
shapes, skipping relations already adjudicated.

`filters.py` drops, and counts, SCIP references the adjudication found wrong
for Python and TypeScript/JavaScript: a site whose identifier is not the name
of what it references (a sibling class's constructor, a value built by an
export), a local or parameter that shadows the name, and a wrapper told from
the function it calls by the files the caller reaches. The counts are in
`commits.jsonl` and the report.

## Adjudication

`adjudicate.py` samples about 200 hard relations (dynamic dispatch, framework
registration, re-export, disagreements) into `adjudicated/<language>.jsonl` as
references: repository, commit, path, line, observed outcome and empty label
fields.

## Limits

- Ranges in an unspecified position encoding are read as UTF-16.
- A constructor also takes the instantiations of its class.
- Only direct relations are truth; `tests_reach` is not derived.
- Dynamic dispatch, reflection and framework registration are outside what
  SCIP resolves; those cases are what `adjudicate.py` samples.
