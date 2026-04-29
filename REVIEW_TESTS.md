<!-- markdownlint-disable -->

# Review: Test Coverage Gaps

Audit framework: Review Pyramid (R1 fact-gathering → R2 analysis → R3 prosecution/defense → R4 verdict).

Goal: find user-facing flows, edge cases, and failure modes in production code that no test catches.

## Round 1 — MCP server mode

**Exposed tools** (all in `src/treemapper/mcp/server.py`):

- `get_diff_context` — git diff analysis with token budget
- `get_tree_map` — full directory YAML/Markdown snapshot
- `get_file_context` — glob-pattern file reader

**Tested**: Only `get_diff_context` has any test coverage (`tests/test_mcp.py`, 7 test cases). `get_tree_map` and `get_file_context` have **zero tests**.

**M1**

- Severity: 🔴
- Scenario: `get_tree_map` and `get_file_context` are completely untested — happy paths, error paths, all edge cases.
- File:line: `server.py:89-217` — both tool implementations
- Why no test catches it: `tests/test_mcp.py` only imports and exercises `get_diff_context`; no call to `server.call_tool("get_tree_map", ...)` or `server.call_tool("get_file_context", ...)` exists anywhere in the test suite.
- Evidence: `grep -n "get_tree_map\|get_file_context" tests/test_mcp.py` returns no results.

**M2**

- Severity: 🔴
- Scenario: `get_file_context` with glob pattern `../../etc/passwd` — `validate_dir_path` calls `Path.resolve()` but never checks that the glob expansion stays within `validated_path`. `globmod.glob(str(validated_path / "../../etc/passwd"), recursive=True)` resolves the base but the `/../` traversal happens inside Python's glob, yielding files outside the root.
- File:line: `server.py:150-153`, `security.py:20-24` — glob pattern is user-controlled, no containment check on matched paths
- Why no test catches it: No path-traversal test exists for `get_file_context`; `TREEMAPPER_ALLOWED_PATHS` guard only applies to the `repo_path` argument, not to glob-expanded results.
- Evidence: `grep -rn "traversal\|relative_to\|is_relative_to" src/treemapper/mcp/` returns nothing for glob results.

**M3**

- Severity: 🟡
- Scenario: `get_diff_context` with `budget_tokens=0` or negative integer — no validation; `build_diff_context` receives `budget_tokens=0`, likely returning an empty/degenerate result with no error signal to the caller.
- File:line: `server.py:36-41` — `budget_tokens: int = 8000`, no lower-bound guard
- Why no test catches it: `test_budget_is_respected` uses 200 and 8000; zero and negative values are never passed.
- Evidence: `grep -n "budget_tokens" tests/test_mcp.py` shows only values 200 and 8000.

**M4**

- Severity: 🟡
- Scenario: `get_tree_map` with `subdirectory` containing path-traversal sequences (e.g. `"../../../etc"`) — `target = validated_path / subdirectory` then `target.is_dir()` is checked, but there is no check that `target` is still under `validated_path`. A valid absolute path outside the repo could be reached.
- File:line: `server.py:104` — `target = validated_path / subdirectory if subdirectory else validated_path`; no `target.is_relative_to(validated_path)` assertion
- Why no test catches it: No test passes a `subdirectory` argument at all, let alone a traversal one.
- Evidence: `_check_allowed` in `security.py` is called only on `repo_path`, not on the resolved `target`.

**M5**

- Severity: 🟡
- Scenario: `get_tree_map` with `output_format="invalid_format"` — `tree_to_string` is called with an arbitrary string; no test verifies the error type or message surfaced back through MCP.
- File:line: `server.py:119` — `tree_to_string(tree, output_format)` with caller-supplied string
- Why no test catches it: No test for `get_tree_map` exists; `output_format` validation (if any) is in `tree_to_string`, untested via MCP layer.
- Evidence: `grep -n "output_format" tests/test_mcp.py` returns nothing.

**M6**

- Severity: 🔵
- Scenario: Capability / version handshake not tested — FastMCP handles `initialize` automatically, but no test verifies that the server correctly advertises its tool list, or that an `initialize` with a mismatched `protocolVersion` is rejected gracefully.
- File:line: `server.py:18` — `mcp = FastMCP("treemapper")`; no custom capability hooks
- Why no test catches it: All tests call `server.call_tool(...)` directly, bypassing the JSON-RPC transport layer entirely. No test sends a raw `initialize` request.
- Evidence: `grep -n "initialize\|capabilities\|protocolVersion" tests/test_mcp.py` returns nothing.

**M7**

- Severity: 🔵
- Scenario: CRLF injection in `diff_range` — a caller passes `diff_range="HEAD~1..HEAD\r\nmalicious"`. This reaches `subprocess` in `build_diff_context` as a git argument; git will reject it, but the error response format (clean MCP `ToolError` vs raw Python traceback) is untested.
- File:line: `server.py:44-47` — `diff_range` passed directly to `build_diff_context` with no sanitization
- Why no test catches it: `test_invalid_diff_range` uses a well-formed but non-existent ref; no test uses control characters or multi-line strings.
- Evidence: `grep -n "\\\\r\\|\\\\n\|CRLF\|control" tests/test_mcp.py` returns nothing.

---

## Round 1 — CLI flag combinations

**F1**

- Severity: 🔴
- Scenario: User runs `treemapper . --budget -2` (negative value other than -1), expecting an error, but no test exercises the `_validate_budget` rejection branch.
- File:line: `cli.py:42-43` — `if budget is not None and budget < -1: _exit_error(...)`
- Why no test catches it: All test files (`test_cli.py`, `test_errors.py`, `test_options.py`) test `--max-depth` and `--max-file-bytes` negative rejection but contain zero invocations of `--budget` with an invalid value. `grep -rn "budget" tests/test_errors.py` returns nothing.
- Evidence: `tests/test_errors.py` lines 94–105 cover `--max-depth -1` and `--max-file-bytes -1` but no equivalent for `--budget -2`.

**F2**

- Severity: 🟡
- Scenario: User runs `treemapper . --save -o out.yaml`, which is supposed to be rejected as mutually exclusive; no test verifies the error message or exit code.
- File:line: `cli.py:108-109` — `if save and output_file_arg is not None: _exit_error("--save and -o/--output-file are mutually exclusive")`
- Why no test catches it: `test_options.py` tests `--save` alone and `-o` alone; the combined form is never passed to any subprocess or `parse_args` call in any test file.
- Evidence: `grep -rn "save.*-o\|-o.*save\|mutually exclusive" tests/` returns no results.

**F3**

- Severity: 🟡
- Scenario: User passes diff-mode flags (`--budget`, `--scoring ppr`, `--full`, `--alpha`, `--tau`) without `--diff`; the code emits a warning but never errors — no test confirms the warning text or that exit code stays 0.
- File:line: `cli.py:367-383` — `_warn_diff_only_flags` prints to stderr and returns without `sys.exit`.
- Why no test catches it: No test file constructs a call with e.g. `[".", "--budget", "500"]` (no `--diff`) and asserts on stderr content or exit code.
- Evidence: `grep -rn "_warn_diff_only_flags\|diff.*flag.*ignored\|ignored without" tests/` returns no results.

**F4**

- Severity: 🟡
- Scenario: User runs `treemapper . --diff HEAD@@` or `--diff ..` (malformed git range) — the range string is passed straight to the git subprocess; no test exercises invalid/malformed diff-range strings through the CLI.
- File:line: `cli.py:326-330` — `--diff` argument is stored as `args.diff_range` with no format validation before dispatch.
- Why no test catches it: `test_diffctx_invariants.py` uses well-formed ranges (`HEAD~1..HEAD`); no test passes a syntactically broken range and asserts on the error output.
- Evidence: `grep -rn "HEAD@@\|\.\.\b\|malformed" tests/` returns no results.

**F5**

- Severity: 🔵
- Scenario: User combines `-c` (copy to clipboard) with `-o output.yaml`; the code is supposed to both copy and write the file, but no test verifies the dual-output path.
- File:line: `cli.py:107-123` — `_resolve_output_file` allows `copy=True` alongside a non-None `output_file_arg` without conflict; both are stored independently in `ParsedArgs`.
- Why no test catches it: `test_clipboard.py` tests clipboard alone; `test_cli.py` tests `-o` alone; the combination is never exercised.
- Evidence: `grep -rn "\-c.*-o\|-o.*\-c\|copy.*output" tests/test_cli.py tests/test_clipboard.py` returns no results.

---

## Round 1 — Output format integrity

**O1**

- Severity: 🔴
- Scenario: File content ending with two or more consecutive newlines (e.g. `"line\n\n\n"`) is silently truncated to a single trailing newline in YAML output.
- File:line: `writer.py:77` — `content.rstrip("\n").split("\n")` strips all trailing newlines; the YAML `|2` block clip-chomping then adds back exactly one. Any file ending with a blank line loses it on round-trip.
- Why no test catches it: `test_write_tree_yaml_multiline_content` and `test_whitespace_only_content` only use single trailing newlines. No test constructs `"line\n\n"` and asserts the parsed value equals the original.
- Evidence: `write_tree_yaml` on `"line\n\n"` → `yaml.safe_load` returns `"line\n"`.

**O2**

