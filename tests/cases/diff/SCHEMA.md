# Test Case Schema v3

## Root Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `name` | string | No | Test case name (defaults to filename stem) |
| `tags` | list[str] | No | Labels: language, scenario type, etc. |
| `repo` | dict | Yes | Git repo setup |
| `fixtures` | dict | No | Additional test fixtures |
| `fragments` | list[dict] | No | Named fragment declarations with selectors |
| `oracle` | dict | No | Pass/fail conditions |
| `accept` | dict | No | How selectors match output fragments |
| `xfail` | dict | No | Mark test as expected failure |
| `min_score` | float | No | Per-case score threshold; falls back to `DIFFCTX_YAML_MIN_SCORE`, then 10.0 |

## repo

```yaml
repo:
  initial_files:
    src/auth.py: |
      def check(user, password):
          return True
  changed_files:
    src/auth.py: |
      def check(user, password, mfa_code=None):
          return True
  commit_message: "Add MFA parameter"
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `initial_files` | dict[str, str] | `{}` | File path → content before the change |
| `changed_files` | dict[str, str] | `{}` | File path → content after the change |
| `deleted_files` | list[str] | `[]` | Paths removed by the change (git records a deletion) |
| `renamed_files` | dict[str, str] | `{}` | Old path → new path; a `changed_files` entry for the new path overlays the moved content (rename + edit) |
| `commit_message` | string | `"Update files"` | Git commit message for the diff |

Order of application: renames first, then `changed_files` writes, then
deletions — so a deleted path cannot be resurrected by also appearing in
`changed_files`. Whether git records an R or a delete+add pair follows
git's own similarity detection.

## fixtures

```yaml
fixtures:
  auto_garbage: false
  distractors:
    src/legacy/v2_auth.py: |
      def check(user, password, mfa_code=None):
          return False
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `auto_garbage` | bool | `false` | Auto-add conftest garbage files for isolation testing |
| `distractors` | dict[str, str] | `{}` | Additional adversarial files added to the repo (not changed) |

When `auto_garbage: true`, the runner adds ~10 standard unrelated files and
verifies their markers don't appear in output.

## fragments

Named declarations of code fragments we care about. Used by `oracle` to express
requirements.

```yaml
fragments:
  - id: auth.check
    selector:
      path: src/auth.py
      anchor: "def check("

  - id: settings.mfa
    selector:
      any_of:
        - {path: config/settings.py, symbol: MFA_REQUIRED, kind: variable}
        - {path: config/settings.py, anchor: "MFA_REQUIRED = True"}

  - id: legacy.distractor
    selector:
      path: src/legacy/v2_auth.py
      symbol: check
      kind: function
```

### Selector Fields

| Field | Type | Description |
|-------|------|-------------|
| `path` | string | File path — exact match or path-component suffix (`auth.py` matches `src/auth.py`; `sr/auth.py` and `auth` do not) |
| `symbol` | string | Symbol name (function, class, variable name) |
| `kind` | string | Symbol kind: `function`, `class`, `variable`, etc. — only enforced when `accept.kind_must_match: true` (default `false`: a specified `kind` is ignored) |
| `anchor` | string | Content anchor — must appear in fragment content or path |
| `any_of` | list[selector] | Match if ANY sub-selector matches |

A selector matches an output fragment if ALL specified-and-enforced fields
match. Unspecified fields are ignored.

## oracle

Declares which named fragments must/can/must-not appear in output.

```yaml
oracle:
  required:
    - auth.check
  allowed:
    - settings.mfa
  forbidden:
    - legacy.distractor
```

**Pass/fail semantics:**

Let `O` = output fragments, `M(O)` = set of fragment IDs matched in `O`:

1. Every `required` ID must be in `M(O)`
2. Every `forbidden` ID must NOT be in `M(O)`
3. `allowed` IDs may be present or absent — no effect on pass/fail

Score = `required_recall × (1 - forbidden_rate) × 100`

where:

- `required_recall` = matched required / total required
- `forbidden_rate` = matched forbidden / total forbidden

## accept

Controls how selectors are matched against output fragments.

