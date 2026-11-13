# Migrating to diffctx 2.0.0

2.0.0 moves the JSON of every mode to a v2 schema and removes v1 (no
compatibility flag). Exit codes, `--budget` and `-f md` Markdown keep their
meaning; the Markdown's wording changed where [the table at the end](#markdown)
says so. A document names its version in `schema`: `diffctx.impact.v2`,
`diffctx.locate.v2`, `diffctx.context.v2`.

This file covers `diffctx.impact.v2`, `diffctx.locate.v2` and the pack artifact
`diffctx.context.v2` field by field.

## What changed, in five lines

1. One value, `Answer`, is rendered by every door; no door counts for itself.
2. Every list is `{total, items}`; what a list omits is `total − len(items)`,
   never a field.
3. Every count of uses is `{n, exact}`; `exact` holds only under a complete
   search.
4. Every unknown is a structured `gaps` entry; no JSON field carries a sentence.
5. Every row carries one `guard` for its test fact and one `standing` for how
   sure it is.

## `diffctx.impact.v1` → `diffctx.impact.v2`

Kept = same name and meaning. Moved = same fact, new place. Merged = several v1
fields, one v2 field. Dropped = derived or internal, with the reason.

### Top level

| v1 | v2 | |
| --- | --- | --- |
| `schema`, `name` | `schema` (`diffctx.impact.v2`), `name` | kept |
| `range`, `index_tree`, `commit_count` | `range{kind, spec, index_tree, commits}` | merged; `kind` is `committed`, `staged`, `worktree`, `window` or `symbol` (locate and pack add `messages`, below) |
| `symbol` | `symbol` | kept |
| `symbol_named` | `spelled{files, rows, gap}` | `caveat` (prose) became a `gap` |
| `changed_files`, `changed_files_omitted`, `untracked_files`, `untracked_files_omitted`, `deleted_files`, `lockfile_changes`, `withheld_files` | `inventory.{changed, untracked, deleted, lockfiles}: {total, items}`, `inventory.withheld` | merged; the `*_omitted` counters are `total − len(items)` |
| `changed` | `definitions` | moved; the entry with `kind: "omitted"` is gone (see `omitted`) |
| (the omitted entry, `unresolved: "N changed definition(s) omitted…"`, its `omitted[]`) | `omitted{definitions: {total, items[{def, shed}]}, needs}` | `total` is the one count of shed definitions |
| `contracts`, `contracts_omitted` | `contracts: {total, items}` | kept shape |
| `stale_references`, `stale_references_omitted` | `stale: {total, items[{path, line, target, how, to, class}]}` | `kind` ("deleted" / "renamed to X") split into `how` and `to` |
| `named_by`, `named_by_omitted` | `named.by_path` (rows without `key`), `named.by_key` (rows with `key`) | split |
| `key_searches`, `collection_reads`, `collection_reads_omitted` | `named.key_searches`, `named.collections` | moved |
| `limits` | `coverage.limits` | moved |
| `unanalysed_files`, `unanalysed_files_omitted` | `coverage.unanalysed: {total, items}` | moved; the file `reason` values are kept |
| (not in v1) | `coverage.scope{complete, gaps}` | the run's own witness |
| `tests_known`, `untested_definitions` | `tests{known, hops, definitions_without_link{n, exact}}` | merged |
| `untested_unlisted`, `untested_called_in_diff`, `entry_unlisted`, `layout_only`, `layout_only_omitted` | `unlisted.{no_caller, called_in_diff, entered_by, layout_only}: {total, items}` | merged; the `path::symbol (lines)` strings became `{path, symbol, lines}`; `entered_by` keeps its `role` |
| `deleted_modules`, `deleted_modules_omitted` | `deleted_modules: {total, items}` | kept shape |
| `empty` | `empty` | kept |
| `truncated` | `summary.shed` | the sum of every definition's `shed` |
| `budget_needed` | `fit.least` | `fit` also carries `cap` and `needs`, and is present only when something was shed |
| (not in v1) | `summary{definitions, callers{n, exact}, possible, folded, shed, untested_callers, files}`, `provenance.engine` | the totals the Markdown header prints, and the engine that answered |

### A changed definition (`changed[]` → `definitions[]`)

