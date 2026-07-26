<!-- markdownlint-disable MD013 MD033 MD034 -->

# diffctx Context-Extraction Quality Report

**Sample:** 109 commits across gitpod, sentry, react-native repos (mainline diffctx pipeline, both `ctx.md` and `ctx.yaml` outputs compared against hand-labeled ground truth).

**Headline numbers:** avg precision **0.137**, avg recall **0.283**, avg quality score **11.1/100**. **74.3%** of commits got *insufficient* context (mostly complete pipeline failures), **83.5%** suffered severe token bloat, and **90.8%** were bloated-or-worse. Only a small minority of commits (roughly a dozen of 109) produced a usable, well-scoped context bundle. This is not a tuning problem — it is a tool that fails outright on most real-world commits and over-dumps on the rest.

---

## 1. Relevance / Over-Selection

When the tool *does* produce output, it drags in far more files than the diff touched. The worst confirmed over-dumps, ranked by noise ratio:

| Commit | Changed files | ctx_files rendered | Noise | Precision |
|---|---|---|---|---|
| `react-native/035-fcd6303` | 13 | **120** (9.2x) | ~107 unrelated files | 0.12 |
| `react-native/029-66df63d` | 26 | **120** | 94 files (78%) absent from diff | 0.20 |
| `react-native/009-1cb0a33` | 59 | **123** | 64 files (52%) — vendored debugger JS, unrelated codegen fixtures | 0.25 |
| `react-native/008-59101d6` | 59 | **128** | 64+ files incl. a vendored third-party minified bundle | 0.10 |
| `react-native/007-07abfce` | 60 (mostly deletions) | 6 fragments | 5 of 6 fragments totally unrelated (jsi.h, FabricMountingManagerTest.cpp) | 0.17 |
| `gitpod/036-dd50c2a` | ~38 | 136 fragments | 74/136 fragments (54%) are one generated protobuf file's boilerplate getters | 0.45 |

The pattern is consistent: generic symbol names in new code (e.g. `ComponentDescriptors`, `Props`, `ShadowNodes` in `react-native/009`) pattern-match against unrelated core-engine files of the same generic name, and the selector pulls in whole unrelated subsystems. `react-native/007` is the starkest example of pure noise: a 59-file template deletion needed nothing beyond a deleted-files list, yet 5 of the 6 rendered fragments (jsi.h, an ObjC AppDelegate, a cocoapods test file) have zero connection to the change.

A second over-selection mode is **full-file dumps for one-line edits**: non-tree-sitter-friendly formats (Ruby podspecs, `.mm`, `CMakeLists.txt`, generated `.api`/protobuf files) collapse to a single "chunk" fragment spanning the whole file. `react-native/004-42d6745` (187 insertions/72 deletions, mechanical `.m`→`.mm` rename) renders 49,013 tokens across 140 files because a 1-line podspec edit drags in the full 124-line file, and a 1001–1480 line window of a generated `Podfile.lock` is dumped verbatim with zero semantic value.

## 2. Completeness — Missing / Lost Context

Completeness failure has two distinct faces in this sample:

**(a) Total pipeline failure (the dominant mode).** ~74% of commits returned **0 fragments, 0 tokens** — not "thin" context but *no* context at all. Examples spanning the full size spectrum: `gitpod/006` (147 files, 332k inserted lines) down to `sentry/033-dc56fdd` (13 files, 398 lines — the smallest commit in the batch) and `gitpod/037-0150cf8` (15 files, 1947 lines). Recall is mechanically 0 in these cases regardless of how good the ranking logic is, because nothing ever reaches the reader.

**(b) Selective lost-change signal inside an otherwise-working run.** The clearest confirmed instance is **`react-native/027-d2c48f3`**: diffctx's own "changed files" header lists `RCTViewManager.m`, `StyleSheetTypes.js`, and `src/private/featureflags/ReactNativeFeatureFlags.js` as changed, but **none of the three has a rendered fragment body** — the token budget instead went to similarly-named but *untouched* companion files (`UIView+React.m`, `StyleSheet.d.ts`, `ReactNativeFeatureFlagsBase.js`). This is a real regression hiding inside a `severe_bloat` classification (precision 0.87 looked good on paper, but the actually-declared-changed files silently dropped their bodies) — structurally the same bug class as issue #103.

Also worth flagging: `gitpod/036-dd50c2a` and `react-native/019-37375d8` both show **phantom files** — `editor.pb.go` and `listen-to-workspace-ws-messages2.test.ts` appear in `ctx_files` despite never appearing in `truth.diff` at all, i.e. stray unrelated-file leakage independent of the generated-code-bloat problem.

## 3. Token Efficiency

83.5% of commits are `severe_bloat`. Token cost tracks **file size**, not **diff size** — a structural defect, not noise:

| Commit | Real diff | Rendered tokens | Tokens per changed line |
|---|---|---|---|
| `react-native/005-62e1110` | 47 ins / 47 del (pure renames, 0 content diff) | 24,955 | **~530/line** |
| `react-native/015-03b8b09` | ~220 lines (1 flag renamed) | 47,417 | **~215/line** |
| `react-native/026-9526406` | 385 ins / 68 del | 47,396 | **~123/line** |
| `react-native/004-42d6745` | 187 ins / 72 del | 49,013 | **~262/line** |
| `react-native/029-66df63d` | 26 real files | 47,533 (120 ctx_files) | 78% noise files |
| `sentry/010-a8d6180` | 12,321 ins (genuinely huge — migration squash) | full 9,102-line field-by-field re-emission of a squashed Django migration | defensible size, indefensible content-per-token |