```yaml
accept:
  symbol_match: exact       # exact | prefix | substring
  kind_must_match: false    # enforce kind field equality
  span_relation: exact_or_enclosing  # informational
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol_match` | string | `exact` | How to compare `selector.symbol` with `fragment.symbol` |
| `kind_must_match` | bool | `false` | Whether `selector.kind` must equal `fragment.kind` |
| `span_relation` | string | `exact_or_enclosing` | Informational: expected relationship between output span and gold span |

## xfail

```yaml
xfail:
  category: parser            # null | parser | ranking | language_support | known_bug
  reason: "tree-sitter lacks Julia support"
  issue: null                 # issue URL or number
```

All fields default to `null`. If `reason` or `category` is set, the test is
marked as expected failure (xfail).

## Minimal example

```yaml
name: mfa_arg_added

repo:
  initial_files:
    src/auth.py: |
      def check(user, password):
          return True
  changed_files:
    src/auth.py: |
      def check(user, password, mfa_code=None):
          return True

fragments:
  - id: auth_check
    selector:
      path: src/auth.py
      anchor: mfa_code

oracle:
  required: [auth_check]
```

## Full example

```yaml
name: mfa_arg_propagation
tags: [python, auth, arg_addition]

repo:
  initial_files:
    src/auth.py: |
      def check(user, password):
          return True
    src/handlers/login.py: |
      def login(user, password):
          return check(user, password)
    config/settings.py: |
      MFA_REQUIRED = True
  changed_files:
    src/auth.py: |
      def check(user, password, mfa_code=None):
          return True
    src/handlers/login.py: |
      def login(user, password, mfa_code=None):
          return check(user, password, mfa_code)
  commit_message: "Pass MFA code into auth check"

fixtures:
  auto_garbage: false
  distractors:
    src/legacy/v2_auth.py: |
      def check(user, password, mfa_code=None):
          return False

fragments:
  - id: handler.login
    selector:
      path: src/handlers/login.py
      symbol: login
      kind: function

  - id: auth.check
    selector:
      path: src/auth.py
      symbol: check
      kind: function

  - id: settings.mfa
    selector:
      any_of:
        - {path: config/settings.py, symbol: MFA_REQUIRED, kind: variable}
        - {path: config/settings.py, anchor: "MFA_REQUIRED = True"}

  - id: legacy.distractor
    selector:
      path: src/legacy/v2_auth.py
      symbol: check
      kind: function

oracle:
  required: [handler.login, auth.check]
  allowed: [settings.mfa]
  forbidden: [legacy.distractor]

accept:
  symbol_match: exact
  kind_must_match: true
  span_relation: exact_or_enclosing

xfail:
  category: null
  reason: null
  issue: null
```

## Runner support matrix

Two runners execute these YAML cases.

| Feature | `cargo test --test yaml_cases` | `cargo run --example diffctx-test` |
|---------|-------------------------------|--------------------------------|
| `oracle.required` | enforced | enforced |
| `oracle.forbidden` | enforced | enforced |
| `oracle.allowed` | informational only | informational only |
| `fixtures.distractors` | yes | yes |
| `fixtures.auto_garbage` | yes | yes |
| `xfail` (active) | skipped | tracked separately |
| `min_score` (per-case) | overrides env default | ignored |
| Pipeline | real git repo via `tempfile` | in-memory `MemoryRepo` |
| Parallelism | `libtest-mimic` threads | `rayon` |
| Use case | CI regression gate | bulk benchmarking, JSON reports |

Both runners share schema parsing, oracle evaluation and budget calculation
via `crates/diffctx-native/tests/common/mod.rs`. Per-case `min_score` is
supported in the YAML schema:

```yaml
min_score: 50.0
```

If `min_score` is omitted, runners fall back to `DIFFCTX_YAML_MIN_SCORE`
environment variable, then to the built-in default (10.0).

## Known-failure baseline (the actual CI gate)

CI runs the full corpus and gates every case against
`crates/diffctx-native/tests/known_below_threshold.txt` — **bidirectionally**:
a case scoring below threshold fails unless listed, and a listed case that
starts passing also fails (so improvements must be claimed by removing the
entry). When your edit changes a case's outcome, update the baseline file in
the same commit. `DIFFCTX_YAML_IGNORE_BASELINE=1` (nightly) reports instead of
gating.
