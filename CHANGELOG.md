# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **`--scoring rrf`** — reciprocal-rank fusion of the structural (`ego`) and
  lexical (`bm25`) signals: each ranks the same candidate universe, and a
  fragment scores `Σ 1/(k + rank_i)` with `k=60` (`DIFFCTX_RRF_K`). Fusion on
  ranks alone removes the score-scale calibration between the two signals
  that made the deployed mixture rank below its own lexical component on
  genuine retrieval, and the candidate set becomes their union rather than
  either one alone. EGO's structural guards (hub-noise and
  generic-config-only suppression) and the per-file cap are re-applied to
  the union against the fused scores, so the wider candidate set does not
  leak the noise EGO already rejects. `ego` remains the default (#125).
- **`--mode locate`** emits the ranked selection as compact
  `diffctx.locate.v1` JSON — path, line range, kind, symbol, relevance score
  and machine-readable provenance reasons (`changed`, `edge` with category /
  strongest source / mass, `proximity` seed-hops) with **no source bodies**:
  a navigation prior for agents at a fraction of pack-mode tokens. Available
  on the Python CLI, the native binary, and the MCP `get_diff_context` tool
  (`mode="locate"`); pack output is byte-unchanged (#126). Each item carries
  a coarse impact `group` (`test` / `type` / `config`) and the header a
  blast-radius `summary` (files, changed, context, tests) — with
  `--diff --mode locate` this answers "what does my uncommitted change
  touch, and which tests cover it" in one call (#135).
- `DIFFCTX_PROVENANCE_DUMP=<path>` writes one JSONL record per scored
  candidate — relevance, core/selected verdicts, seed-hop distance, and
  per-edge-category incoming mass — for offline analysis of why the selector
  included or skipped a fragment. Experimental telemetry, not a stable
  interface; the rendered output is byte-identical with the variable set
  (#93).
- The YAML test-case schema can express **deletions and renames**
  (`repo.deleted_files`, `repo.renamed_files`), so rename regression cases
  exercise git's real rename path instead of silently degrading to adds
  (#176).

### Fixed

- **`xfail:` in a YAML case was an unconditional silent skip.** The runner
  returned success before building the repo, so a marked case proved nothing and
  an XPASS was unobservable — the day its bug got fixed it still reported a pass.
  `known_below_threshold.txt` is enforced bidirectionally for exactly this
  reason; the two suppression mechanisms now agree. Marked cases run, and one
  that passes fails with instructions to drop the marker. Twelve of the 44
  markers turned out to be stale and were removed (ansible jinja templates,
  HTML `script src`, Rust `build.rs`, an abstract-class case, six C#/PHP one-hop
  cases, Haskell record syntax, a shell/terraform same-file case); 32 remain
  legitimately failing.
- **Excerpt downshift for mostly-unchanged cores** (#149, closing #105, #107 and
  the scoping half of #114). A core fragment is now rendered as its hunk-window
  excerpt whenever that window covers no more than half of it, instead of only
  when the budget forces the substitution. The old rule made granularity a
  function of leftover budget rather than of how much actually changed: the same
  402-line `CMakeLists.txt` shipped whole at `--budget 8000` and tightly
  excerpted at a small budget.

  Two changes make it work. Excerpts are now generated for kinds that *have* a
  signature variant as well — a signature is not a substitute for a
  mostly-unchanged function, because it drops the changed lines, which is the
  one thing the fragment was selected to carry. And `select_core_fragments` now
  reports which cores it satisfied rather than letting the caller infer it from
  id membership: a core represented by a substitute has a different id, so it
  looked "skipped" and the full fragment was handed straight back to the greedy,
  which re-emitted it alongside the excerpt.

  `render` also had to agree with `locate` that an `Excerpt` carries the
  `changed` role. Its id is a synthetic span cut out of the core and is not in
  `core_ids`, so without that the downshift would have stripped the change
  marker from the output — worse than the over-dump it replaces.

  Measured on the reported shapes: a 122-line bash script with one changed line
  goes from the whole file to a 7-line window; a 402-line `CMakeLists.txt` from
  the whole file to 7 lines; a 264-line JavaScript function with two changed
  lines from the whole function to an 8-line window plus the enclosing
  signature as context.
- **Diff-header paths escaping the repository root were accepted on Linux.**
  `Path::starts_with` compares components, not locations, and `canonicalize`
  fails for a path that does not exist — the normal case for the old side of a
  deletion. The guard fell back to the lexically joined path, and
  `<root>/../../escape.py` starts with `<root>` component-wise. It only failed
  on macOS, where the temp root canonicalizes through `/var → /private/var`, so
  the protection rested on an accident of filesystem layout; on Linux the
  accepted path reached `read_file_content`, whose `exists()` check the OS
  resolves through `..`, so content outside the repository could be read into
  the output. A `..` or absolute component is now rejected before any
  resolution. `--with-raw-diff`'s section filter was a second copy of the same
  guard with the same hole; both now share one resolver (#147).
- **`core.excludesFile` was written to a guessable name with `fs::write`**,
  which follows a symlink and truncates its target — so a pre-planted name in
  the shared temp directory could redirect the write. Now `O_CREAT | O_EXCL`
  with mode 0600 and a bounded retry (#147).
- **A rejected or unrecognized diff header charged its hunks to the previous
  file.** The hunk loop could not tell "not a path line" from "path refused",
  so the previous file's paths stayed live. Reset now happens on `diff --git`,
  the per-file boundary git always emits.
- **`TestFileDiscovery` matched the changed file's bare stem repo-wide**, so one
  changed `mod.rs` pulled in every other `mod.rs` in the tree. Bare-stem pairing
  is now scoped to the changed file's own directory, which is what makes such a
  pair meaningful; test prefixes and suffixes still match anywhere (#65).
- **Externally imported types became unanswerable information needs.** The
  external-symbol filter gated only the call-reference loop, so
  `from typing import Optional` plus `: Optional[int]` demanded a definition of
  `optional` from the repository — permanently unsatisfied, which inflates the
  diversity bonus for every candidate and lends match strength to any file
  mentioning the same stdlib type (#65).
- **The nontrivial-context rescue could spend its whole allowance on one file.**
  Its "file not yet represented" filter was a snapshot taken before the loop.
  The metric it serves is file-level, so a second fragment of an already-reached
  file buys nothing and costs the next file its chance.
- **Core seed selection depended on fragment order.** The tie-breaks compared
  kind class and span length only, so equally-ranked candidates were resolved by
  parser emission order — reversing it moved which code the output marks as
  changed. All three selectors now end on the fragment id.
- **Two panics reachable from repository content**: generated-fragment
  truncation checked the cap against the id's line span but sliced the actual
  text, and signature generation produced an id whose end preceded its start
  when a fragment's span was wider than its content. Both are now clamped.
- MCP `get_file_context` reported the raw glob match while checking containment
  on the resolved path, so a file accepted through a symlink raised an
  uncaught `ValueError` instead of returning its contents (#147).
- `build_locate` rejected an empty `diff_range` that `build_diff_context` treats
  as the working tree, so two entry points into one pipeline disagreed.
- Sibling grouping deduped with `Vec::contains` against the bucket, making it
  quadratic in exactly the shape it exists for — thousands of files in one
  directory (#116).

### Removed

- The dead `low_relevance_threshold` gate.
  `PipelineConfig.low_relevance_filter` was `false` for every scoring mode, so
  `filter_low_relevance` — and with it `FILTERING.low_relevance_threshold`
  (0.015) and its `size_penalty_*` scaling — could never execute;
  `filter_positive_relevance` was always the gate that ran. ContextBench result
  rows nevertheless stamped `low_relevance_threshold: 0.015` into their `config`
  block, so every recorded run attributed its selections to a threshold that
  never fired. Removed on both sides. Bit-equivalent: identical output across
  four scoring modes × three diff ranges, plus the full 2725-case corpus.
- Phantom knobs that only ever fed a discarded value:
  `GRAPH_FILTERING.git_rename_similarity_threshold` (its return value was
  dropped at the single call site) and `GitConfig`'s `poll_interval_ms` /
  `default_timeout_seconds` fields, which nothing read. Also a
  `_check_allowed(path.resolve())` in the MCP path validator that ran under a
  comment claiming it closed a symlink-swap race, while comparing the same
  already-resolved value against the same allowlist.

## [1.12.3] - 2026-07-27

### Added

- **Scoop install on Windows.** This repository is now itself the bucket:
  `scoop bucket add diffctx https://github.com/nikolay-e/diffctx` then
  `scoop install diffctx/diffctx`. The manifest moved from `packaging/scoop/`
  to `bucket/diffctx.json`, where `scoop bucket add` reads it, so CD pushing
  the regenerated manifest to `main` is the publication step. Previously the
  manifest was correct but unreachable — no bucket existed, so
  `scoop search diffctx` found nothing.

- `--with-raw-diff` bundles git's raw unified diff ahead of the selected
  context in every Python-CLI format (md/yaml/json/txt) and in the Python API
  (`build_diff_context(..., with_raw_diff=True)`). Additive only — selection is
  byte-identical with and without it — and not charged to `--budget`; the
  stderr token summary reports the real output size and breaks out the patch's
  share. Lock-file (#112), ignored and secret-like sections stay omitted. Not
  in the native binary or the MCP server yet (#150).
- Token counting is documented explicitly: every count and `--budget` use
  tiktoken `o200k_base` (GPT-4o family) and are approximate for other model
  families — `docs/product/token-budget.md`, plus a `Token counting` section in
  `--help` (#150).
- A GitHub Action (`action.yml`) runs diffctx as a CI step and exposes the
  context file, its exact token count and an `empty` flag as step outputs, so a
  downstream job can feed an LLM without re-tokenizing
  (`docs/product/github-action.md`, #145).
- The Claude Code plugin declares its MCP server self-bootstrapping:
  `uvx --from 'diffctx[mcp]' diffctx-mcp`. Installing the plugin no longer
  assumes `pip install 'diffctx[mcp]'` has already happened — `uv` fetches it
  on first use. (The entry point rather than the `diffctx mcp` subcommand: the
  subcommand only exists from this release on, so on any older version the
  bare command would try to map a directory named `mcp`.)
- `diffctx mcp` starts the MCP server, alongside the existing `diffctx-mcp`
  console script. MCP registries publish a *package* name, and clients that
  derive the executable from it run `diffctx` — which started a tree-mapping
  run and wrote 31 MB of the working directory into the protocol transport
  instead of speaking MCP. Both spellings now reach the same server.

### Security

- **A diff range could re-enable repository-configured diff commands.** The
  range validator allowed a leading `-`, so `--ext-diff` or `--textconv` passed
  as a "range" landed in argv *after* the `--no-ext-diff --no-textconv` that
  every diff invocation sets — undoing them and letting `.gitattributes` run
  external commands. Neither side of a range may now begin with a dash, and
  single revisions are additionally rejected for whitespace or control
  characters (a newline split one `cat-file --batch` request into two). Refs
  starting with a dash are unaddressable on a git command line, so nothing
  legitimate is lost.
- MCP tool calls had no wall-clock deadline — the CLI's watchdog covered only
  the CLI, leaving parse, fragmentation and scoring unbounded on the MCP path.
  All three tools now share the CLI's 300 s default.
- `SECURITY.md` documents prompt injection via repository content honestly: it
  is inherent to moving repository text into a model's context, diffctx does
  not detect or neutralize it, and output must be treated as untrusted.

### Fixed

- **A changed file could vanish from the diff entirely when `.gitignore`
  excluded its parent directory.** `check-ignore --no-index` — required for
  `.diffctx/ignore` to apply to tracked files — also revives git's rule that a
  file cannot be re-included once a parent directory is excluded. pandoc
  excludes every dotted root entry with `/*.*` and re-includes `!.github/**`;
  git keeps the tracked workflow file, diffctx dropped it, and a one-file
  commit rendered as an empty selection with exit 4. An exclusion inherited
  from an excluded ancestor no longer counts; a pattern matching the path
  itself still excludes it, so `.diffctx/ignore` and per-directory
  `.gitignore` rules are unaffected (#153).
- A user-level git config (`diff.noprefix`, `diff.external`, `color.ui=always`,
  custom src/dst prefixes) could empty the selection: diffctx now pins the
  diff invocation flags instead of inheriting the caller's configuration.
- `cd.yml` now dispatches `publish-extras.yml`, so npm and the Docker Hub
  mirror track every release instead of silently serving the previous one.

### Removed

- **AUR support.** `diffctx-bin` had never been submitted (the AUR RPC reported
  `resultcount: 0`), the publishing job gated on a credential this repository
  does not hold, and every release regenerated a PKGBUILD nobody could install.
  Arch users are served by `pipx install diffctx` and `cargo install diffctx`.

## [1.12.2] - 2026-07-26

### Fixed

- **The container images ran as root.** The published 1.12.1 image had no
  `USER` directive, so every `docker run` executed as uid 0 while the README
  promised an unprivileged user. The image now runs as uid 10001.
- **The MCP server advertised the SDK's version as its own.** `FastMCP` takes
  no version argument, so `initialize` reported the installed `mcp` package
  version (e.g. `1.28.1`) as the diffctx server version — a number that drifts
  with every SDK bump and never matched the shipped package. Clients now see
  the real version.
- **The native binary hard-capped every run at 4096 tokens.** `--budget`
  carried a fixed clap default instead of leaving the budget unset, so the
  auto-sizing the Python CLI has always used never ran on the binary shipped
  via crates.io, npm, the container images and the release archives — the
  same command returned a truncated selection there (14-file
  self-eat range: 15 fragments across 10 files, against 172 across 44). The
  binary now defaults to auto sizing, and `--budget -1` means unlimited as it
  does in Python.
- The token cache grew without bound (a machine that had analyzed a few dozen
  repositories reached 817k files / 4.0 GB). It is now capped at 512 MB,
  overridable with `DIFFCTX_TOKEN_CACHE_MAX_BYTES` (`0` disables eviction);
  each run trims one of the 256 shards back under its share of the cap,
  oldest entries first (#122).
- The native binary exited `0` on a diff that produced no semantic context
  (clean tree, binary-only, everything over the size cap), so callers on the
  binary channels could not tell an empty selection from a successful one. It
  now mirrors the Python CLI: the `no semantic context` warning plus its hint
  on stderr and exit code `4`.
- The native binary printed no token summary, although the README documents
  one for every run. It now writes `N tokens (o200k_base), SIZE` to stderr,
  silenced by the new `-q/--quiet`.
- `diffctx -v` was a usage error on the native binary while it printed the
  version on the Python CLI. Both short forms (`-v`, `-V`) now work.
- **A diff could lose its change signal entirely.** When the only fragment
  covering a hunk was a whole-file chunk — flat data files, languages without a
  tree-sitter parser, parse degradation — that chunk had no cheap variant to
  fall back on, so an oversized core was skipped and the output carried *zero*
  `role: "changed"` fragments while rendering the unchanged rest of the file as
  context. Such cores now fall back to an excerpt: the changed lines plus three
  lines of context, cut from the chunk itself and rendered as `kind: excerpt`,
  `role: "changed"`. On the elasticsearch `muted-tests.yml` repro the output
  goes from 121 fragments and no change signal to 118 fragments carrying the
  appended entry, 5,337 tokens instead of 5,970 (#103). The whole-file
  fragmentation of flat lists that the same repro shows is the separate
  granularity issue (#105) and is unchanged.
- A diff whose selection came back empty rendered as a bare `name`/`type` stub
  in every format: the writer gated *all* diff metadata on there being
  fragments, so the commit message and the list of changed files — the only
  actionable facts about such a run — were dropped. They are now always
  written.

### Changed

- **Lock files no longer render their hunks in diff mode.** A dependency bump
  is signal, the checksum churn carrying it is not: `diffctx . --diff` used to
  spend 12 KB of a 48 KB output on a `Cargo.lock` chunk. The touched lock files
  are now listed under `lockfile_changes:` (paths only, like `deleted_files:`),
  mirroring the tree-mode ignore policy while keeping the fact that they
  changed — on the reported range the output drops from 12,947 to 7,888 tokens
  (#112). Recognized: `Cargo.lock`, `package-lock.json`, `npm-shrinkwrap.json`,
  `yarn.lock`, `pnpm-lock.yaml`, `bun.lock(b)`, `deno.lock`, `Pipfile.lock`,
  `poetry.lock`, `uv.lock`, `pdm.lock`, `composer.lock`, `Gemfile.lock`,
  `flake.lock`, `go.sum`, `mix.lock`, `packages.lock.json`, `gradle.lockfile`,
  `Package.resolved`, `cabal.project.freeze`. `--full` still renders their
  content — it is the escape hatch that promises every fragment of the changed
  files.
- Native binary CLI parity: `-f` is accepted as the short form of `--format`,
  bare `--diff` means the working tree against `HEAD`, `--scoring` advertises
  its accepted values, and every option carries help text.
- The standalone binary is covered by its own integration suite
  (`crates/diffctx-native/tests/native_cli.rs`, run in CI): exit codes, both
  version flags,
  the token summary, `--quiet`, format validation and budget handling are now
  contract-tested against real git repositories. Every parity defect above
  shipped because nothing exercised the clap parser.

## [1.12.1] - 2026-07-23

### Added

- Standalone native binaries (linux x86_64/aarch64, macOS arm64, Windows
  x64) are built and attached to every GitHub release. The README had
  promised them since 1.9.x; releases only ever carried wheels and an sdist.
- The Rust engine is published to crates.io as the `diffctx` crate
  (`cargo install diffctx` for the native CLI, `cargo add diffctx` to embed the
  selection pipeline). Previously the name held only a reservation stub; the
  crate now carries the released engine, starting at 1.12.0.
- Container image `ghcr.io/nikolay-e/diffctx` (linux amd64/arm64), built from
  the release tag and smoke-tested against a real repository before the tag
  moves: `docker run --rm -v "$PWD:/repo" ghcr.io/nikolay-e/diffctx . --diff HEAD~1`.
  Mirrored to Docker Hub as `nikolajer/diffctx`.
- npm wrapper (`packaging/npm/`) that downloads the platform binary and
  verifies its SHA-256 against the published checksum. Scoop
  (`packaging/scoop/diffctx.json`) and AUR (`packaging/aur/`) manifests are
  generated from the same checksums but are not published to any bucket or
  to the AUR.

### Fixed

- The native binary silently emitted YAML for every unrecognized `--format`,
  including `md` — the documented default of the Python CLI. It now accepts
  only `yaml`/`json` and exits 2 on anything else.

## [1.12.0] - 2026-07-23

### Added

- `--timeout SECONDS` — wall-clock deadline for `--diff` analysis (default
  300); exceeding it exits `124` instead of hanging indefinitely (#70).
- `--no-ignores` — turns off every ignore rule (built-in patterns, project
  `.gitignore`, `.diffctx/ignore`). `--no-default-ignores` only disables the
  built-in list; its help now says so. Not supported with `--diff`.
- Output format is inferred from the `-o` extension when `-f` is omitted, so
  `-o out.json` no longer writes Markdown into a `.json` file; a mismatch
  between `-f` and the extension warns.

### Fixed

- **All error logging was dead.** An import-time `NullHandler` made
  `setup_logging` skip attaching a real handler, so `--log-level` was a no-op
  and all 19 `logger.error/warning/exception` sites were silent — `diffctx . -o
  /bad/path.md` exited 1 with no message at all.
- **diffctx invoked from inside a git hook silently analyzed the wrong
  repository.** Git exports repo-locating env vars (`GIT_DIR`,
  `GIT_INDEX_FILE`, `GIT_WORK_TREE`, ...) to hook subprocesses; inherited,
  they overrode `-C` on every internal git call. All git spawns now scrub
  these variables (`git_command()` in `git.rs`).
- YAML output preserved file content byte-exactly except for trailing
  newlines; the block chomping indicator is now chosen per content.
- Arrow-function fragments bound to variables were never stub-eligible (#106).
- Decorated definitions rendered as a bare `@decorator` line without the
  `class X:` / `def x():` header.
- `--max-depth`-pruned directories were labelled `_(empty directory)_` — a
  factual lie to the reader; they now read
  `_(children omitted: --max-depth reached)_` (`truncated: true` in
  YAML/JSON).
- Mixed directory + glob arguments dropped the glob files' parent path from
  node names.
- Double Ctrl-C printed a ~60-line traceback.
- Lock files `uv.lock`, `pdm.lock`, `bun.lock`, `bun.lockb`, `deno.lock` and
  `flake.lock` leaked into output; they now join the other lock files in the
  default ignore patterns.
- Large-repo hangs/OOM on trivial diffs (#70, #95): discovery no longer
  re-reads and re-tokenizes the whole candidate universe per ensemble
  strategy (one shared pass + a persistent per-blob token cache keyed by
  `(blob OID, tokenizer epoch)`), and edge construction is two-pass with a
  bounded per-source top-K instead of materializing up to tens of millions
  of raw edges before the cap; pass 2 replays a compact 16-byte-per-emission
  log instead of re-running the builders, so generation cost stays 1x.
  Verified: gitpod 8000s-hang -> 35.7s,
  pytorch 1848s-SIGKILL -> 7.4s, mui/material-ui OOM class recovered.
  Outputs are bit-identical (gated by `eval/analysis/equivalence_gate.py`).

### Known limitations

- Near-dense edge emission on huge same-directory trees (observed: 199M
  raw edges, 37GB peak on one mui/material-ui instance) remains expensive
  even with bounded construction; tracked in #116.

### Changed

- **The token budget is now a hard cap.** The changed-files post-pass no
  longer exceeds the budget to guarantee representation: a changed file
  whose cheapest representative does not fit stays unrepresented (visible
  as changed-file retention < 1). `--budget 0` therefore yields an empty
  selection (use `--full` for changed files only); CLI help updated.
- Latency telemetry: new `graph_build_ms` phase (graph construction was
  previously misattributed to `scoring_ms`, which now measures pure
  ranking) and `peak_rss_bytes` (in-process peak memory). Release builds
  carry line tables (`debug = "line-tables-only"`) for profiling at no
  runtime cost.
- CLI diagnostics are honest end to end: an exit-code table in `--help`
  (2 usage, 3 environment, 4 empty diff, 124 timeout), flag-value validation
  exits 2 instead of 1, git failures report a single line plus a
  `git log --oneline` hint on unknown revisions, conflicting flags warn
  (`-q`+`--log-level`, `--full`+selection flags, ...), a failed clipboard
  copy warns before falling back to stdout, and `--tau` / `--scoring` /
  `--alpha` / `--budget 0` help text describes what actually happens.
- `graph --summary` reports category shares as percentages, suppresses
  degenerate top-referenced lists, detects cycles over dominant-direction
  edges only, derives churn from `git log --since`, and disambiguates
  duplicate mermaid labels to relative paths.

## [1.11.0] - 2026-07-07

### Changed

- **The default `--format` is now `md` (Markdown), previously `yaml`.** Markdown
  is ~7% more token-efficient than YAML on real diffs and is preferred by
  reviewers for code-fragment-heavy output (evidence:
  `datasets/real-world-diff/v1/`). YAML remains available via `-f yaml`.
  This also changes the default stdout format of `treemapper` (a passthrough
  wrapper over this engine); downstream scripts that parse the default output
  must now pass `-f yaml` explicitly. (#104)

## [1.10.0] - 2026-06-20

### Changed

- The default `--tau` (stopping threshold) is now **0.12** across the CLI, MCP,
  and Python API — the calibrated grid optimum — bringing the Python side in
  line with the standalone Rust binary (was 0.08). Diff-context selections are
  slightly tighter by default.
- The MCP `get_tree_map` / `get_file_context` default `max_file_bytes` is now
  256 KB, matching the documented CLI default (was 100 KB).

### Fixed

- Document/citation edges can no longer create self-edges (a fragment linking to
  itself); `Graph::add_edge` now rejects `src == dst`.
- Test→source import edges no longer match `import` statements that appear inside
  comments or string literals (the import regex is now line-anchored).
- The MCP `get_file_context` clipboard confirmation now reports the number of
  files actually copied, excluding files skipped for exceeding the size cap.

### Docs

- Corrected the AST-parsing language count (was "12 languages", now "30+"),
  aligned the Rust crate version with the wheel, completed the MCP server README
  (all three tools documented), and fixed the documented `EGO.per_hop_decay`
  default (0.5). Documented the `scoring_mode` / `timeout` Python API params,
  clarified `--budget` / `--tau` help text, and marked the `DIFFCTX_OP_*` /
  `DIFFCTX_*` tuning env vars as an experimental, non-public interface.

## [1.9.2] - 2026-06-14

### Added

- Private-key and keystore files are now excluded from output in **both** tree
  mapping and `--diff` context, since such material is never legitimate LLM
  context: `*.pem`, `*.key`, `*.pfx`, `*.p12`, `*.keystore`, `*.jks`, and SSH
  private keys `id_rsa`/`id_dsa`/`id_ecdsa`/`id_ed25519` (public `.pub` keys stay
  visible). The `--diff` path previously applied no ignore filtering at all, so a
  changed key file would have leaked into context. Use `--no-default-ignores` to
  opt out of tree-mode default ignores. (`.env` files are intentionally still
  included — a changed `.env` is legitimate change context; redacting secret
  *values* is a separate planned content-scan feature.)

### Fixed

- **Diffs touching files larger than 100 KB no longer produce empty output.** A
  changed file (e.g. a 142 KB `Math.h`) was subject to the same 100 KB cap as
  context-discovery candidates, so the whole diff yielded "no semantic context".
  Changed files — the subject of the diff — are now parsed up to 5 MB.
- **C/C++ symbol names** for variable declarations now report the variable, not
  the type: a changed `const unsigned blane = …` is labeled `blane`, not
  `unsigned`. Covers `init`/`array`/pointer/function declarators and C++
  `reference`/`parenthesized` declarators (`int &r`, `int (*f)()`).
- Changed-file fragments are no longer dropped by the generated-file reduction
  (cap of 5 + 30-line content truncation), which could discard the small
  fragment covering the edited hunk and mislocate the change.
- Binary detection no longer misclassifies changed text files that embed ANSI
  escape / control bytes (snapshot and terminal-recording fixtures); it now uses
  git's NUL-byte heuristic, so such files are no longer silently dropped.
- Diff-context selection is now deterministic across processes: ego-graph score
  accumulation, context-fragment capping, and the greedy selection heap use
  stable tie-breaks, so identical inputs always yield byte-identical output.
- Python module-import edges are bidirectional (matching every other language
  and Python's own symbol references), so a changed imported module now
  propagates relevance back to its importers.

### Hardened

- `git cat-file` blob reads are bounded (drain-and-reject above 16 MB) instead of
  allocating an arbitrarily large buffer up front.

## [1.9.1] - 2026-06-14

> Supersedes 1.9.0, which was tagged but never published to PyPI (release-process
> hiccup); 1.9.1 ships the same changes. There is no 1.9.0 on PyPI.

### Added

- Diff-context output now leads with an orientation header — `commit_message`
  and `changed_files` — in every format (YAML/JSON/Markdown/text), so a reader
  sees *what* changed before reading any fragment.
- Each fragment carries a `role` of `changed` when it overlaps the diff hunks
  (omitted for supporting context). Changed code is emitted first; context
  follows, ordered by descending per-file relevance instead of alphabetically.

### Changed

- Line-contiguous fragments of the same role within a file are merged into a
  single entry, cutting the per-fragment scaffolding that dominated output made
  up of one-line snippets (lossless on line coverage).

### Fixed

- `get_changed_files` and `get_untracked_files` canonicalize paths consistently
  with deleted/discovered files, preventing duplicate fragments when a tracked
  path traverses a symlinked directory.
- Signature extraction no longer terminates at braces inside parameter defaults
  or annotations (e.g. Python `def f(x={}):`), which previously truncated the
  signature mid-parameter-list.
- The post-pass that rescues unrepresented changed files reuses the open
  `git cat-file --batch` reader instead of spawning a `git show` per file.
- Degraded token-count fallback returns 0 for the empty string (was 1).

## [1.8.0]

### Added

- Public `diffctx.run(argv=None, *, prog=None, version=None)` engine entry. It
  is the same execution path as the `diffctx` console script but accepts an
  injected program name and version string, so a downstream wrapper (e.g. the
  `treemapper` distribution) can present its own branding in `--help`,
  `--version`, and error prefixes without duplicating the CLI. `main()` now
  delegates to `run()` with the default `diffctx` identity — no behavior change
  for existing callers.
- `diffctx.mcp.__main__.main(prog=..., extra=...)` accepts an injected program
  name and install-extra string for the missing-dependency hint, so wrappers
  can re-expose the MCP entry point with their own name.

## [1.7.0] - 2026-05-22

### Added

- README documents the `graph` subcommand and the `--scoring {ppr,ego,bm25}`
  flag (default: `ego`); the absolutist "Uses Personalized PageRank" language
  has been replaced with a table covering all three scoring modes.
- `SECURITY.md` now ships a "Threat model" section that scopes diffctx as a
  local CLI (filesystem + git subprocess, no network) and documents that the
  optional `diffctx-mcp` server confines its filesystem reach via
  `DIFFCTX_ALLOWED_PATHS`.
- Footer of `README.md` links `CHANGELOG.md`, `SECURITY.md`, and
  `docs/engineering/parameter-strategy.md` so they are no longer orphaned.

### Changed

- **Package renamed from `treemapper` to `diffctx`.** PyPI distribution, CLI
  binary, MCP server binary, and Python import path all use `diffctx`. The
  `treemapper` PyPI package remains at 1.6.1 (frozen); install
  `diffctx` for all new development. The `TREEMAPPER_ALLOWED_PATHS`
  environment variable is now `DIFFCTX_ALLOWED_PATHS`.
- Single source of truth for the package version: the Python wheel reads its
  version from `Cargo.toml` (via maturin) instead of duplicating it in
  `pyproject.toml`, eliminating the Rust-crate-vs-Python-package version
  desync that previously shipped to PyPI.
- `--diff` now defaults to `HEAD` when no range is supplied, matching the
  most common invocation (`diffctx . --diff` → diff of the working tree
  against `HEAD`) and saving an argument in the 30-second demo path.
- `numpy` moved from a required runtime dependency into the `[tree-sitter]`
  extra, so default installs no longer pull a ~20 MB scientific stack the
  core tree-mapping mode does not use.
- CLI error messages are now actionable: instead of a raw Python traceback,
  invalid `--diff` ranges, missing git repositories, and unreadable paths
  print a one-line `Error: <what> — try: <next step>` and exit with code `2`
  for user-input errors (`1` is reserved for runtime failures).
- `automerge.yml` GitHub Actions workflow hardened: explicit minimal
  `permissions:` block, pinned action SHAs, and a guard that refuses to
  auto-merge anything touching `.github/`, `pyproject.toml`, or `Cargo.toml`.

### Fixed

- Replaced every user-reachable `unwrap()`/`expect()` in the Rust core
  (`tokenizer.rs`, `git.rs`, `scoring.rs`, `pybridge.rs`) with proper
  `PyRuntimeError` / `GitError` propagation. A malformed diff, a missing
  BPE table, or an oversized hunk-header integer no longer aborts the
  Python interpreter via `panic = "abort"`.
- `diffctx-mcp` entry point now guards against being launched without the
  `[mcp]` extra installed and prints an install hint instead of an
  `ImportError` traceback.

### Removed

- Dropped Kotlin and F# from the language matrix: tree-sitter grammars for
  both were silently misaligned with the project's import-resolution rules
  and produced misleading edge weights. They will return once the grammars
  are vetted.

### Security

- MCP server (`diffctx-mcp`) now refuses to traverse outside the
  directories listed in `DIFFCTX_ALLOWED_PATHS` (OS-pathsep-separated)
  and refuses to start if the envvar is unset when run as a network-facing
  process. See [`SECURITY.md`](SECURITY.md) for the threat model.

## [1.6.1 and earlier]

Earlier releases shipped as `treemapper`; see
<https://pypi.org/project/treemapper/#history> for legacy versions and
<https://github.com/nikolay-e/diffctx/releases> for the corresponding GitHub
release notes (`1.0.0` through `1.6.1`).

[Unreleased]: https://github.com/nikolay-e/diffctx/compare/v1.12.3...HEAD
[1.12.3]: https://github.com/nikolay-e/diffctx/compare/v1.12.2...v1.12.3
[1.12.2]: https://github.com/nikolay-e/diffctx/compare/v1.12.1...v1.12.2
[1.7.0]: https://github.com/nikolay-e/diffctx/compare/v1.6.1...v1.7.0