- Severity: 🔴
- Scenario: A tree node whose `content` contains a lone Unicode surrogate (e.g. `"\ud83d"`, produced by `surrogateescape` when reading malformed UTF-8) causes `write_tree_json` to raise `UnicodeEncodeError` when written to a UTF-8 file.
- File:line: `writer.py:127` — `json.dump(..., ensure_ascii=False)` serialises surrogates into `StringIO` without error; `_write_to_file_path` (`writer.py:370`) opens the temp file with `encoding="utf-8"`, which rejects surrogates on `.write()`.
- Why no test catches it: `test_write_tree_json_unicode` only uses valid Unicode (Cyrillic). No test exercises surrogate-escaped file content through the JSON path.
- Evidence: Writing `"\ud83d"` via `write_tree_json` to a UTF-8 file raises `UnicodeEncodeError: surrogates not allowed`.

**O3**

- Severity: 🟡
- Scenario: CRLF line endings (`\r\n`) inside file content pass through the `txt` formatter verbatim — each line already ends in `\r`, then `file.write(f"…{line}\n")` appends another `\n`, producing `\r\n\n` pairs. Terminals that interpret `\r` as carriage return display garbled output.
- File:line: `writer.py:152-153` (`_write_tree_text_node`) and `writer.py:177-178` (`_write_text_fragment`) — no `\r` stripping before the `\n` append.
- Why no test catches it: All tests use LF-only content. No test writes `"line1\r\nline2\r\n"` and asserts the `txt` output is free of bare `\r`.
- Evidence: `write_tree_text` on `"line1\r\nline2\r\n"` produces lines ending in `\r` — confirmed by `'\r' in result`.

**O4**

- Severity: 🟡
- Scenario: The diffctx formatters emit duplicate fragments without deduplication: two identical `{path, lines}` entries in `tree["fragments"]` produce two identical code blocks in all three output formats (YAML, TXT, Markdown), with no consumer signal.
- File:line: `writer.py:115-119`, `190-192`, `308-310` — all three format writers iterate `tree["fragments"]` with no seen-set.
- Why no test catches it: `test_diffctx_invariants.py` tests relevance and ranking but never constructs a tree with intentionally duplicated fragment entries and asserts each unique `(path, lines)` appears at most once.
- Evidence: `write_tree_text` with two identical fragment dicts emits `foo.py:10-20` twice — `result.count('foo.py:10-20') == 2`.

**O5**

- Severity: 🔵
- Scenario: A filename containing Unicode BiDi control characters (e.g. U+202E RIGHT-TO-LEFT OVERRIDE) is emitted unescaped into Markdown headings (`## file‮gnp.txt`). Renderers and terminals honouring BiDi will display the filename in reversed visual order, making the heading misleading.
- File:line: `writer.py:293` (`_write_markdown_fragment`) and `writer.py:225` (`_write_md_header`) — neither applies BiDi stripping or escaping before writing `## …` headings.
- Why no test catches it: No test in `test_markdown_format.py` or `test_output_formats.py` uses a filename containing U+202E or any other BiDi control character.
- Evidence: `write_tree_markdown` with `name = "file\u202egnp.txt"` writes `## file\u202egnp.txt` verbatim — confirmed by heading-line inspection.

## Round 1 — Concurrency / determinism gaps

**C1**

- Severity: 🔴
- Scenario: `EnsembleDiscovery::discover` collects three parallel strategy results via `par_iter().collect()`, then merges in Vec iteration order. Rayon does not guarantee collection order matches logical strategy index order; under a different thread schedule the merge loop (`discovery.rs:284`) processes strategies in a different sequence, changing which duplicate paths are suppressed and thus which files reach the graph builder.
- File:line: `diffctx/src/discovery.rs:276-290`
- Why no test catches it: YAML case runner only asserts on presence of expected fragments, never on absence of fragments that vary with strategy-merge order. Single-threaded CI runs are ordered by accident.
- Evidence: `par_iter().collect()` on `Vec<Box<dyn DiscoveryStrategy>>` with no subsequent sort; merge loop at line 284 is order-sensitive with a first-seen deduplication invariant.

**C2**

- Severity: 🟡
- Scenario: In `lexical.rs` pass 3, TF accumulation uses `FxHashMap<u32,u32>` (line 149). `rustc-hash` randomizes its seed per-process, so iteration order over the `tf` map when building the sparse `vec` differs between runs. The `sort_unstable_by_key` at line 171 stabilizes final vector order, but the floating-point `norm` is computed *before* the sort via a sum over an arbitrarily-ordered iterator — float addition is non-associative, so the L2 norm and the normalized weights can differ by ±ULP across runs. This can flip edge-weight comparisons in `edges/mod.rs:108` that use `>` on `f64`.
- File:line: `diffctx/src/edges/similarity/lexical.rs:149-170`
- Why no test catches it: No test compares cosine similarity scores across two independent pipeline runs on the same input; garbage-injection tests only check include/exclude, not score values.
- Evidence: `FxHashMap` iteration is process-seed-dependent; `vec.iter().map(|(_, w)| w*w).sum::<f32>()` accumulates in that random order before sort.

**C3**

- Severity: 🟡
- Scenario: `build_file_cache` in `pipeline.rs` sorts results after parallel collection (correct), then applies a byte-budget cutoff that silently drops files beyond the limit. No test ever triggers this truncation, so a future refactor to `sort_unstable_by` would introduce non-determinism at the boundary — two files with the same path prefix sort ambiguously — without any failing test.
- File:line: `diffctx/src/pipeline.rs:390-400`
- Why no test catches it: All YAML test cases use small synthetic files far below `GRAPH_FILTERING.max_cache_bytes`; the truncation branch is dead code in the entire test corpus.
- Evidence: `cache_bytes > GRAPH_FILTERING.max_cache_bytes` break condition at line 395 never fires in tests.

**C4**

- Severity: 🔵
- Scenario: `GIT_TIMEOUT_SECS` is a process-global `AtomicU64` written by `set_git_timeout`. The YAML test harness runs cases with `par_iter` (line 442). If any future test calls `set_git_timeout` mid-run to test timeout behavior, all concurrently executing cases silently adopt the new value without any coordination. `Ordering::Relaxed` provides no happens-before guarantee beyond atomicity.
- File:line: `diffctx/src/git.rs:15-23`, `diffctx/src/test_harness.rs:441-444`
- Why no test catches it: No existing test calls `set_git_timeout`; the hazard is latent and would only manifest when a timeout-behavior test is added to the parallel suite without a thread-local override mechanism.
- Evidence: `Ordering::Relaxed` store at `git.rs:18`; parallel case runner at `test_harness.rs:442` shares the same process global.

---

## Round 1 — Diff/git pipeline edge cases

**Scope:** `diffctx/src/git.rs`, `diffctx/src/pipeline.rs`, `src/treemapper/diffctx/pipeline.py`, `diffctx/src/pybridge.rs`

**G1**

- Severity: 🔴
- Scenario: Empty repo (zero commits) — `git diff` fails with `fatal: bad default revision 'HEAD'`.
- File:line: `git.rs:307` `parse_diff` → `run_git` returns `GitError::CommandFailed`; `pybridge.rs:287` re-maps to `PyRuntimeError`. No test creates a zero-commit repo and calls `build_diff_context`.
- Why no test catches it: `framework/pygit2_backend.py` always commits initial files before running. `test_mcp.py:107` uses a nonexistent ref but still requires at least one prior commit.
- Evidence: `grep -rn "empty.*repo\|zero.*commit" tests/` returns nothing relevant.

**G2**

- Severity: 🔴
- Scenario: Revision not reachable from any local ref — `parse_diff` succeeds, but `CatFileBatch::get` returns `CommandFailed("Path not found")` for every file. `pipeline.rs:121-127` calls `process_files_for_fragments` which silently swallows these errors via `filter_map`, producing empty output with no diagnostic.
- File:line: `git.rs:546-551` missing-header path; `pipeline.rs:121-127`.
- Why no test catches it: All YAML cases use `HEAD~1..HEAD` pointing to real local commits. No test passes a valid-format but unreachable SHA.
- Evidence: `grep -rn "orphan\|unreachable\|not.*branch" tests/` returns nothing.

**G3**

- Severity: 🟡
- Scenario: Pure-binary diff (`.png` blob modified) — git emits `Binary files a/… and b/… differ` with no `---`/`+++`/`@@` lines. `parse_diff` (`git.rs:322-344`) never sets `old_path`/`new_path` → zero hunks → `pipeline.rs:81-83` returns `empty_output` silently. No warning is surfaced.
- File:line: `git.rs:322-344`; `pipeline.rs:81-83`.
- Why no test catches it: All YAML cases change text files. No case commits a real binary blob and asserts on the silent empty result.
- Evidence: `.png`/`.bin` appear in YAML only as string values inside source code, never as committed binary blobs.

**G4**

- Severity: 🟡
- Scenario: Rename with similarity below `git_rename_similarity_threshold` — `get_renamed_paths` (`git.rs:419`) skips adding `new_path` to `pure_new_paths` when sim < threshold. `get_changed_files -M` still returns the new name. That new path reaches `process_files_for_fragments` but doesn't exist at the prior rev; `CatFileBatch::get` silently returns "Path not found".
- File:line: `git.rs:419`; `pipeline.rs:99-110`.
- Why no test catches it: Rename cases use default similarity. No case exercises a below-threshold rename.
- Evidence: `grep -rn "similarity\|min_sim" tests/` returns nothing.

