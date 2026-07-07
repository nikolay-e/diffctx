# UX Findings Log

## 2026-07-07 — a7f398f4

First-time-user CLI ergonomics audit (5 hands-on scouts: flag semantics,
tree mode, diff mode, graph subcommand, errors/edge cases). All fixes below
were applied and verified in this run (494 tests green, pre-commit clean).

### DO — fixed in this run

1. 🔴 Dead logging: import-time `NullHandler` made `setup_logging` skip
   attaching a real handler; all 19 `logger.error/warning/exception` sites
   were silent and `--log-level` was a no-op (`logger.py:23`). Fixed:
   NullHandlers excluded from the handler check.
2. 🔴 Silent data loss: `diffctx . -o /bad/path.md` exited 1 with no error
   text (swallowed by the dead logger). Fixed: `_handle_output_file` prints
   `error: cannot write '...'` to stderr directly (`main.py`).
3. 🔴 Help lied YAML-first: example `diffctx . → Map current directory to
   YAML` while the default is `md` (`cli.py:238` vs `default="md"`). Fixed:
   examples, description order, `--save` text.
4. 🔴 `--budget 0` help claimed "auto" but selects the recall floor; tiny
   budgets silently dropped even changed fragments with a message blaming a
   "clean working tree". Fixed: honest help (`0 = changed code only`),
   cause-specific empty-diff hints, and postpass now keeps the cheapest
   change-covering fragment when nothing fits (`postpass.rs`).
5. 🔴 Graph `--summary` noise: self-contradictory edge counts, degenerate
   top-referenced (all `in_degree=N-1` via `config_generic`), whole-repo
   "cycles" from bidirectional semantic edges, `churn=0` everywhere,
   duplicate bare mermaid labels. Fixed: category shares in %, degenerate
   lists suppressed, SCC over dominant-direction edges only (one-way import
   → "No dependency cycles detected"), churn from `git log --since`, labels
   disambiguated to relative paths, edge weights normalized to %.
6. 🟡 Decorated-definition stub rendered as bare `@dataclass` without the
   `class X:` line (`signatures.rs`, `tree_sitter_strategy.rs`). Fixed:
   stub = decorators + header.
7. 🟡 Git errors triple-wrapped (`git error: git command failed: git diff
   ... failed: fatal: ...`). Fixed: single-line message in Rust + `unknown
   git revision 'X'; check refs with: git log --oneline` in Python.
8. 🟡 `--max-depth` mislabeled pruned dirs as `_(empty directory)_` — a
   factual lie to the LLM reader. Fixed: `_(children omitted: --max-depth
   reached)_` / `truncated: true`.
9. 🟡 Silent flag conflicts. Fixed with warnings: `-q`+`--log-level`,
   `--max-file-bytes`+`--no-file-size-limit`, `--max-file-bytes`+
   `--no-content`, `--full`+selection flags, graph `--summary`+`-f`,
   graph `--level`+`json/graphml`.
10. 🟡 `--no-default-ignores` confusion (project .gitignore still applying)
    with no escape hatch. Fixed: honest help + new `--no-ignores` flag (all
    ignore rules off; explicit usage error with `--diff`).
11. 🟡 `-o out.json` wrote Markdown into a .json file. Fixed: format
    inferred from the `-o` extension when `-f` omitted; mismatch warning
    when both given.
12. 🟡 Custom validators exited 1 while argparse exits 2. Fixed: flag-value
    validation → exit 2; exit-code table added to `--help` epilog.
13. 🟡 Clipboard failure fell back to stdout silently (exit 0). Fixed:
    warning `clipboard unavailable (...); writing to stdout instead`.
14. 🟡 Mixed dir+glob args dropped the glob files' parent path in node
    names. Fixed: cwd-relative fallback in `_build_file_node`.
15. 🟡 Double Ctrl-C produced a ~60-line traceback. Fixed: SIGINT ignored
    inside the handler.
16. 🔵 Help-text polish: `--tau` semantics told honestly (stubs, not
    exclusion), `--scoring` described by outcome, `--alpha` "0-1 exclusive",
    metavars (`TOKENS`/`FLOAT`/`FILE`), `-c`/`-q`/`-o -` documented,
    `.diffctx/` lookup for `-i`/`-w` and their `--diff` incompatibility,
    epilog alignment, `graph` misplacement hints (`diffctx -q graph`, dir
    named `graph`), non-recursive glob hint, oversized-output hint mentions
    `--save`, graph mode prints the token summary, graph `-o` honors `-o -`.

### VERIFY

- Bare `--diff` on a clean tree exits 4 (kept deliberately for scripting;
  now documented in `--help` with a "try --diff HEAD~1" hint). Gate: if CI
  users report friction with non-zero "nothing changed", revisit exit 0 +
  `--fail-on-empty`.

### DON'T

- Rewrite `graph` as real argparse subparsers — the argv[0] dispatch now
  warns on the two confusing cases; a breaking CLI restructure isn't
  justified by what remains.
- `-o` overwrite confirmation — standard CLI behavior; `--save` targets are
  auto-ignored.
- Format-aware `--summary -f json` machine output — warning suffices until
  someone asks.
- Auto-ignoring stale non-default output files from previous runs —
  generically unsolvable.
- `--staged`/`--cached` diff scope — feature request, not a defect; revisit
  on demand.

### What I didn't touch

Benchmarks (`tests/benchmarks`, CI owns them), MCP server surface,
treemapper product layer, `docs/index.html` and stray jpeg files in repo
root (untracked, unrelated).
