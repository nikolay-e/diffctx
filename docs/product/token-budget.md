# Token counting and the raw diff bundle

Reference for two questions the CLI answers only in passing: *whose* tokens
`--budget` counts, and what `--with-raw-diff` costs.

## Which tokenizer diffctx uses

Every token number diffctx prints or enforces comes from **tiktoken's
`o200k_base` encoder** (the GPT-4o / GPT-4.1 tokenizer); the stderr summary
names it. Counts are exact only for OpenAI models on that encoder — Claude,
Gemini, Llama and friends tokenize differently, typically within
single-to-low-double-digit percent either way.

Practical implication: for a non-OpenAI consumer treat `--budget N` as *an
upper bound of N o200k tokens* and leave headroom — e.g. `--budget 28000` when
aiming at a 32k window. If a hard guarantee matters, measure the produced file
with your own model's tokenizer.

There is no `--tokenizer` flag: `o200k_base` is pinned (locked by
`test_tiktoken_o200k_base_encoding_is_pinned`) because every number in the
paper's evaluation is denominated in it.

## What `--budget` does when you omit it

Omitting `--budget` does not mean "no limit" — it sizes one from the change:

```
auto = clamp(3.0 * sum(min(core_tokens, 1500) for each changed fragment), 8_000, 48_000)
```

Each changed fragment contributes at most 1 500 tokens, so one 2 000-line
unparsed template touched by a single hunk cannot drag in the whole repo as
"context". The result is clamped to `[8 000, 48 000]`.

**On large real-world diffs it saturates at the 48 000 ceiling**, and output
tracks whatever budget it is given almost exactly — measured on one
react-native commit: `--budget 8000` produced 8 823 markdown tokens, `16000`
produced 16 889, `48000` produced 48 117, and auto produced 48 396. So on a wide
change the default spends 48k, and the way to spend less is to say so.

Whether 48k is the right ceiling is open (#167). The measured trade on
react-native, all 35 cases scored twice: dropping to `--budget 8000` costs
0.145 recall and gains 0.118 precision for a fifth of the tokens, with the
forbidden-file rate falling by nearly two thirds. That is a real trade rather
than a free win — 20 of 35 cases lose some recall — so the constant has not been
changed on that evidence alone.

`--budget -1` disables the limit entirely. Do not reach for it on a wide range:
selection and the post-passes dominate wall clock there, and with nothing to
stop the greedy the run tends to hit the deadline instead of finishing.

## `--with-raw-diff`

`diffctx . --diff HEAD~1 --with-raw-diff` writes git's own unified diff into
the output ahead of the selected fragments, so a reader gets the literal `+`/`-`
edit alongside the surrounding code that explains it. One command replaces the
hand-assembled `git diff` + `diffctx` bundle.

- **Additive only.** Fragment selection for a given input is byte-identical
  with and without the flag; the patch is attached after selection has run.
- **Not charged to `--budget`.** The budget governs *selection* — the code
  diffctx chose to show you. The raw diff is verbatim input you already have,
  and charging it would silently shrink the selected context that is the
  actual product. So a `--budget 8000` run with the flag emits ~8000 tokens of
  fragments **plus** the patch.
- **The stderr summary still tells the truth.** It counts the whole rendered
  output, and adds a second line breaking out the patch's share:

  ```text
  9,120 tokens (o200k_base), 34.1 KB
    of which 1,708 tokens are the raw diff (not charged to --budget)
  ```

- **Same disclosure policy as the rest of diff mode.** Sections for lock files
  (#112 — paths are reported, checksum churn is not), for paths excluded by
  `.gitignore` / `.diffctx/ignore`, and for secret-like paths (`*.pem`,
  `*.key`, `id_rsa`, …) are dropped from the bundled patch. A section whose
  path cannot be resolved inside the repository is dropped too.
- **Untracked files do not appear** in it. `git diff` does not show them;
  diffctx still fragments them into the selected context as usual.
- **Python surfaces only.** The `pip`/`pipx` CLI, the Python API
  (`build_diff_context(..., with_raw_diff=True)`) and the MCP server
  (`diffctx_context(mode="pack", include_raw_diff=true)`) support it. The standalone
  Rust binary (`cargo install diffctx`, `npx`, the Docker image) has no such
  flag in this release.

Rendering per format: a fenced ` ```diff ` block under a `## Raw diff` heading
in Markdown, a `raw_diff` block scalar in YAML, a `raw_diff` string field in
JSON, and an indented `raw diff:` section in text — always ahead of the
fragments.