**G5**

- Severity: 🟡
- Scenario: Submodule change — `get_changed_files` (`git.rs:356`) lists the submodule directory path; `process_files_for_fragments` calls `std::fs::read_to_string` on a directory which errors silently via `filter_map`. Result: empty context, no error, no warning.
- File:line: `git.rs:356-366`; `pipeline.rs:121-127`.
- Why no test catches it: No test in `tests/cases/diff/` or `tests/framework/` initialises or modifies a git submodule.
- Evidence: `grep -rn "submodule\|gitmodules" tests/` returns nothing.

**G6**

- Severity: 🔵
- Scenario: `CatFileBatch` reader thread panic / I/O error — `wait_with_timeout` at `git.rs:124-131` chains `.ok().unwrap_or_default()`, discarding both `JoinError` and `io::Error`. A corrupt object store returns empty bytes; pybridge surfaces it as a successful empty result.
- File:line: `git.rs:124-131`; `pybridge.rs:287` only catches `anyhow::Error`, not thread panics.
- Why no test catches it: No test injects a mid-stream pipe failure or object-store corruption.
- Evidence: No test in the suite uses `catch_unwind`, mock processes, or adversarial pipe closure.

---

## Round 1 — Filesystem edge cases

**FS1**

- Severity: 🔴
- Scenario: `walk_recursive` (Rust non-git fallback) aborts the entire walk on the first unreadable subdirectory. The `?` on `walk_recursive(&path, result)?` propagates any `io::Error` up through `walkdir()`, which returns `Err`. `collect_candidate_files` then falls through to an empty `fallback` vec — every file discovered before the error is silently dropped.
- File:line: `diffctx/src/candidate_files.rs:81`
- Why no test catches it: All diffctx tests use real git repos so `git ls-files` always succeeds; the fallback branch is never entered. No test creates a `chmod 000` subdirectory inside a non-git directory and verifies non-zero results.
- Evidence: Lines 46–58: `fallback` is only populated via the `walkdir()` branch. Inner `walk_recursive(&path, result)?` propagates errors rather than skipping the offending directory.

**FS2**

- Severity: 🔴
- Scenario: A file whose first 8 KB is clean text but contains a NUL byte at offset >8192 is fully loaded into RAM before binary classification. `_detect_binary_in_sample` reads only 8 KB and returns `None`; then `file_path.open("rb").read()` allocates the entire file. The secondary NUL check at `raw_bytes[BINARY_DETECTION_SAMPLE_SIZE:]` runs only after that full allocation.
- File:line: `tree.py:206–213` (`_detect_binary_in_sample`), `tree.py:267–268` (unconditional full read)
- Why no test catches it: `test_unicode_content_and_encoding_errors` uses a 4-byte binary `b"\x00\x81\x9f\xff"` — the NUL is in the first 8 KB. No test constructs a file with a clean text prefix and a NUL in the tail.
- Evidence: `_decode_file_content` line 232: `if b"\x00" in raw_bytes[BINARY_DETECTION_SAMPLE_SIZE:]` — this branch is unreachable from any existing test.

**FS3**

- Severity: 🟡
- Scenario: A FIFO (named pipe) in the scanned directory causes `_read_file_content` to block indefinitely. `stat().st_size` returns 0, bypassing size limits. `is_symlink()` and `is_dir()` are both `False`, so `_create_node` calls `_read_file_content`. `file_path.open("rb")` on a FIFO with no writer blocks the process permanently.
- File:line: `tree.py:250–268`
- Why no test catches it: No test calls `os.mkfifo()`. `test_symlinks_and_special_files` only creates hidden files and a valid symlink. No `stat.S_ISFIFO`/`S_ISSOCK` guard exists before the `open()` call.
- Evidence: `tree.py` handles `PermissionError` and `OSError` but has no mode-check for special files.

**FS4**

- Severity: 🟡
- Scenario: Two hardlinks to the same file (same inode) in the scanned tree are both read and emitted as separate YAML nodes with identical content, doubling their token contribution with no deduplication.
- File:line: `tree.py:141–148` (`build_tree` / `iterdir`)
- Why no test catches it: No test calls `os.link()` to create a hardlink. There is no inode-tracking set anywhere in `tree.py`.
- Evidence: `build_tree` calls `sorted(dir_path.iterdir())` and passes every entry to `_process_entry`; no `st_ino` deduplication exists.

**FS5**

- Severity: 🟡
- Scenario: `[abc]` character-class patterns in `.gitignore`/ignore files are never tested end-to-end. `_process_ignore_line` rewrites patterns with path anchoring and passes the bracket expression verbatim to `pathspec`. A future change to the anchoring logic could silently break patterns like `[Mm]akefile` or `*.[ch]` with no test failure.
- File:line: `ignore.py:95–108` (`_process_ignore_line`)
- Why no test catches it: `test_ignore.py` covers `!`-negation, `**`, `/anchored` prefixes, and subdirectory scoping — but not bracket-class syntax.
- Evidence: `grep -rn "\[[a-zA-Z0-9]" tests/test_ignore.py` returns no test patterns using character classes.

**FS6**

- Severity: 🔵
- Scenario: `--output` path traversal outside project root. `_get_output_file_pattern` detects the out-of-root case and skips adding an ignore entry, but `_handle_output_file` calls `write_string_to_file` with the raw unchecked path. A user can run `treemapper . --output ../../etc/cron.d/evil` and overwrite files outside the repo.
- File:line: `ignore.py:52–61`, `treemapper.py:126`
- Why no test catches it: No test passes an `--output` path that resolves outside the root directory. The `is_relative_to` check in `ignore.py` only guards the ignore-spec entry, not the write.
- Evidence: `treemapper.py:126` — `write_string_to_file(output_content, args.output_file, ...)` with no containment assertion.

---

## Round 1 — Tree-sitter parser failure modes

**Languages declared** (37 grammars in `diffctx/Cargo.toml`): python, javascript/jsx, typescript/tsx, go, rust, java, c, cpp, c_sharp, ruby, php, scala, swift, html, bash, css, json, yaml, haskell, julia, ocaml, erlang, elixir, lua, r, zig, clojure, nix, groovy, objc, cmake, make, hcl, graphql, dart, prisma, svelte.

Entry point: `diffctx/src/parsers/tree_sitter_strategy.rs` — single `TreeSitterStrategy` handles every language via `LANG_CONFIGS`. Semantic edge extractors in `edges/semantic/*.rs` are separate from fragmentation.

**P1**

- Severity: 🔴
- Scenario: Very large file (10 MB / 100k LOC) passed to thread-local cached `Parser::parse`. No `set_timeout_micros` is called anywhere; parsing blocks a Rayon thread indefinitely.
- File:line: `parsers/tree_sitter_strategy.rs:1034-1054` (`parse_with_cached_parser`); `parsers/mod.rs:28-38` (`fragment_file`)
- Why no test catches it: `fragments_026_large_file_granularity_stress.yaml` uses ~150 LOC. No test creates a file of substantial size.
- Evidence: `grep -rn "set_timeout_micros|cancellation_flag" diffctx/src/` returns nothing.

**P2**

- Severity: 🔴
- Scenario: Kotlin (`.kt`) and F# (`.fs`/`.fsi`/`.fsx`) are parsed with the wrong grammar (Kotlin→`java`, F#→`c_sharp`). tree-sitter silently produces `ERROR` subtrees; `extract_definitions` walks them without any `has_error()` guard, emitting fragments with garbage boundaries.
- File:line: `tree_sitter_strategy.rs:711-832`
- Why no test catches it: One Kotlin YAML case exists (`kotlin_001_sealed_class_refactor.yaml`), testing cross-module hops, not fragmentation correctness. Zero F# cases exist.
- Evidence: `ls tests/cases/diff/ | grep -E "^kotlin"` = 1 file; no `.fs` case found.

**P3**

- Severity: 🟡
- Scenario: File with syntax errors — tree-sitter error-recovery injects `ERROR` subtrees. `extract_definitions` has no `node.is_error()` or `node.has_error()` check; definitions overlapping an `ERROR` node produce misleading fragment content.
- File:line: `tree_sitter_strategy.rs:1487-1516` (`extract_definitions`)
- Why no test catches it: `fragments_007` and `fragments_008` only assert the file appears in results, not that fragment boundaries are correct or error-node-free.
- Evidence: `grep -n "has_error|is_error" diffctx/src/parsers/tree_sitter_strategy.rs` returns nothing.

**P4**

- Severity: 🟡
- Scenario: File starting with a real UTF-8 BOM (`\xEF\xBB\xBF`). tree-sitter byte offsets are 3 bytes ahead of `content.split('\n')` line slices, misaligning fragment ranges and injecting the BOM prefix into extracted identifiers.
- File:line: `tree_sitter_strategy.rs:1578-1579` — no BOM strip before split
- Why no test catches it: `internals_050_utf8_bom_import.yaml` and `internals_051` contain no actual BOM bytes.
- Evidence: `grep -c "BOM" tests/cases/diff/internals_050_utf8_bom_import.yaml` = 0.

**P5**

