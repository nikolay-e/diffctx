# Diff Test Cases Enrichment

## Goal

Enrich all diff test cases with:

1. **Realistic code content** - larger, more representative code snippets
2. **Garbage files** - files with unique markers to test exclusion
3. **Proper diffctx format** - structured assertions

## File Structure

Each test case consists of 3 files:

- `{name}_before.yaml` - codebase state before the change
- `{name}_after.yaml` - codebase state after the change
- `{name}_diffctx.yaml` - assertions for diff context selection

## Garbage Marker Pattern

```text
GARBAGE_{CATEGORY}_{NUM}_{DESCRIPTION}_{LETTER}
```

Examples:

- `GARBAGE_CICD_003_ROLLBACK_MARKER_A`
- `GARBAGE_ALGO_005_LOGGING_MARKER_B`

## diffctx Format

```yaml
must_include_files:
  - path/to/important/file.py
must_include_content:
  - |
    actual code block that must appear
must_not_include:
  - GARBAGE_MARKER_A
  - GarbageClassName
  - garbage_file.py
```

---

## Progress

### ✅ Completed

| Directory | Files | Test Cases | Status |
|-----------|-------|------------|--------|
| algorithm | 63 | 21 | ✅ Done |
| cicd | 45 | 15 | ✅ Done |
| cicd_and_docs | 234 | 78 | ✅ Done |
| comprehensive | 81 | 27 | ✅ Done |
| config | 15 | 5 | ✅ Done |
| cpp | 108 | 36 | ✅ Done |
| csharp | 78 | 26 | ✅ Done |
| dependencies | 123 | 41 | ✅ Done |
| docker | 135 | 45 | ✅ Done |
| fragments | 72 | 24 | ✅ Done |
| frontend | 90 | 30 | ✅ Done |
| go | 105 | 35 | ✅ Done |
| graph | 51 | 17 | ✅ Done |
| helm | 135 | 45 | ✅ Done |
| infrastructure_validation | 15 | 5 | ✅ Done |
| internals | 195 | 65 | ✅ Done |
| java | 78 | 26 | ✅ Done |
| javascript | 255 | 85 | ✅ Done |
| javascript_extended | 147 | 49 | ✅ Done |
| json | 45 | 15 | ✅ Done |
| jvm_and_compiled | 216 | 72 | ✅ Done |
| kubernetes | 120 | 40 | ✅ Done |
| merging | 36 | 12 | ✅ Done |
| operations | 18 | 6 | ✅ Done |
| output | 36 | 12 | ✅ Done |
| patterns | 135 | 45 | ✅ Done |
| php | 60 | 20 | ✅ Done |
| ppr | 105 | 35 | ✅ Done |
| python | 186 | 62 | ✅ Done |
| relations | 21 | 7 | ✅ Done |
| ruby | 60 | 20 | ✅ Done |
| rust | 120 | 40 | ✅ Done |
| scala | 78 | 26 | ✅ Done |
| scripting | 90 | 30 | ✅ Done |
| selection | 24 | 8 | ✅ Done |
| shell | 75 | 25 | ✅ Done |
| swift | 60 | 20 | ✅ Done |
| terraform | 240 | 80 | ✅ Done |
| typescript | 27 | 9 | ✅ Done |
| yaml | 60 | 20 | ✅ Done |

### 🔄 Remaining

None - all directories enriched.

---

## Process

For each test case:

1. **Read** all 3 files (before, after, diffctx)
2. **Check** if already enriched (look for garbage markers)
3. **Enrich** if minimal:
   - Add realistic code to before/after
   - Add 2 garbage files with unique markers
   - Update diffctx to proper format
4. **Write** updated files

## Notes

- Process one directory at a time
- Use consistent garbage marker naming per directory
- Keep code realistic for the test scenario
- Ensure diffctx assertions match actual diff content
