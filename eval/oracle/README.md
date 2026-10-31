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
`indexes.jsonl`, `unmapped.jsonl`, `bench.json`, `report.md`, `report.json`.
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

## Pins and isolation

- `tools/package.json` pins the indexers exactly; `tools/package-lock.json`
  holds the integrity hashes; installs use `--ignore-scripts`.
- `corpus.toml` pins every repository at a commit and the `scip` CLI release
  with a SHA-256 per platform; the download is verified before it is used.
- Indexers and installers run with a scrubbed environment: no tokens, no user
  configuration, a throwaway home. `scip-python` is given an empty environment
  file, so no `pip` call and none of the repository's code runs. TypeScript
  dependencies are installed with scripts off; `--no-install` skips them.
- Git trees are exported with `git archive`: no hooks, no filters.
- The workflow has `contents: read` and no secrets.

The cache (`cache/`, ignored) is keyed by repository, commit, indexer and
indexer version.

## Adjudication and traces

`adjudicate.py` samples about 200 hard relations (dynamic dispatch, framework
registration, re-export, disagreements) into `adjudicated/<language>.jsonl` as
references: repository, commit, path, line, observed outcome and empty label
fields. `trace/python_profile.py` (`sys.setprofile` over the repository's own
tests) and `trace/ts_coverage.py` (V8 function coverage) produce the runtime
evidence a label needs. Run them in the same isolation as indexing.

## Limits

- Ranges in an unspecified position encoding are read as UTF-16.
- A constructor also takes the instantiations of its class.
- Only direct relations are truth; `tests_reach` is not derived.
- Dynamic dispatch, reflection and framework registration are outside what
  SCIP resolves; those cases are what `adjudicate.py` samples.