- Severity: 🟡
- Scenario: Svelte (`.svelte`) mapped to `tree-sitter-svelte-ng` with `definition_types: &["script_element", "style_element", "element"]`. Embedded TS/JS inside `<script>` is opaque to the outer grammar; inner imports and symbols are invisible. No `edges/semantic/svelte.rs` exists.
- File:line: `tree_sitter_strategy.rs:648-651`; `edges/semantic/` has no `svelte.rs`
- Why no test catches it: Zero Svelte YAML test cases exist.
- Evidence: `ls tests/cases/diff/ | grep svelte` returns nothing.

**P6**

- Severity: 🔵
- Scenario: Single-line file with no trailing newline. `trim_blank_lines` returns `start > end` when that one line is blank, silently dropping the file from context with no error or warning.
- File:line: `parsers/mod.rs:70-86` (`trim_blank_lines`); `parsers/mod.rs:88-142` (`create_code_gap_fragments`)
- Why no test catches it: `fragments_021_single_line_env_change.yaml` targets a `.env` file (config parser path). No single-line source file exercises the tree-sitter path.
- Evidence: `grep "single_line|one.*line" tests/cases/diff/*.yaml | grep -v env` returns nothing relevant.

---

## Round 1 — Configuration / env-var matrix

Env vars read (all via `once_cell::Lazy`): `DIFFCTX_OBJECTIVE`, `DIFFCTX_NO_COMMIT_SIGNAL`, `TREEMAPPER_MAX_FRAGMENTS`, `DIFFCTX_OP_PPR_{ALPHA,FORWARD_BLEND}`, `DIFFCTX_OP_UTILITY_{ETA,STRUCTURAL_BONUS_WEIGHT,R_CAP_SIGMA,PROXIMITY_DECAY}`, `DIFFCTX_OP_SELECTION_{CORE_BUDGET_FRACTION,R_CAP_MIN,STOPPING_THRESHOLD}`, `DIFFCTX_OP_RESCUE_{BUDGET_FRACTION,MIN_SCORE_PERCENTILE}`, `DIFFCTX_OP_BOLTZMANN_{CALIBRATION_TOLERANCE,BISECT_ITERS}`, `DIFFCTX_OP_MODE_HYBRID_LARGE_CANDIDATE_THRESHOLD`, `DIFFCTX_OP_EGO_PER_HOP_DECAY`, `DIFFCTX_CATWEIGHT_*` (10 weights), `DIFFCTX_OP_FILTERING_*`.

**E1**

- Severity: 🔴
- Scenario: `DIFFCTX_OP_PPR_ALPHA=1.0` — `parse_fraction_or_default` clamps to `[0,1]` so `1.0` passes without fallback. `ppr.rs:62` computes `restart = 1.0 - alpha = 0.0`. Every push step multiplies residual by `restart`, so probability mass never returns to seed nodes — PPR degenerates to plain graph diffusion with no personalization. Pipeline returns a result silently; all semantic guarantees of PPR no longer hold.
- File:line: `diffctx/src/ppr.rs:62`, `diffctx/src/config/env_overrides.rs:15-16`
- Why no test catches it: `test_diffctx_invariants.py:116-121` parameterizes `DIFFCTX_OP_PPR_ALPHA` with only `-1.0` (fallback path). The boundary value `1.0` (accepted but catastrophic) is absent from all test files; no invariant asserts that PPR output is non-trivially personalized.
- Evidence: `tests/test_diffctx_invariants.py:116-121` — `1.0` absent from parametrize list.

**E2**

- Severity: 🔴
- Scenario: `TREEMAPPER_MAX_FRAGMENTS=0` — `limits.rs:17-20` parses via plain `.ok()` with no zero-guard; `0` is stored. `fragmentation.rs:253` truncates the fragment list to zero length. Pipeline returns empty selection with no diagnostic — user sees no output and no error.
- File:line: `diffctx/src/config/limits.rs:17-20`, `diffctx/src/fragmentation.rs:253`
- Why no test catches it: `TREEMAPPER_MAX_FRAGMENTS` is absent from every Python test file. No test sets it to `0` and asserts a non-empty result or explicit warning.
- Evidence: `grep -rn "TREEMAPPER_MAX_FRAGMENTS" tests/` returns no results.

**E3**

- Severity: 🟡
- Scenario: `DIFFCTX_OP_SELECTION_CORE_BUDGET_FRACTION=1.0` + `DIFFCTX_OP_RESCUE_BUDGET_FRACTION=1.0` simultaneously — both are independently clamped to `[0,1]`, both accepted. `select.rs:229` allocates `core_budget = budget_tokens * 1.0`, exhausting the budget. The rescue pass then requests another full budget against `remaining_budget=0`, selects nothing, and returns silently — rescue phase is completely voided with no warning.
- File:line: `diffctx/src/select.rs:229`, `diffctx/src/config/selection.rs:54,59`
- Why no test catches it: Only `CORE_BUDGET_FRACTION=42` (clamped to 1.0) is tested in isolation; the joint override with `RESCUE_BUDGET_FRACTION=1.0` is never exercised.
- Evidence: `tests/test_diffctx_invariants.py:106,119` — rescue fraction tested only via `-1.0` fallback path.

**E4**

- Severity: 🟡
- Scenario: `DIFFCTX_OBJECTIVE=boltzmann` with `budget_tokens=0` — `calibrate_beta` (`boltzmann.rs:98`) computes `target=0.0`, `tol=max(0*epsilon, 1.0)=1.0`. Neither bisect condition fires on iteration 0 (`0 > 1` false; `0 < -1` false), so the loop exits immediately returning a near-zero beta (very high temperature). Boltzmann selector then samples many fragments unconstrained by the zero-token budget.
- File:line: `diffctx/src/utility/boltzmann.rs:98-113`, `diffctx/src/pipeline.rs:241-256`
- Why no test catches it: No test combines `DIFFCTX_OBJECTIVE=boltzmann` with zero/near-zero budget; `test_diffctx_invariants.py:95` only checks determinism across objectives.
- Evidence: `grep -rn "boltzmann" tests/test_diffctx_invariants.py` — only the determinism test; no budget-0 variant.

**E5**

- Severity: 🟡
- Scenario: (a) Whitespace typo `DIFFCTX_OP_PPR_ALPHA="0.8 "` — Rust's `str::parse::<f64>()` rejects trailing spaces and silently falls back to the default; user believes the override took effect. (b) `DIFFCTX_OP_BOLTZMANN_BISECT_ITERS=0` — `0` is a valid `u32`; `calibrate_beta` runs its loop zero times and returns `(beta_lo*beta_hi).sqrt()` immediately — a near-zero beta that over-selects fragments.
- File:line: `diffctx/src/config/env_overrides.rs:9-12`, `diffctx/src/config/selection.rs:64`, `diffctx/src/utility/boltzmann.rs:101`
- Why no test catches it: `env_overrides` unit tests cover only `"hello"` and `""` as junk inputs — no whitespace variant. `bisect_iters=0` is not exercised anywhere in the suite.
- Evidence: `diffctx/src/config/env_overrides.rs:73-75` — test values are `"hello"` and `""` only.

**E6**

- Severity: 🔵
- Scenario: `DIFFCTX_OBJECTIVE` set to a typo (e.g. `"Submodularr"`) — `mode.rs:46` falls back silently to `Submodular` with no log output at any level. A user who intended `BoltzmannModular` gets the default objective with no indication the value was rejected.
- File:line: `diffctx/src/mode.rs:43-47`, `diffctx/src/pipeline.rs:139-141`
- Why no test catches it: `test_diffctx_invariants.py:95` tests only correctly-spelled objective names; no test passes a misspelled value and asserts on a warning or that Submodular was actually used.
- Evidence: `grep -rn "unrecognized\|unknown.*objective" diffctx/src/` returns nothing — no diagnostic is emitted on unknown value.

---

## Round 2 — Challenge findings

