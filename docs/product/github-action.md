# GitHub Action — diffctx LLM Diff Context

A composite action that turns a pull request diff into a token-budgeted
context file and hands the path plus the exact token count to the next step.
No secret, no agent loop, no model of its own: the review step downstream
chooses the model.

Marketplace name: **diffctx LLM Diff Context**
(`nikolay-e/diffctx@v1.15.0`). Runs on `ubuntu-latest`. Releases are tagged
`vMAJOR.MINOR.PATCH` only — there is no floating `@v1` tag, so pin an exact
release.

## Quick start

```yaml
jobs:
  llm-review:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@v7
        with:
          fetch-depth: 0

      - id: context
        uses: nikolay-e/diffctx@v1.15.0
        with:
          budget: '8000'

      - name: Review the selected context
        env:
          CONTEXT_FILE: ${{ steps.context.outputs.context-file }}
          TOKEN_COUNT: ${{ steps.context.outputs.token-count }}
        run: |
          echo "reviewing $TOKEN_COUNT tokens from $CONTEXT_FILE"
          # pipe "$CONTEXT_FILE" into any model you already pay for
```

`fetch-depth: 0` is required: diffctx resolves the range with git, so both
endpoints must exist in the local object store. The default shallow checkout
has neither the base commit nor `HEAD~1`.

## Inputs

Every input maps one-to-one onto a real `diffctx` flag; leaving one empty
leaves the CLI default in place.

| Input | Default | Flag | Meaning |
|---|---|---|---|
| `path` | `.` | positional | Directory to analyze, relative to the workspace |
| `diff-range` | auto | `--diff` | `main..HEAD`, `<base>..<head>`, … |
| `budget` | auto | `--budget` | Token cap; `-1` unlimited, `0` strict-zero floor |
| `scoring` | `ego` | `--scoring` | `ego`, `ppr`, `bm25`, `rrf` |
| `tau` | CLI default | `--tau` | Relevance threshold for full fragment content |
| `alpha` | CLI default | `--alpha` | PPR damping; only affects `scoring: ppr` |
| `full` | `false` | `--full` | Changed files only, every fragment, no related code |
| `format` | `md` | `--format` | `md`, `yaml`, `json`, `txt` |
| `output-path` | `$RUNNER_TEMP/diffctx-context.<ext>` | `--output-file` | Where to write |
| `timeout` | `300` | `--timeout` | Wall-clock deadline, in seconds |
| `fail-on-empty` | `false` | — | Fail the step on CLI exit code 4 |
| `diffctx-version` | current release | — | Exact PyPI version to install |
| `python-version` | `3.12` | — | Interpreter for the isolated install |

`diff-range` left empty resolves to the pull request's `base..head` on
`pull_request` events, and to `HEAD~1..HEAD` on everything else.

`output-path` defaults outside the workspace on purpose. Writing the context
into the repository would dirty the working tree that diffctx analyzes and
would leak into any later `git status` gate.

## Outputs

| Output | Meaning |
|---|---|
| `context-file` | Absolute path of the written context file |
| `token-count` | Exact `o200k_base` token count of that file |
| `byte-size` | Size of that file in bytes |
| `diff-range` | The range actually analyzed, after auto-resolution |
| `empty` | `true` when diffctx found no semantic context |

`empty` exists because a clean tree, a binary-only diff, or a fully filtered
diff makes the CLI exit `4` while still writing output. By default that is
reported, not fatal; set `fail-on-empty: true` to make it fail the step.

## Installation model

The action creates a throwaway virtualenv under `$RUNNER_TEMP` and installs
one pinned wheel, `diffctx==<diffctx-version>`, from PyPI. The published
wheels are `abi3` manylinux builds with the Rust extension compiled in, so no
toolchain and no build step are needed on the runner, and nothing touches the
repository's own Python environment.

## Notes

- Requires no token and no secret. Grant only `contents: read` unless a
  downstream step needs more.
- Exit codes other than `0` and `4` are propagated verbatim, so a missing
  revision (`3`) or a timeout (`124`) fails the step loudly.
