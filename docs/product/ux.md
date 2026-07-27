# UX Findings Log

## 2026-07-07 — a7f398f4

First-time-user CLI ergonomics audit (5 hands-on scouts: flag semantics,
tree mode, diff mode, graph subcommand, errors/edge cases). All 16 fixes
found in this run were applied and verified in-run; the itemized DO list is
dropped as shipped history.

Note: the `--budget 0` semantics fixed in that run were **later reversed**.
Current behavior: `--budget 0` is a strict-zero floor (empty selection);
use `--full` for changed-files-only output.

### VERIFY

- Bare `--diff` on a clean tree exits 4 (kept deliberately for scripting;
  now documented in `--help` with a "try --diff HEAD~1" hint). Gate: if CI
  users report friction with non-zero "nothing changed", revisit exit 0 +
  `--fail-on-empty`.

### DON'T

- Rewrite `graph` as real argparse subparsers — the argv[0] dispatch now
  warns on the two confusing cases; a breaking CLI restructure isn't
  justified by what remains.
- `-o` overwrite confirmation — standard CLI behavior; `--save` targets are
  auto-ignored.
- Format-aware `--summary -f json` machine output — warning suffices until
  someone asks.
- Auto-ignoring stale non-default output files from previous runs —
  generically unsolvable.
- `--staged`/`--cached` diff scope — feature request, not a defect; revisit
  on demand.