**M1** CONFIRMED — server.py:88-217 verified; both tools fully implemented and untested.
**M2** CONFIRMED — verified via `python3 -c "glob.glob('/tmp/../../etc/passwd')"` returns `['/tmp/../../etc/passwd']`. Python's glob does not escape the base; traversal is real.
**M3** CONFIRMED — server.py:36-41 has no lower-bound guard on budget_tokens.
**M4** CONFIRMED — server.py:104 builds target via `/` operator with no `is_relative_to(validated_path)` check.
**M5** DOWNGRADE 🟡→🔵 — `tree_to_string` likely raises a clean ValueError; missing test is a gap but not a defect.
**M6** CONFIRMED — protocol-level test gap.
**M7** CONFIRMED — control-char sanitization gap.
**F1** CONFIRMED — cli.py:42-43 rejection branch untested.
**F2** CONFIRMED — mutual-exclusion combination never tested.
**F3** CONFIRMED — warning text never asserted.
**F4** CONFIRMED — no malformed range tests via CLI.
**F5** CONFIRMED — dual `-c` + `-o` path untested.
**O1** CONFIRMED — writer.py:77 `rstrip("\n")` strips ALL trailing newlines. Round-trip preservation is implicit goal of YAML serialization for LLM consumption ("comprehension-per-token") — silent content corruption qualifies as 🔴.
**O2** CONFIRMED — surrogate write to UTF-8 file raises; JSON path uniquely affected.
**O3** CONFIRMED — bare `\r` not stripped before `\n` append.
**O4** CONFIRMED — no fragment dedup in any of three writers.
**O5** CONFIRMED — BiDi/control chars unescaped in MD headings.
**C1** CONFIRMED — Rayon `par_iter().collect()` order is not strategy-index order.
**C2** CONFIRMED — FxHashMap iteration before float sum is non-associative.
**C3** DOWNGRADE 🟡→🔵 — latent hazard requiring future refactor, not current bug.
**C4** CONFIRMED as 🔵 already.
**G1** CONFIRMED — empty repo path untested.
**G2** CONFIRMED — silent empty result on unreachable SHA.
**G3** CONFIRMED — pure-binary diff silent empty.
**G4** CONFIRMED — below-threshold rename silent.
**G5** CONFIRMED — submodule silent.
**G6** CONFIRMED as 🔵.
**FS1** CONFIRMED — `?` propagation aborts walk.
**FS2** CONFIRMED — secondary NUL check at tree.py:232 only after full read.
**FS3** CONFIRMED — no S_ISFIFO guard before open().
**FS4** CONFIRMED — no st_ino dedup.
**FS5** CONFIRMED — bracket-class never tested.
**FS6** CONFIRMED as 🔵 — user-controlled CLI tool; standard expectation is user owns their `--output` path (analogous to shell `>` redirect). 🔵 severity is correct.
**P1** CONFIRMED — no `set_timeout_micros` call exists in tree-sitter wrapper.
**P2** DOWNGRADE 🔴→🟡 — verified at tree_sitter_strategy.rs:710,790; explicit documented fallback ("close enough for fragmentation"). Deliberate design trade-off producing degraded but non-empty fragments, not a misroute. Test gap remains valid at 🟡.
**P3** CONFIRMED — no has_error/is_error check.
**P4** CONFIRMED — no BOM strip.
**P5** CONFIRMED — no svelte semantic edges.
**P6** CONFIRMED as 🔵.
**E1** CONFIRMED — verified at ppr.rs:62,90: `restart=0.0` makes `estimate[ui] += 0.0 * r_u` for every push, returning all-zero estimates (not "no personalization" — total degeneration). Severity 🔴 justified: user gets a silently meaningless result.
**E2** CONFIRMED — zero fragments returned silently.
**E3** CONFIRMED — joint clamp to 1.0 voids rescue silently.
**E4** CONFIRMED — boltzmann + budget=0 misbehavior.
**E5** CONFIRMED — whitespace-trim and bisect_iters=0 gaps.
**E6** CONFIRMED as 🔵.

---

## Round 2 — Severity calibration & ROI

Scoring rubric: Likelihood × Impact on a 1–5 scale each (product 1–25). Test/Fix cost in person-hours. Leverage = (L×I) / (Test + Fix).

**Specific scrutiny notes:**

- **FS3 (FIFO)**: real but rare. Users who run `treemapper .` on a project tree containing a FIFO are exotic (developers who placed `mkfifo` artefacts in their repo, or run on `/tmp`/`/var/run`). Likelihood low (1), impact high (4 — process hang). Net leverage modest.
- **M2 (MCP traversal)**: MCP server is invoked as a stdio subprocess by an LLM agent. The "caller" is the LLM, which is an *untrusted prompt-injection surface* (any document the LLM reads can craft tool args). Treat as adversarial. Likelihood high (4), impact critical (5 — exfiltrate `/etc/passwd`, host SSH keys).
- **FS2 (late-NUL)**: verified `_detect_binary_in_sample` returns `None` if first 8 KB has no NUL → `_decode_file_content` is reached only after `raw_bytes = file_path.open("rb").read()` (tree.py:267) — full file in RAM. Real, but a clean-text-then-NUL file is unusual outside attacker-crafted inputs. Likelihood low-medium.
- **C1 (Ensemble merge order)**: production default is `DiscoveryKind::Default` (mode.rs:25, pipeline.rs:374) — Ensemble is opt-in only. No invariants test enables Ensemble explicitly. Real bug but reachable only when user configures non-default. Downgrade likelihood.

| Finding | Likelihood × Impact | Test cost | Fix cost | Leverage | Final severity |
|---------|---------------------|-----------|----------|----------|----------------|
| M2      | 4 × 5 = 20          | 1h        | 30 min   | 13.3     | 🔴 confirmed   |
| M4      | 4 × 5 = 20          | 30 min    | 30 min   | 20.0     | 🔴 confirmed   |
| FS6     | 3 × 4 = 12          | 30 min    | 30 min   | 12.0     | 🟡 upgrade from 🔵 (MCP-adjacent) |
| E2      | 3 × 4 = 12          | 20 min    | 20 min   | 18.0     | 🔴 confirmed   |
| E1      | 2 × 5 = 10          | 30 min    | 20 min   | 12.0     | 🔴 confirmed   |
| O1      | 4 × 3 = 12          | 30 min    | 1h       | 8.0      | 🔴 confirmed   |
| O2      | 2 × 4 = 8           | 30 min    | 30 min   | 8.0      | 🔴 confirmed   |
| F1      | 3 × 2 = 6           | 15 min    | 0 (test only) | 24.0 | 🟡 confirmed   |
| M1      | 5 × 3 = 15          | 2h        | 0 (test only) | 7.5  | 🔴 confirmed   |
| G1      | 3 × 3 = 9           | 30 min    | 30 min   | 9.0      | 🔴 confirmed   |
| G2      | 3 × 3 = 9           | 30 min    | 30 min   | 9.0      | 🔴 confirmed   |
| FS1     | 2 × 4 = 8           | 30 min    | 15 min   | 10.7     | 🔴 confirmed   |
| P1      | 3 × 4 = 12          | 1h        | 30 min   | 8.0      | 🔴 confirmed   |
| FS2     | 2 × 4 = 8           | 30 min    | 20 min   | 9.6      | 🔴 confirmed   |
| O4      | 3 × 2 = 6           | 20 min    | 20 min   | 9.0      | 🟡 confirmed   |
| M3      | 3 × 2 = 6           | 15 min    | 15 min   | 12.0     | 🟡 confirmed   |
| E3      | 2 × 3 = 6           | 20 min    | 30 min   | 7.2      | 🟡 confirmed   |
| F2/F3   | 3 × 1 = 3           | 30 min    | 0        | 6.0      | 🟡 confirmed   |
| C1      | 1 × 3 = 3           | 1h        | 30 min   | 2.0      | 🔵 downgrade (Ensemble opt-in) |
| C2      | 1 × 2 = 2           | 1h        | 1h       | 1.0      | 🔵 downgrade   |
| FS3     | 1 × 4 = 4           | 30 min    | 15 min   | 5.3      | 🟡 confirmed (rare but unrecoverable hang) |
| FS4     | 2 × 1 = 2           | 30 min    | 30 min   | 2.0      | 🔵 downgrade   |
| P2      | 2 × 2 = 4           | 30 min    | 1h       | 2.7      | 🟡 confirmed   |
| P3      | 3 × 2 = 6           | 30 min    | 30 min   | 6.0      | 🟡 confirmed   |
| E5      | 2 × 2 = 4           | 20 min    | 15 min   | 6.9      | 🟡 confirmed   |

### Top 10 fix-this-week (by leverage)

1. **F1** (24.0) — add 1 line `--budget -2` rejection test; zero fix cost.
2. **M4** (20.0) — `get_tree_map` subdirectory traversal: add `is_relative_to` guard + 1 test.
3. **E2** (18.0) — `TREEMAPPER_MAX_FRAGMENTS=0`: clamp to ≥1 or reject; trivial.
4. **M2** (13.3) — `get_file_context` glob traversal: containment check on every match.
5. **M3** (12.0) — `budget_tokens<=0` validation in MCP.
6. **FS6** (12.0) — `--output` containment when path resolves outside root (MCP-reachable).
7. **E1** (12.0) — clamp `PPR_ALPHA` to `[0, 1)` (open at 1).
8. **FS1** (10.7) — `walk_recursive` swallow per-dir errors instead of `?`-propagating.
9. **G1/G2** (9.0 each) — empty-repo and unreachable-SHA explicit error path.
10. **FS2** (9.6) — stream-read with NUL scan instead of full slurp.

Defer: **C1, C2, FS4** (downgraded 🔵). **M1** (high impact but high test cost — schedule as a dedicated effort).

---

## Round 2 — Pattern synthesis

**Pattern 1**: Silent fallback on malformed/boundary input

- Manifestations: E1, E2, E3, E4, E5a, E5b, E6, M3, M5 (and partially F1, F4)
- Root cause: Env-var/argument parsers (`parse_fraction_or_default`, `read_env_*`, `Mode::from_str`, `budget_tokens` int coerce) accept boundary or malformed values and substitute defaults without emitting any log; `[0,1]` clamping treats `0.0`/`1.0` as valid even when they degenerate the algorithm (PPR α=1.0 → all-zero estimate, both budget fractions=1.0 → rescue voided, max_fragments=0, bisect_iters=0).
- Defensive primitive: A single `read_env_validated(name, parser, validator, fallback)` helper in `diffctx/src/config/env_overrides.rs` that (a) `tracing::warn!` on every fallback with the rejected raw value, (b) distinguishes open `(0,1)` from closed `[0,1]` intervals, (c) is mirrored by an MCP-side `validate_int_in_range(name, value, lo, hi)` for `budget_tokens` and `output_format`.
- Test leverage: ~9 individual env/boundary tests collapse into 2 — one parametric "every env var rejects junk and logs" sweep + one "degenerate-but-clamped boundary triggers warning" assertion.

