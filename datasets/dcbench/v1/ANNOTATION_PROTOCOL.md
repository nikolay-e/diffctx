# dcbench gold-annotation protocol (LLM reviewer)

You are annotating ONE instance at a time: a real commit in a real repository.
Goal: decide which files a reader needs to UNDERSTAND this change — the gold
context — with evidence, not vibes.

## Ground rules (non-negotiable)

1. **Read-only git.** Repos under `test-repos/` are shared by concurrent
   annotators. Use ONLY `git -C test-repos/<repo> show <commit>:<path>`,
   `git -C ... grep <pattern> <commit> [-- <pathspec>]`,
   `git -C ... diff-tree --no-commit-id --name-only -r <commit>`,
   `git -C ... log --format=... -n...`. NEVER checkout, never create
   worktrees, never write into test-repos.
2. **Evidence per gold entry.** Every gold file must have a `rationale`
   naming the concrete symbol/line/mechanism that links it to the change
   (e.g. "defines `RuntimeValidatableParameterInfoResolver` that the patch
   overrides"). If you cannot name the link, it is not gold.
3. **Nontrivial gold is the point.** Look hard for context OUTSIDE the diff:
   definitions of symbols the patch uses/modifies, callers/consumers of
   changed APIs, covering tests, coupled configs/build files, codegen
   sources, docs describing the changed behavior. If, after a genuine
   search, the change truly needs no external context (pure mechanical
   rename, changelog-only), set `no_nontrivial_context: true` with a
   one-line justification INSTEAD of inventing gold. Do not pad.
4. **Forbidden list.** Add 2–4 files that LOOK relevant (siblings, similar
   names, same directory) but are NOT needed — with a one-clause reason
   each. These score precision later.
5. **string_indirection.** Set `true` if the key linkage runs through a
   string (DI by name, event bus, URL routing, task names) rather than a
   static reference.
6. **candidates.yaml, if present** in the instance dir, is a HINT list from
   mechanical generators (cochange/bm25/graph/random) — judge each, but do
   not limit yourself to it, and do not trust it: random entries are
   deliberate distractors.

## What to write

Edit the instance's `annotation.yaml` IN PLACE, updating ONLY these fields
(keep everything else byte-identical, keep valid YAML):

```yaml
gold:
  - path: <repo-relative path>
    tier: essential | helpful      # essential = cannot understand change without it
    role: definition | caller | test | config | doc | cochange
    in_diff: true | false          # true only if the path is in the commit's diff
    rationale: <one concrete sentence naming the linking symbol/mechanism>
forbidden:
  - path: <path>
    reason: <one clause>
nontrivial_gold_count: <count of gold entries with in_diff: false>
string_indirection: true | false
no_nontrivial_context: true        # ONLY with justification in notes, instead of fake gold
annotator: llm/sonnet-5-2026-07-22
candidates_from: [agent-exploration]   # append generator names if candidates.yaml was consulted
notes: <2-3 sentences: what the change does + any annotation caveat>
```

Include the 1–5 CENTRAL diff files as gold too (`in_diff: true`, usually
`essential`) — for very wide diffs pick the semantic core, not every touched
file. Target total gold size: typically 3–10 entries. Quality over count.

## Workflow per instance

1. Read `annotation.yaml` (repo, commit, base_commit, curation description)
   and `patch.diff` (if absent: `git show <commit>` from the repo).
2. Understand the change: what was the intent, which symbols changed.
3. Hunt context with `git grep` at the pinned commit: definitions of used
   symbols, callers of changed functions, tests referencing them, configs.
4. Verify every gold path exists at the commit
   (`git show <commit>:<path>` succeeds) and whether it is in the diff.
5. Write the YAML update. Validate mentally that it parses.