The single biggest driver: React Native's **generated feature-flag accessor files** (`ReactNativeFeatureFlagsAccessor.cpp`, `*.kt` accessors/providers). Adding *one* new flag touches 1–2 lines in a dozen boilerplate files, but diffctx re-renders large stretches of each as "changed" — `react-native/018`, `react-native/027`, `react-native/028` all show this exact pattern independently, each burning ~47k tokens on what is a single semantic edit repeated mechanically. A close second: **regenerated protobuf/Java/Go bindings** (`gitpod/036`'s `OrganizationOuterClass.java` alone supplies 54% of all rendered fragments for one new proto field).

## 4. MD vs YAML

Aggregate: **md_better: 34, yaml_better: 3, format_similar: 66**. Markdown wins by an 11:1 margin whenever a difference registers at all, and never loses meaningfully. Concrete evidence: `react-native/006-610b14e` — "md_tokens < yaml_tokens... Markdown's prose framing reads more efficiently than YAML for this many small text-only fragments." `react-native/005`, `014`, `024`, `035` and the majority of `md_vs_yaml: md_better` rows repeat the same finding: YAML's block-scalar/quoting overhead adds tokens without adding comprehension for code-fragment-heavy output.

**Verdict: switch the default to Markdown.** There is no recorded case in this sample where YAML was clearly superior; the 3 yaml_better cases are outliers, not a competing trend. The 66 "similar" cases are overwhelmingly the hang failures (0 tokens either way, so format is moot) rather than genuine ties on real content — once you exclude hangs, MD's advantage is closer to structural than marginal.

## 5. Systemic Patterns and Top 5 Highest-Leverage Fixes

**Pattern A — the hang (#70) is not a scale problem, it's a correctness bug.** Hangs occur across the entire size spectrum with no monotonic relationship to file count or diff size: `gitpod/001` (2397 files) hangs, but so does `sentry/033-dc56fdd` (13 files, 370 lines, one clean new endpoint), `gitpod/037-0150cf8` (15 files, "smallest in the entire batch"), `react-native/034-f140c49` (14 files, single flag flip), and `sentry/034-757cf20` (13 files, simple directory rename). These are textbook-easy diffs that should return near-instantly. This rules out "just too big" as the explanation and points to a structural trigger — plausibly interaction with renames, generated-file detection, or symbol-graph construction on specific file shapes — that must be root-caused with the *smallest* repro (`sentry/033` or `gitpod/037`), not the largest.

**Pattern B — generated/mechanical content is never special-cased.** Regenerated protobuf/gRPC bindings (`.pb.go`, `*OuterClass.java`, `*_pb.ts`), feature-flag accessor boilerplate, golden test fixtures, and lockfiles repeatedly dominate both the hang triggers and the token bloat, e.g. `gitpod/002/006/022/034`, `react-native/018/027/028/029`. The fix pattern is a `should_not_include` list that appears near-identically across a third of all analyses — this is the single most repeated finding in the corpus.

**Pattern C — mechanical homogeneous diffs (find/replace across N files) get full per-file treatment instead of collapsing to "N call-sites, 1 pattern."** `sentry/001` (680 files, ruff reformat), `sentry/003` (288 files, one token rename), `sentry/004` (286 files, one router-mock helper swap), `sentry/011` (108 files, `moment`→`moment-timezone` import swap) — all hang, and all have an ideal output of a two-sentence summary plus 1–2 representative diffs.

**Top 5 fixes, ranked by expected comprehension-per-token impact:**

1. **Fix the hang (#70) using the smallest repros first** (`sentry/033`, `gitpod/037`, `react-native/034`, `sentry/034` — all <20 files, clean diffs). This single fix would move ~74% of commits from 0 recall to *something*, which dwarfs every other improvement in expected value.
2. **Path/heuristic-based generated-code suppression**: skip or one-line-summarize `*.pb.go`, `*OuterClass.java`, `*_pb.ts`, `*_grpc.pb.go`, Kotlin/C++ feature-flag accessor files, and golden/snapshot fixtures. This alone would cut token cost by 50%+ on a large fraction of the `severe_bloat` set (`react-native/018/027/028/029`, `gitpod/002/006/022/034/036`).
3. **Homogeneous-diff detection**: when N files share one mechanical hunk pattern (rename, import swap, quote-style reformat), render one representative file + a list of the rest, not N full fragments. Targets `sentry/001/003/004/011`, `react-native/019` (Folly version bump).
4. **Rename/zero-content-diff short-circuit**: a file with `similarity index 100%` and no hunk body should never trigger a full-file "chunk" render (`react-native/005`: 530 tokens/changed-line from pure renames).
5. **Switch default output format to Markdown** — a free win (11:1 in favor across measured commits) with no observed downside, and it should ship independently of the above since it compounds with every other fix.