**Pattern 2**: Untrusted path reaches filesystem without containment check

- Manifestations: M2, M4, FS6, F4
- Root cause: `validate_dir_path`/`_check_allowed` validate the *root* once but downstream operations (glob expansion, `validated_path / subdirectory`, `--output ../etc/x`, raw `diff_range` to git subprocess) construct new paths/args never re-checked against the allowed root before `open()`/`glob`/`write`/`git`.
- Defensive primitive: `assert_contained(child, root)` invoked at every filesystem boundary in `mcp/server.py` and `treemapper.py:_handle_output_file` — `child.resolve().is_relative_to(root.resolve())` raising `ToolError`/exit-1 otherwise; plus `re.fullmatch(r"[A-Za-z0-9_./~^@-]+", diff_range)` gate.
- Test leverage: 4 separate traversal tests collapse into 1 parametric "every (tool, path-arg) pair rejects `../`" sweep.

**Pattern 3**: Pipeline error swallowed to empty result

- Manifestations: G1, G2, G3, G4, G5, G6, FS1, FS2, FS3, P1, P6
- Root cause: `process_files_for_fragments` / `walk_recursive` / `CatFileBatch::get` use `filter_map(Result::ok)` or `unwrap_or_default()`; binary-only diffs, unreachable refs, FIFO blocks, oversize files, walkdir errors all yield successful empty returns rather than typed warnings.
- Defensive primitive: Replace `filter_map(Result::ok)` with `(Vec<T>, Vec<DiagnosedSkip>)` partition in `pipeline.rs::process_files_for_fragments` and surface skip count via `DiffContextResult.warnings`. Add `S_ISFIFO`/`S_ISSOCK` guard in `tree.py::_create_node` and `set_timeout_micros(1_000_000)` in `parse_with_cached_parser`.
- Test leverage: 11 disjoint failure scenarios collapse into 3 — one "all skip kinds yield warnings" matrix, one FIFO/special-file test, one parser-timeout test.

**Pattern 4**: Output/serialization assumes well-formed text

- Manifestations: O1, O2, O3, O4, O5, P4
- Root cause: Writers (`writer.py`, `tree_sitter_strategy.rs:1578`) treat content as LF-only, BMP-only, BOM-free, fragment-unique — no normalization stage between tree construction and serialization.
- Defensive primitive: `normalize_emit_chunk(text)` in `writer.py` strips `\r`, escapes lone surrogates, preserves trailing-newline count via explicit field; plus `dedupe_fragments` pass at top of every formatter; plus BOM-strip in Rust `extract_definitions` before byte-offset arithmetic.
- Test leverage: 6 format-specific tests collapse into 1 round-trip "arbitrary-bytes file → write → parse → equal" property test per format.

**Pattern 5**: Process-global mutable state under parallel execution

- Manifestations: C1, C2, C3, C4
- Root cause: Rayon `par_iter().collect()` and `FxHashMap` iteration interact with order-sensitive merge / float reductions; `GIT_TIMEOUT_SECS` is a process-global atomic shared across parallel test cases.
- Defensive primitive: `sort_by_key(strategy_index)` after every `par_iter().collect()` in `discovery.rs`/`pipeline.rs`; switch `tf` to `BTreeMap` *or* sort `(id, weight)` before L2-norm reduction in `lexical.rs:149`; convert `GIT_TIMEOUT_SECS` to thread-local with scope guard.
- Test leverage: 4 latent-race findings collapse into 1 "two runs of identical input produce byte-identical scores and selection" determinism test (plus multi-thread-count CI matrix entry).

**Pattern 6**: Untested CLI/MCP flag-combination matrix

- Manifestations: F1, F2, F3, F5, M1, M6, M7
- Root cause: Each flag is tested in isolation; pairwise/k-wise combinations and the entire MCP `get_tree_map`/`get_file_context` surface are missing from the test corpus.
- Defensive primitive: Pure test addition — `tests/test_cli_combinations.py` driven by a declarative `{flag: [valid, invalid]}` matrix, plus `tests/test_mcp_all_tools.py` round-tripping every registered FastMCP tool (including `initialize` handshake and control-char inputs).
- Test leverage: 7 missing scenarios collapse into 2 matrix-driven test files.

**Pattern 7**: tree-sitter parser robustness gaps

- Manifestations: P2, P3, P5
- Root cause: `extract_definitions` walks the AST without a `node.has_error()` guard; Kotlin/F# are deliberately misrouted to neighbour grammars producing degraded fragments; Svelte has no semantic-edge extractor — the degraded paths emit results indistinguishable from clean parses.
- Defensive primitive: Early `if node.has_error() { record_skip(file, "parse_error"); continue; }` in `extract_definitions`; emit a `DiagnosedSkip` on grammar-fallback paths so degraded output is observable; add a `LanguageCoverage` integration assertion that every `LANG_CONFIGS` entry has at least one positive YAML case.
- Test leverage: 3 latent gaps collapse into 1 coverage matrix test driven off `LANG_CONFIGS`.

---

## Round 2 — Missed issues

**R2-T1 (Special-token collapse breaks the budget)**

- Severity: 🔴
- Scenario: Literal `<|endoftext|>` in a fragment counts as **1 token** instead of ~7. `tokenizer.rs:7` uses `encode_with_special_tokens`, not `encode_ordinary`. LLM consumer re-tokenizing without `allowed_special` blows past budget.
- File:line: `diffctx/src/tokenizer.rs:6-8`; consumers `pipeline.rs:407`, `fragmentation.rs:331`, `memory_pipeline.rs:87,94`.
- Why no test / Evidence: `tests/test_tokens.py` exercises only the Python counter. `grep -n "encode_ordinary" diffctx/src/tokenizer.rs` → 0.

**R2-P1 (PyO3 — unwinding panic across FFI = UB)**

- Severity: 🔴
- Scenario: Any panic in `build_diff_context` (`o200k_base().unwrap()` first call, `IDENT_RE` compile, scoring `unwrap_or`) unwinds across the C ABI into CPython. `[profile.release]` lacks `panic = "abort"`; no `#[pyfunction]` uses `catch_unwind`. Result: UB / delayed segfault.
- File:line: `diffctx/src/pybridge.rs:276-287`; `diffctx/Cargo.toml:126`; `diffctx/src/tokenizer.rs:4`.
- Why no test / Evidence: Tests expect clean return or `PyRuntimeError`; none forces a panic. `grep -n "catch_unwind" diffctx/src/pybridge.rs` → 0.

**R2-S1 (Importance prior — `tests/` is NOT peripheral)**

- Severity: 🟡
- Scenario: `compute_file_importance` downweights `examples/`, `docs/`, `vendor/`, `fixtures/`, `__mocks__/`, `stories/` — omits `tests/`, `test/`, `__tests__/`, `spec/`, `e2e/`. Test files get `DEFAULT_IMPORTANCE = 1.0`. Diff in `src/auth.rs` ranks `tests/test_auth.rs` (lexically near-identical) at parity with the real implementation.
- File:line: `diffctx/src/config/importance.rs:39-73`; `utility/importance.rs:25-42`.
- Why no test / Evidence: Self-tests cover `tests/example_usage.py` *only via stem `example_`*; no test for ordinary `tests/test_user.py`. `grep -c '"tests"\|"__tests__"\|"spec"' diffctx/src/config/importance.rs` → 0.

**R2-I1 (Identifier extraction — ASCII-only regex)**

- Severity: 🟡
- Scenario: `IDENT_RE = [A-Za-z_]\w*` (`types.rs:267`) anchors on ASCII. Codebases with `обработчик`, `计算总额`, `λambda` yield zero identifiers. Seed expansion (`pipeline.rs:144`) empty → PPR has no semantic query → lexical similarity collapses.
- File:line: `diffctx/src/types.rs:267-275`; consumers `pipeline.rs:144,151`, `discovery.rs:71`, `fragmentation.rs:155,332`.
- Why no test / Evidence: All YAML cases use ASCII. `grep -rn "обработ\|计算\|λ" tests/cases/diff/` → 0.

**R2-PP1 (`is_reasonable_ident` — byte vs char length)**

- Severity: 🔵
- Scenario: `stopwords.rs:967` checks `ident.len() < min_len` — UTF-8 bytes, not chars. 2-char Cyrillic `ан` (4 bytes) passes `min_len=3`; ASCII-only stopword set means non-ASCII synonyms never filter.
- File:line: `diffctx/src/stopwords.rs:966-979`. Zero direct test coverage.

**R2-O1 (Postpass — silent skips, zero observability)**

- Severity: 🔵
- Scenario: `coherence_post_pass`, `rescue_nontrivial_context`, `ensure_changed_files_represented` silently skip fragments that don't fit / overlap / lack whole-file replacement (`postpass.rs:294`: `None => continue` even for a changed file). None emits `tracing::warn`. A budget-arithmetic regression dropping every changed file surfaces only as YAML failures with no leading diagnostic.
- File:line: `diffctx/src/postpass.rs`; `diffctx/src/select.rs`.
- Why no test / Evidence: No log-capturing assertion in suite. `grep -rn "tracing::warn\|tracing::error" diffctx/src/postpass.rs diffctx/src/select.rs` → 0.