| v1 | v2 | |
| --- | --- | --- |
| `path`, `symbol`, `lines`, `commits`, `entry` | same | kept; `lines` are base-side when `change.kind` is `removed` |
| `kind` (a definition kind, or `removed`, `local`, `omitted`) | `kind` (a definition kind, absent for a removal read off the text), `change{kind, delta, origin}`, `local_of{enclosing}` | split: `change.kind` is `added`, `removed`, `edited` or `layout_only`; `delta` is `{signature, body, doc, visibility, decorators}`; `origin` is `moved` or `renamed` |
| `callers` | `rows` | renamed |
| `unresolved` (a sentence) | `gaps` (closed, tagged by `kind`) | see the gap table |
| `missed_in` | `missed_in` | kept: the files that spell the name and were not read whole; `coverage.calls.gaps` also says `unaccounted{reason, files}` |
| `untested` | `guard{state: "none"}` | stated only when no test reaches the definition by a static link |
| `compatible_callers` | a `folded` entry, rule `check-the-default` | merged |
| `also_changed` | `also_changed: [{path, symbol, lines}]` | structured |
| (not in v1) | `possibly_also_changed` | changed users whose binding is not resolved: possible, never callers |
| `callers_omitted` | `shed{resolved, candidate, name, untested}` | the ledger; `total` of it is the old count |
| `omitted` | `omitted` (top level) | moved |
| `folded` | `folded[{rule, kind, count{n, exact}, examples[{path, symbol, lines}], parameters, sharing}]` | `count` states its bound; examples are structured |
| (not in v1) | `coverage{calls{complete, gaps}, reads{complete, gaps}}` | `complete` is the witness: no row of a group then means no such use exists in the analysed scope |

### A row (`callers[]` → `rows[]`)

| v1 | v2 | |
| --- | --- | --- |
| `path`, `symbol`, `lines`, `sites` | same | kept |
| `weight` | dropped | internal ranking weight; rows come in the order the cap sheds them |
| `evidence` (`resolved`, `candidate`, `name`, `reexport`) | `standing` (`resolved`, `candidate`, `name`) | `reexport` is a class |
| `reason` | `reason` | kept, candidates only |
| `relation` (`call`, `reference`, `implementation`, `reexports`; absent for `call`) | `class` (always present) | renamed |
| `kind` | `kind` | kept: stated only where the (language, relation) cell is precise |
| `tested_by`, `tested_via`, `file_tested`, `registered_by`, `unread_test` | `guard{state, test, via, registrar}` | merged; `state` is `linked`, `file_only`, `none`, `registered`, `entry_point` (a framework enters it; `registrar` is its sentence), `unread`, `no_tests` or `not_established` (the search for a test stopped at its hop bound, so no absence is claimed) |
| `folded_by` | `folded_by` | kept, only under `--unfold` |

### Gaps (`unresolved` sentences → `gaps`)

`unlanded{name, uses}`, `unpaired{name}`, `base_unresolved{reason}`,
`stopped{reasons}`, `outside_scope{files}`, `unaccounted{reason, files}`,
`withheld{files}`, `untracked{files, name}`, `dispatched{via}`,
`macros{files, what}`, `capped`, `not_traced`,
`ambiguous{name, definitions, language}`, `not_modelled{language}`,
`overridable{via}`, `templated{files}`, `context_unbound{files}`,
`rendered_elsewhere{files}`, `external_source{pointers}`, `source_flag{files}`,
`unread_directories{directories, total}`.
`unaccounted` is the file account of the definition's interaction group: `files`
of them could be neither bound by a cell nor ruled out by the names they spell,
and `reason` says why (`read_failed`, `changed`, `too_large`, `not_read`,
`unparsed`, `no_extractor`, `failed`, `ambiguous`, `manifest`, `other_language`,
`no_group`). `unread_directories` names the first ten untracked directories a
working-tree read did not enter (over its entry budget), `total` counts them: a
use of the name inside is unknown. A search with no gap is `complete: true`; a
count over an incomplete one is `{n, exact: false}`: at least `n`.

## What locate and pack share with impact

One name means one fact in every mode. The impact tables above give the shapes;
these are the v1 fields of locate and pack that moved into them.

