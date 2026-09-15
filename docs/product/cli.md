# Command-line reference

Every flag `diffctx` accepts, with its default and one line of meaning. This
page is rendered from the parsers themselves by `scripts/update_cli_reference.py`
(`tests/test_cli_reference.py` fails when it differs from `diffctx --help`), so
what you read here is what the installed version answers. Worked examples
live in the [README](../../README.md#usage); what `--budget` counts is in
[Token counting](token-budget.md).

The native binary (`cargo install diffctx`, `npx diffctx`, the Docker image)
takes the diff-mode subset of these flags and writes YAML or JSON; `--help`
there lists exactly which.

## `diffctx`

```text
usage: diffctx [-h] [-o FILE] [-i FILE] [-w FILE] [--no-default-ignores] [-c]
               [-q] [--log-level {error,warning,info,debug}] [-v]
               [-f {yaml,json,txt,md}] [--save] [--no-ignores] [--max-depth N]
               [--no-content] [--max-file-bytes N] [--no-file-size-limit]
               [--diff [RANGE]] [--budget TOKENS] [--alpha FLOAT]
               [--tau FLOAT] [--scoring {ppr,ego,bm25,rrf,pit}]
               [--mode {pack,locate}] [--timeout SECONDS] [--full]
               [--with-raw-diff]
               [paths ...]

Generate a structured representation of a directory tree (Markdown, YAML, JSON, or text). Supports diff context mode (--diff) for intelligent code change analysis.

Subcommands:
  graph    Build and analyze the project dependency graph
  mcp      Run the MCP server over stdio (same as diffctx-mcp; needs the [mcp] extra)

positional arguments:
  paths                 Directories, files, or glob patterns to analyze

options:
  -h, --help            show this help message and exit
  -o FILE, --output-file FILE
                        Write output to FILE instead of stdout ('-' forces
                        stdout)
  -i FILE, --ignore FILE
                        Custom ignore file (bare names also resolve inside
                        .diffctx/; not yet supported with --diff)
  -w FILE, --whitelist FILE
                        Whitelist file, only matching files are included (bare
                        names also resolve inside .diffctx/; not yet supported
                        with --diff)
  --no-default-ignores  Tree mode only: disable built-in ignore patterns;
                        project .gitignore and .diffctx/ignore still apply
                        (see --no-ignores)
  -c, --copy            Copy to clipboard instead of printing to stdout
                        (combine with -o to also write a file)
  -q, --quiet           Suppress status messages (token summary, save/copy
                        confirmations); overrides --log-level
  --log-level {error,warning,info,debug}
                        Log level (default: error)
  -v, --version         show program's version number and exit
  -f {yaml,json,txt,md}, --format {yaml,json,txt,md}
                        Output format (default: md; inferred from the -o FILE
                        extension when omitted)
  --save                Save output to tree.{ext} in the current directory
                        (tree.md by default; extension follows -f)
  --no-ignores          Disable all ignore rules: built-in patterns, project
                        .gitignore, and .diffctx/ignore (a custom -i file
                        still applies)
  --max-depth N         Maximum traversal depth (default: unlimited)
  --no-content          Skip file contents (structure only)
  --max-file-bytes N    Omit content of files larger than N bytes (default:
                        256 KB). Use --no-file-size-limit to include all.
  --no-file-size-limit  Include all files regardless of size

diff context mode:
  --diff [RANGE]        Git diff range (e.g., HEAD~1..HEAD, main..feature) or
                        a duration window ending now (24h, 8d, 90min, 1h30m,
                        2w — units s/m/h/d/w), which covers the commits inside
                        the window plus the uncommitted work on top. Bare
                        --diff shows uncommitted changes (working tree vs
                        HEAD).
  --budget TOKENS       Token budget in o200k_base tokens (tiktoken, GPT-4o
                        family — other model families tokenize differently, so
                        leave headroom; see 'Token counting' below): omit =
                        auto (default), N = cap on the whole artifact (change
                        summary charged first), -1 = unlimited, 0 = strict-
                        zero floor (empty selection; use --full for changed
                        files only)
  --alpha FLOAT         PPR continuation probability, 0-1 exclusive (default:
                        0.60; higher = mass travels further from the change,
                        lower = tighter around it). Only affects --scoring ppr
  --tau FLOAT           Relevance threshold for full fragment content, >= 0
                        (default: 0.05). Fragments scoring below it are
                        reduced to signature stubs or dropped; higher = leaner
                        output, lower = more surrounding context
  --scoring {ppr,ego,bm25,rrf,pit}
                        Scoring mode: ego = structural neighbors of the change
                        (default); ppr = graph-wide relevance (Personalized
                        PageRank), for far-reaching changes; bm25 = lexical
                        similarity, for sparse cross-file structure; rrf =
                        rank fusion of ego and bm25 on ranks; pit = the same
                        fusion on score percentiles rather than ranks
  --mode {pack,locate}  Output mode: pack = context with source bodies
                        (default); locate = ranked navigation list with
                        provenance reasons, JSON only (diffctx.locate.v1; -f
                        is ignored)
  --timeout SECONDS     Wall-clock deadline for --diff analysis (default:
                        300); on expiry diffctx aborts with exit code 124
                        instead of hanging
  --full                Include every fragment of the changed files and
                        nothing else — no related-code context (ignores
                        --budget/--tau/--alpha/--scoring)
  --with-raw-diff       Also embed the raw unified diff (git's own +/- text)
                        ahead of the selected fragments. Additive only:
                        selection is unchanged, and the diff does NOT count
                        against --budget (the stderr token summary counts it,
                        reporting the real output size). Lock-file, ignored,
                        and secret-like sections stay omitted. Python CLI only
                        — the native binary has no such flag

Built-in ignored patterns (disable with --no-default-ignores; project .gitignore
and .diffctx/ignore always apply unless --no-ignores is given):
  .git/, .svn/, .hg/    Version control directories
  __pycache__/, *.py[cod], *.so, venv/, .venv/, .tox/, .nox/  Python
  node_modules/, .npm/  JavaScript/Node
  package-lock.json, yarn.lock, pnpm-lock.yaml  JS lock files
  Pipfile.lock, poetry.lock, Cargo.lock, Gemfile.lock  Other lock files
  target/, .gradle/     Java/Maven/Gradle
  bin/, obj/            .NET
  vendor/               Go/PHP
  dist/, build/, out/   Generic build output
  .*_cache/             All cache dirs (.pytest_cache, .mypy_cache, etc.)
  .idea/, .vscode/      IDE configurations
  .DS_Store, Thumbs.db  OS-specific files
  tree.{yaml,json,md,txt}  Default output files (auto-ignored)

Ignore files (hierarchical, like git):
  .gitignore            Standard git ignore patterns
  .diffctx/ignore       diffctx-specific patterns

Whitelist files (auto-discovered):
  .diffctx/whitelist    Include-only filter

Examples:
  diffctx .                    Map current directory to Markdown
  diffctx /path/to/project     Map a specific directory
  diffctx . -f json            Output as JSON
  diffctx . --save             Save as tree.md
  diffctx . --diff             Context for uncommitted changes
  diffctx . --diff HEAD~1      Context for the last commit
  diffctx . --diff 24h         Context for everything changed in the last 24 hours
  diffctx . --diff 8d          Same, over the last 8 days (also 90s, 10min, 1h30m, 2w)
  diffctx . --diff HEAD~1 --with-raw-diff   Raw patch + selected context in one file
  diffctx . -c                 Copy output to clipboard
  diffctx . --no-content       Structure only, no file contents
  diffctx graph .              Build the project dependency graph (see: diffctx graph --help)
  diffctx graph . --summary    Print graph stats (cycles, hotspots, coupling)

Output routing:
  Default:      stdout
  -o FILE:      write to FILE (format inferred from extension unless -f is given)
  -o -:         force stdout
  --save:       write to tree.{ext} (tree.md by default; extension follows -f)
  -c:           copy to clipboard, suppress stdout
  -c -o FILE:   copy to clipboard AND write to FILE

Token counting (--budget, and the summary line on stderr):
  Every count comes from tiktoken's o200k_base encoder (the GPT-4o/GPT-4.1
  family). It is exact for those models only. Claude, Gemini, Llama and other
  families use different tokenizers, so their counts differ from the number
  printed here — usually by single-digit to low-double-digit percent, in either
  direction. Treat --budget as an upper bound in o200k tokens and leave
  headroom (e.g. --budget 28000 for a 32k target) when the consumer is not an
  OpenAI model. The budget covers the whole artifact, not only the fragments:
  the change summary (commit message, changed/deleted/renamed/lockfile/ignored
  lists) is charged against it first and the selection spends the remainder.
  A budget smaller than that summary therefore yields the summary alone —
  a changed path is never dropped to fit, because a reader who cannot see what
  changed is worse off than one who is over budget. There is no --tokenizer
  flag; o200k_base is pinned so results stay reproducible against the
  published evaluation.
  --with-raw-diff output is NOT charged to --budget, but IS included in the
  stderr token summary, which always reports the real size of what was written.

Exit codes:
  0  success
  1  runtime error (unreadable path, write failure)
  2  usage error (unknown flag or invalid value)
  3  environment error (git missing, not a repository, unknown revision)
  4  --diff produced no context (clean tree or empty range)
  124  --diff exceeded the --timeout wall-clock deadline
```

## `diffctx graph`

```text
usage: diffctx graph [-h] [-o FILE] [-i FILE] [-w FILE] [--no-default-ignores]
                     [-c] [-q] [--log-level {error,warning,info,debug}]
                     [-f {mermaid,json,graphml}] [--summary]
                     [--level {fragment,file,directory}]
                     [directory]

Build and analyze the project dependency graph

positional arguments:
  directory             The directory to analyze

options:
  -h, --help            show this help message and exit
  -o FILE, --output-file FILE
                        Write output to FILE instead of stdout ('-' forces
                        stdout)
  -i FILE, --ignore FILE
                        Custom ignore file (bare names also resolve inside
                        .diffctx/; not yet supported with --diff)
  -w FILE, --whitelist FILE
                        Whitelist file, only matching files are included (bare
                        names also resolve inside .diffctx/; not yet supported
                        with --diff)
  --no-default-ignores  Tree mode only: disable built-in ignore patterns;
                        project .gitignore and .diffctx/ignore still apply
                        (see --no-ignores)
  -c, --copy            Copy to clipboard instead of printing to stdout
                        (combine with -o to also write a file)
  -q, --quiet           Suppress status messages (token summary, save/copy
                        confirmations); overrides --log-level
  --log-level {error,warning,info,debug}
                        Log level (default: error)
  -f {mermaid,json,graphml}, --format {mermaid,json,graphml}
                        Graph output format (default: mermaid)
  --summary             Print graph statistics instead of the graph (cycles,
                        hotspots, coupling); -f is ignored
  --level {fragment,file,directory}
                        Node granularity: directory, file, or fragment =
                        function/class-level block (default: directory);
                        applies to mermaid output and --summary
```

## `diffctx mcp`

Runs the MCP server over stdio — the same entry point as `diffctx-mcp` —
and takes no flags. It needs the `mcp` extra (`pip install 'diffctx[mcp]'`);
the tool it exposes, its arguments and its read-only guarantees are
described in the [security policy](../../SECURITY.md) and the README's
MCP section.