---

## Round 3 — Prosecution

## Top 10 Critical Gaps

1. **M2 — Path traversal in `get_file_context` glob**
   - Failure mode: Claude Desktop reads a malicious README; an injected prompt issues `get_file_context(repo_path=".", glob="../../../home/user/.ssh/id_rsa")`. Verified: `glob.glob('/tmp/../../etc/passwd')` returns the path. The MCP server returns the exfiltrated content as tool output, which the LLM dutifully relays.
   - Consequence: **CVE-class data exfiltration**. SSH keys, AWS creds, browser cookies. Single user trust loss → reputational kill-shot for the project.
   - Structural argument: `_check_allowed` validates root once; every downstream path-construction (glob, `validated_path / subdir`, `--output`, `diff_range`) is unguarded. Same containment-bypass class repeats in M4, FS6, F4.
   - Cost of missing test: First public MCP user with prompt-injectable docs (every user) is one bad commit away from a CVE filing. No regression test exists, so a future "fix" easily reintroduces the bug.

2. **R2-P1 — Rust panic UB across PyO3**
   - Failure mode: `o200k_base().unwrap()` or any `unwrap_or` deep in scoring panics on rare input; unwinding crosses C ABI into CPython without `panic="abort"` or `catch_unwind`.
   - Consequence: **Undefined behavior** — segfault hours later, corrupted heap, mis-attributed crashes. Paper benchmarks on adversarial inputs become non-reproducible. Worst case: memory disclosure.
   - Structural argument: Whole FFI boundary lacks panic discipline. Every new `unwrap` in `diffctx/src/` is a latent UB. No build-time guard.
   - Cost of missing test: Reviewer running calibration sweep hits a panic, gets a segfault, files an "unreproducible flaky" issue. Paper figure 3 numbers depend on a process that may have UB-corrupted state.

3. **E1 — `PPR_ALPHA=1.0` produces all-zero rankings silently**
   - Failure mode: Calibration sweep includes `α ∈ {0.1,...,1.0}`. At α=1.0, `restart=0.0`, every push contributes `0.0 * r_u`, estimates are uniformly zero. Selection becomes effectively random. The pipeline returns a result with full token budget filled by lexically-similar noise.
   - Consequence: **Silent paper-result corruption**. The α=1.0 column in any ablation table is meaningless but plotted. Reviewer reproducing the paper sees a clean curve hiding a degenerate endpoint.
   - Structural argument: Pattern 1 — clamp accepts boundary values that algorithmically degenerate. Same shape as E2 (max_fragments=0), E3 (rescue voided), E5b (bisect_iters=0).
   - Cost of missing test: Future reviewer sweeps α, publishes "treemapper underperforms at α=1.0" — except it's a bug, not a finding. Paper credibility damaged.

4. **G2 — Unreachable revision returns silent empty**
   - Failure mode: User runs `treemapper . --diff abc1234..def5678` against a SHA only present on a remote-tracking branch they haven't fetched. `parse_diff` succeeds; `CatFileBatch::get` returns `Path not found` for every file; `filter_map` swallows. Output: empty YAML with no error. Exit code 0.
   - Consequence: **Silent wrong output**. User assumes "diff was empty / no relevant changes." Pastes empty context into LLM. LLM hallucinates a review.
   - Structural argument: Pattern 3 — pipeline-wide `filter_map(Result::ok)` swallows typed errors as empty success. Same in G1, G3, G4, G5, FS1, FS2.
   - Cost of missing test: User's PR review based on empty context misses a security regression. Trust loss is permanent.

5. **G1 — Empty (zero-commit) repo returns silent empty / unclear error**
   - Failure mode: `treemapper . --diff HEAD~1..HEAD` in fresh `git init` repo. Either silent empty or raw `PyRuntimeError` with `fatal: bad default revision 'HEAD'` — neither matches the documented contract.
   - Consequence: User trust loss on first-run UX; CI integrators wrap treemapper, assume exit-0 means clean diff, ship a broken context to downstream.
   - Structural argument: Same Pattern 3 root cause as G2; no typed error vocabulary across the git layer.
   - Cost of missing test: A PR refactoring `parse_diff` changes the error class; no regression catches it; downstream CI silently "succeeds."

6. **R2-T1 — `encode_with_special_tokens` breaks the budget guarantee**
   - Failure mode: Source code contains the literal string `<|endoftext|>` (legitimately, in tokenizer test fixtures, GPT-related code, blog posts). Treemapper counts it as **1 token**; downstream LLM with `allowed_special=set()` counts ~7. Budget overrun by 6× per occurrence.
   - Consequence: **Paper budget guarantee violated**. Any submodular bound proven against `count(text)` is wrong if the consumer uses `encode_ordinary`. Reviewer cannot reproduce "fits in 8k tokens" claim.
   - Structural argument: Tokenizer wrapper choice is asymmetric with consumer assumptions; no contract test. Single line fix but zero observability.
   - Cost of missing test: Paper Theorem on budget-feasibility has a counterexample buried in any repo containing GPT/tokenizer code.

7. **O1 — Trailing-newline truncation corrupts round-trip**
   - Failure mode: File ending in `"line\n\n\n"` (POSIX-mandated trailing newlines, often two). YAML output strips all trailing newlines via `rstrip("\n")`; reader reconstructs `"line\n"`. Silent content corruption.
   - Consequence: **Silent paper-result corruption** in benchmarks where treemapper output is fed back as ground truth. LLM trained on stripped output learns wrong file shape. Patches generated against stripped output fail to apply cleanly.
   - Structural argument: Pattern 4 — writers assume "well-formed text"; no normalization stage. Same root in O2 (surrogates), O3 (CRLF), O4 (dup), O5 (BiDi).
   - Cost of missing test: A future micro-optimization "consolidates" the rstrip call across formatters; no round-trip test to catch divergence.

8. **E2 — `TREEMAPPER_MAX_FRAGMENTS=0` returns empty silently**
   - Failure mode: Operator copies a config snippet with `MAX_FRAGMENTS=0` (typo, leftover debug, env-leak). Pipeline returns zero fragments; user sees empty YAML; no error.
   - Consequence: Production deployment ships empty contexts. LLM downstream answers "I don't see any code." Hours of debugging before someone checks env.
   - Structural argument: Pattern 1; same shape as E1, E3, E5.
   - Cost of missing test: Future PR adds `MAX_FRAGMENTS` to a Helm chart with default `0`; nothing catches it until production users complain.

9. **P1 — Tree-sitter has no parse timeout / cancellation**
   - Failure mode: Repo contains a 20MB minified vendored JS bundle or a pathological grammar input. `parse_with_cached_parser` blocks a Rayon thread; with multiple such files all worker threads hang; treemapper appears frozen to the user.
   - Consequence: **CI timeout, no diagnostic**. Calibration runs hang overnight; reviewer cannot reproduce paper benchmarks within wall-clock SLA.
   - Structural argument: No `set_timeout_micros` exists *anywhere* in the codebase. Class-of-bug: any future grammar with a known DoS input (regex-style catastrophic backtracking exists in tree-sitter grammars too) brings the tool down.
   - Cost of missing test: Single adversarial file in benchmark corpus breaks the entire calibration sweep with no actionable error.

10. **M1 — `get_tree_map` and `get_file_context` have zero tests**
    - Failure mode: Two of three MCP tools are entirely unverified. Refactor of `tree_to_string` signature, MCP schema regen, FastMCP version bump — all silently break runtime tool dispatch.
    - Consequence: **User trust loss on day one**. Claude Desktop user installs treemapper-mcp, calls `get_tree_map`, sees a JSON-RPC error. Uninstalls. Word spreads.
    - Structural argument: Pattern 6 — no flag/tool-combination matrix; each surface tested in isolation if at all.
    - Cost of missing test: Any refactor in `treemapper.py` that changes its function signature breaks MCP at runtime with no CI failure. The first user is also the first tester.

---

## Round 3 — Defense

Rebutting prosecution's Top 10 plus the targets in the brief.

### Verdicts

**1. M2 (`get_file_context` glob) — DEFER.** Threat model is
overstated. MCP runs as a *local* stdio subprocess; the LLM has
already been granted filesystem read in the same Claude Desktop
session via Read/Bash. "Prompt injection in a README" gives the
adversary nothing they can't already do via `cat ~/.ssh/id_rsa`.
"CVE-class" is dramatic — there's no privilege escalation, no
remote vector, no untrusted-network input. The bug is real
(per-glob containment missing) but its severity collapses into
the broader fact that an MCP server *is* a filesystem-access
tool. Fix it as part of the M4 containment primitive (same
`is_relative_to` helper), not as a 🔴 standalone fire.

**2. R2-P1 (PyO3 panic UB) — ACCEPT.** Cannot defend. Cross-FFI
unwinding is genuine UB; a single `unwrap` flips paper benchmarks
to "may segfault." `panic = "abort"` in Cargo.toml is one line.

**3. E1 (PPR α=1.0) — ACCEPT.** Cannot defend. Silent algorithm
degeneration during a calibration sweep is the worst class of bug
for a research-paper artefact.

