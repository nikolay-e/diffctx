# Token counting and the raw diff bundle

Reference for two questions the CLI answers only in passing: *whose* tokens
`--budget` counts, and what `--with-raw-diff` costs.

## Which tokenizer diffctx uses

Every token number diffctx prints or enforces comes from **tiktoken's
`o200k_base` encoder** (the GPT-4o / GPT-4.1 tokenizer); the stderr summary
names it. Counts are exact only for OpenAI models on that encoder — Claude,
Gemini, Llama and friends tokenize differently, typically within
single-to-low-double-digit percent either way.

`--budget N` therefore means exactly *N o200k_base accounting tokens*, and
every artifact says so: the `provenance` block of the JSON/YAML output carries
the effective configuration hash, and `DIFFCTX_PROVENANCE=full` adds the whole
record — tokenizer id, every parameter, the resource caps — so a consumer
converting to another model's count knows what it is converting from. The
full record costs ~500 tokens, which is why it is opt-in.

For a non-OpenAI consumer leave headroom — e.g. `--budget 28000` when aiming
at a 32k window — or set `DIFFCTX_TOKEN_SAFETY_FACTOR` (a multiplier ≥ 1.0,
default 1.0): every count is scaled by it, so the budget is enforced against
the inflated numbers and the factor is recorded in provenance. diffctx ships
no model-specific default for it on purpose — the right margin is the
consumer's own measurement, and `eval/tools/token_count_validate.py` is the
script that takes it against a model's real token-count endpoint. If a hard
guarantee matters, measure the produced file with your own model's tokenizer.

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
stays within whatever budget it is given. It did not always: until #241 the
budget bounded only the fragments, so the change summary — commit message plus
the changed/deleted/renamed/lockfile/ignored path lists — rendered for free and
was printed twice, once in full and once again as a "not represented" footer.
On an 83-file range that put `--budget 1000` at 3 150 tokens and `--budget 0`
at 2 464, with the selection dutifully under budget the whole time. The summary
is now charged before selection starts and printed once, with omitted entries
marked in place.

One consequence is deliberate: when the summary alone exceeds the budget, the
summary is what you get and no fragment is selected. A changed path is never
dropped to fit — a reader who cannot see what changed is worse off than one who
is over budget — so on a very wide change a very small `--budget` is a request
the artifact cannot honour, and it says so by containing nothing else — and, since
2.0.0, in words: `fit.least` in JSON and YAML holds the tokens the least answer takes
(a budget of that size delivers it), and Markdown and text open with a "budget too
small" line. Locate does the same.

`--mode locate` bounds the document as printed. The ranked candidates the budget left
out are always counted (`overflow.total`); their entries, without bodies, are listed
only with `--overflow` (MCP `include_overflow`) and are charged like any other byte, so
a tight budget sheds them first.

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
- **Every door can ask for it.** The CLIs (`--with-raw-diff`, pack mode only; the
  `pip`/`pipx` CLI and the standalone binary alike), the Python API
  (`build_diff_context(..., with_raw_diff=True)`) and the MCP server
  (`diffctx_context(mode="pack", include_raw_diff=true)`) pass the same request
  to the engine and get the same patch.

Rendering per format: a fenced ` ```diff ` block under a `## Raw diff` heading
in Markdown, a `raw_diff` block scalar in YAML, a `raw_diff` string field in
JSON, and an indented `raw diff:` section in text — always ahead of the
fragments.

## The MCP server: `budget_tokens` and the `max_tokens` ceiling

`diffctx_context` takes two numbers that mean different things. `budget_tokens`
is what the caller asks for, with the CLI's meaning: omit it and each mode uses
its own default — 2 000 tokens for `impact`, the auto budget above for `locate`
and `pack` — so "under 2k tokens" holds for an impact answer through the MCP as
it does at the command line; `-1` is unlimited and `0` leaves no fragments.

`max_tokens` (default 25 000) is a ceiling on the whole reply, the notes in it
included, and the engine holds the answer under it. A budget above it is lowered
to it and the reply opens with a note saying so; an answer larger than the
ceiling is cut to it the way a budget cuts it, never refused whole. Only a
ceiling below the least an answer can say (the header and the first omitted
name for `impact`, the change inventory for `pack`, one item per changed file for
`locate`) sends a notice instead, naming how many tokens the answer takes. The
raw diff, which `--budget` does not charge, counts against the ceiling: it is
in the reply.