| v1 (locate and pack) | v2 | |
| --- | --- | --- |
| `commit_message`, `commit_messages`, `commit_count` | `range.messages`, `range.commits` | merged; `messages` are the newest commits' messages, subject and body, newest first (a run that reads no commits, `--full`, holds the head's subject); `commits` is how many commits the range holds, always stated (v1 stated it only when it exceeded the messages listed) |
| (not in v1) | `range{kind, spec, index_tree}` | what was asked, as in impact |
| `changed_files`, `deleted_files`, `renamed_files`, `lockfile_changes`, `ignored_changes` | `inventory.{changed, deleted, renamed, lockfiles, ignored}: {total, items}` | moved; a list with nothing in it is absent; `renamed` items are `{from, to}` |
| `policy_excluded_count` | `inventory.withheld` | renamed: the one name for files the run withheld |
| `coverage.limit_reasons` | `coverage.limits` | renamed: impact's name for the same vocabulary; `capped`, `limited_files`, `lossy_files` keep theirs |
| `budget_tokens` (locate), `budget_needed` | `fit{asked, cap, needs, least, exempt}` | merged; `cap` is the budget the run resolved, `asked` what was requested (absent for auto), `needs` what showing everything takes to the next hundred tokens, `least` set when the least the answer can say outgrows the cap, `exempt` the tokens the cap does not charge (a pack's raw diff) |
| `provenance`, `redactions` | same | kept; `provenance.engine.build` is where the release smoke reads the build label |

## `diffctx.locate.v1` → `diffctx.locate.v2`

| v1 | v2 | |
| --- | --- | --- |
| `schema`, `name`, `summary`, `items`, `provenance`, `redactions` | same | kept; `items[].path` and `lines` are the fragment id, unchanged |
| `item_count` | dropped | `items \| length`: a count is never stored beside the list it counts |
| `overflow[]`, `overflow_count` | `overflow{total, items}` | `total` is every ranked candidate the budget left out, always stated; `items` (the first 50, without bodies) only with `--overflow` or MCP `include_overflow`, and then charged to the budget like every other byte: the first thing a tight budget sheds, still counted in `total`. v1 delivered the list uncharged from the CLIs and stripped it in the MCP |
| `coverage.{unparsed_files, zero_edge_files, ppr_truncated, next_up, unrepresented_changed_files, no_fragment_changed_files}` | `coverage.selection.{…}` | moved; same fields |
| `coverage.confidence` | dropped | a heuristic that invites reading as a score |

The budget bounds the document as printed (newline included). Below the least
locate can say (the first item of every changed file and the inventory) it says
so in `fit.least` and cuts nothing more.

## `diffctx.context.v1` → `diffctx.context.v2` (pack)

| v1 | v2 | |
| --- | --- | --- |
| `schema`, `name`, `type`, `fragments`, `changes`, `redactions`, `provenance` | same | kept; `type` stays `diff_context`; `provenance.selection` stays where evals read it |
| `fragment_count` | dropped | `fragments \| length` |
| `coverage.status` | `coverage.status` | kept: `complete`, `partial` or `degraded` (a changed file has no witness in the answer); `resources`, `unknown` and `lossy_files` are kept |
| `raw_diff` | `raw_diff` | kept, ahead of `fragments`; its tokens are `fit.exempt` |

The Python API returns the same document as a dict, `latency` beside it;
`fit_artifact(dict, format)` refuses a document of another `schema`. YAML
carries the JSON structure, so it moved with it. Markdown and text for the same
selection are byte-identical to 1.x, with two exceptions: a range's commit
messages print whenever there are any (1.x printed them only when something else
was listed), and `--full -f txt` lists the head's subject under `commits:`
instead of `change:`.

### The ten most-read fields

| v1 | v2 (jq) |
| --- | --- |
| `.changed_files[]` | `.inventory.changed.items[]` |
| `.fragment_count` | `.fragments \| length` |
| `.item_count` | `.items \| length` |
| `.coverage.limit_reasons` | `.coverage.limits` |
| `.budget_needed` | `.fit.least` |
| `.policy_excluded_count` | `.inventory.withheld` |
| `.commit_messages[]` | `.range.messages[]` |
| `.commit_count` | `.range.commits` |
| `.overflow_count` | `.overflow.total` |
| `.coverage.unparsed_files` | `.coverage.selection.unparsed_files` |

<!-- markdownlint-disable-next-line MD033 -->
<a id="markdown"></a>

## Markdown (`-f md`)

Markdown is not versioned; this is where its changes are promised.

- The header says "at least N" whenever the count's search was not complete.
- A deleted module nothing imports is one line: "deleted module M: N
  definition(s), no importers".
- Folded lines say what they are: "possible calls through an untyped receiver
  (not resolved)" for candidates, "not affected (rule)" only for rules that
  cannot affect the rows.
- A definition only the range's own changes use is named on one line, with its
  users ("used by"), possible users apart ("possibly used by").
- Locate and pack say "budget too small" when the least they can say does not
  fit (`fit.least` in their JSON and YAML).
- The header's "N caller(s) without a static test link" counts every row the cap
  shed, whichever stage shed it. v1 counted it at one of the three stages, so a
  shed answer's count moves up; this is a correction.
- A definition with several gaps prints the most limiting one (whole-search
  limits, then resolution limits, then scope), with "; N other limit(s)"
  appended; the JSON carries all of them.
- A definition whose search left a file that names it unread now says so on its
  own line ("N file(s) naming it were not analysed whole"); v1 listed the file
  under the word matches only.
- A count of calls that only fields or parameters with defaults changed says "at
  least N" when its search was not complete.
- `--symbol` on a name no file defines: the files read as text are named by the
  header's "not analysed" block, not inside the first sentence.

## GitHub Action, plugin, MCP

Exit codes and `--output-file` are unchanged. The Action and the plugin move to
the 2.0.0 engine in the release's finalize step; nothing to do unless you parse
the files they write. The MCP `diffctx_context` tool keeps its parameters and
gains `include_overflow` (locate; the CLI's `--overflow`); `budget_tokens` is a
per-mode default (impact 2 000, locate and pack automatic), and `max_tokens` is
a ceiling the reply is fitted to.