**4. G2 (unreachable revision) — ACCEPT.** Cannot defend at 🔴.
Silent empty diff fed back to LLM is a comprehension-per-token
catastrophe (negative information, hallucinated review).

**5. G1 (empty repo) — DEFER.** Same root cause as G2; fix the
typed-error vocabulary once and both close. Severity 🟡 — first-
run UX hit, not silent-corruption.

**6. R2-T1 (`encode_with_special_tokens`) — ACCEPT.** Cannot
defend. Direct violation of the paper's budget contract.

**7. O1 (trailing-newline strip) — DEFER.** Real corruption, but
"POSIX-mandated double trailing newline" is rare in practice; most
files end in exactly one `\n`. Fix is cheap, but the consequence
("LLM trained on stripped output") is several inferential steps
removed. 🟡 not 🔴.

**8. E2 (`MAX_FRAGMENTS=0`) — ACCEPT.** Same Pattern-1 family as
E1; trivial clamp, silent prod failure mode. Indefensible.

**9. P1 (no parse timeout) — DEFER.** Hypothetical DoS; no
reported real-world hang. 30-min fix when convenient. Calibration
sweeps don't hit pathological grammars in current corpus.

**10. M1 (MCP tools untested) — DEFER.** Real test gap, but
prosecution's "first user is first tester / word spreads" is
hyperbole. Fix is pure-test (2h); schedule it, don't panic.

### Targets from the brief

**F1–F5 — DISMISS.** User-input rejections already error. Add a
one-line smoke test each; the message-format contract is not a
paper-grade invariant.

**P2 (Kotlin/F# fallback) — DISMISS.** R2-downgraded. A
`has_error()` guard everywhere converts "degraded-but-useful" into
"empty", which *lowers* comprehension-per-token. Keep the fallback.

**`--output` traversal (FS6) — DISMISS.** Analogous to shell `>`.
User owns their write path on a CLI tool. The MCP-reachable
variant collapses into M4.

**FS3 (FIFO) — DEFER.** Zero field reports. 15-min fix is cheap
but not urgent.

**R2-I1 (ASCII-only ident regex) — DEFER.** Benchmark corpus is
ASCII-dominant. Out-of-scope until the corpus expands.

**C1 (Ensemble merge order) — DISMISS as urgent.** Opt-in;
determinism test catches regressions cheaply.

**R2-O1 (postpass silent skips) — DISMISS.** Skipped-by-budget is
the expected steady state, not an error. An aggregate counter in
the result struct is sufficient if observability is wanted; full
`tracing::warn` per skip drowns signal.

### Prosecution's strongest 3 findings (cannot defend)

1. **E1** — PPR α=1.0 silently degenerates ranking; paper-result
   corruption.
2. **R2-T1** — `encode_with_special_tokens` violates the budget
   contract by ~7× per `<|endoftext|>` literal.
3. **R2-P1** — PyO3 panic = UB across FFI; segfaults invalidate
   any benchmark run.

Must-fix this week.

---

## Final Verdict

### TL;DR

После 4 раундов (8+4+2+1 агентов) на 70+ findings: **сильная корреляция вокруг 5 структурных паттернов**, не разрозненных багов. R2-калибровка отбросила ~5 false positives (M5, C3, C1, FS6 как user-responsibility), R3 Defense оспорил threat model для M2/F1-F5/P2/R2-O1. Сухая выжимка: paper-integrity и PyO3-boundary имеют **реальные дыры**, остальное — управляемый test debt.

### Top Issues (fix immediately) — verified ✅

1. ✅ **E1 — `DIFFCTX_OP_PPR_ALPHA=1.0` → all-zero rankings** (`diffctx/src/ppr.rs:62`)
   `restart = 1.0 - alpha` → `0.0`, далее `estimate[ui] += 0.0 * r_u`. Любой узел = 0. Калибровочный sweep, попавший на 1.0, выдаст бессмысленные числа в paper. **Fix**: clamp upper bound в `read_env_fraction` до `1.0 - epsilon` для α-параметров, или ранний guard в `ppr_push_csr`.

2. ✅ **R2-T1 — `encode_with_special_tokens` ломает budget guarantee** (`diffctx/src/tokenizer.rs:7`)
   Любой фрагмент содержащий литеральные `<|endoftext|>` / `<|im_start|>` (а такое есть в LLM-кодовых базах) считается как **1 токен** вместо 7. Бюджет `--budget 4096` фактически становится не строгим. **Fix**: `encode_ordinary(text)` вместо `encode_with_special_tokens(text)`.

3. ✅ **R2-P1 — Rust panic через PyO3 = UB** (`diffctx/src/pybridge.rs`, `diffctx/Cargo.toml`)
   Нет `catch_unwind`, в `[profile.release]` нет `panic = "abort"`. Любой panic в release-сборке (а `o200k_base().unwrap()` в `tokenizer.rs:4` его создаёт) даёт UB через C ABI. CVE-class. **Fix**: либо `panic = "abort"` в release, либо `catch_unwind` обёртки на каждом `#[pyfunction]`.

4. ✅ **E2 — `TREEMAPPER_MAX_FRAGMENTS=0` → пустой вывод без ошибки** (`diffctx/src/config/limits.rs:17-20`)
   `.unwrap_or(200)` принимает `0` как валидное значение → `max_fragments = 0` → все фрагменты truncate'ятся → пустой результат, никакого warning. **Fix**: `if v >= 1 { v } else { 200 }` после parse.

5. ✅ **M2 — MCP `get_file_context` glob path traversal** (`src/treemapper/mcp/server.py:150-153`)
   Verified: `glob.glob('/tmp/../../etc/passwd')` → `['/tmp/../../etc/passwd']` на macOS, без containment-проверки в server. Под prompt injection через любой LLM-агент → exfiltration. R3 Defense справедливо снизил severity ("LLM уже имеет FS read"), но containment всё равно правильный design — MCP-инструмент с явным `repo_path` не должен читать вне него. **Fix**: `assert p.resolve().is_relative_to(validated_path)` после `globmod.glob`.

### Systemic Patterns (R2.2 synthesis — фиксить как класс, не точечно)

1. **Silent fallback on malformed/boundary input** — 9 findings (E1, E2, E5, E6, F4, мелочи). Корень: `parse_*_or_default` молча падает в default. **Primitive**: `read_env_validated(name, default, range)` с `tracing::warn!` при fallback.

2. **Pipeline error → empty result** — 11 findings (G1, G2, G5, FS1, R2-O1). Корень: `filter_map(Result::ok)` глотает диагностику. **Primitive**: вернуть `(Vec<T>, Vec<DiagnosedSkip>)` и пробросить warnings в `DiffContextResult`.

3. **Untrusted path → FS без containment** — 4 findings (M2, M4, FS6, частично FS1). **Primitive**: `assert_contained(child, root)` на каждой FS-границе MCP/CLI слоя.

4. **Output assumes well-formed text** — 5 findings (O1-O4). **Primitive**: round-trip property test per format + `normalize_emit_chunk`.

5. **CLI/MCP flag-combination matrix untested** — 7 findings (F1-F5, M1, M3-M5). **Primitive**: parametric test matrix.

### False Positives from Round 1

- **M5** (output_format invalid in MCP) — R2 downgrade 🟡→🔵. ValueError путь чистый.
- **C3** (cache byte-budget dead in tests) — R2 downgrade 🟡→🔵. Это латентный риск, не текущий баг.
- **C1** (EnsembleDiscovery merge order) — R2 downgrade 🔴→🔵. Production default = `Default`, ensemble opt-in only.
- **P2** (Kotlin→Java grammar) — R2 downgrade 🔴→🟡. Документированный fallback с комментариями "close enough".
- **F1-F5** (CLI flag combos) — R3 Defense DISMISS. Это user-input rejection, не paper-grade contract.
- **FS6** (`--output` path traversal) — R3 Defense DISMISS. Аналог shell redirect, ответственность пользователя.

### Verdict

**Paper integrity под угрозой** через 3 верифицированных канала (E1 α=1.0, R2-T1 special-token budget escape, E2 MAX_FRAGMENTS=0) — если калибровочный sweep / configuration drift попадёт на любой из них, paper figures станут невоспроизводимыми. **Безопасность на MCP-границе** имеет реальный path traversal (M2), который дёшево закрывается одной containment-проверкой. **R2-P1 (PyO3 panic UB)** — самый серьёзный по классу, но самый дешёвый по фиксу (одна строка `panic = "abort"` в `Cargo.toml`).

Из ~70 findings реальных must-fix для submission paper — **5**. Остальные — управляемый debt; pattern-based primitives закроют ~30 за счёт 5 мелких рефакторов.

## Audit Log

- Date: 2026-04-28
- Project: /Users/nikolay/treemapper
- Skill: /review-tests
- Total findings: 8 R1 sections × ~5 findings = ~40 R1 + ~6 R2.3 missed = ~46 raw; after R2 calibration: 5 must-fix, ~30 deferrable, ~10 false positives
- Severity (final, post-R4): 🔴 5 (E1, R2-T1, R2-P1, E2, M2), 🟡 ~25, 🔵 ~15
- Agents used: 8 + 4 + 2 + 1 = 15
