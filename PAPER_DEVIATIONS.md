# Deviations from Paper

This document tracks implementation deviations from the research
paper [Context-Selection for Git Diff](https://nikolay-eremeev.com/blog/context-selection-git-diff/).

## 1. Caller Importance Weighting for Impact Needs

**Paper reference:** Section 4.2.1 (Impact need scoring)

**Problem:** The paper's `m(f, n)` assigns a flat 0.8 to any
fragment that mentions a symbol for `impact` needs. This cannot
distinguish production callers (`handler.ts`) from peripheral
code (`examples/parsing.ts`) — both receive identical scores.

**Extension:** For impact needs only, the match strength is
scaled by a file importance factor:

```text
m'(f, n) = m(f, n) * I(f)    where n.type == "impact"
```

`I(f)` is computed from three layers:

| Layer | Signal | Importance |
|-------|--------|------------|
| Path patterns | `examples/`, `demo/`, `vendor/`, etc. | 0.15 |
| Generated code | `generated/`, `__generated__/` paths | 0.10 |
| Script dirs | `scripts/`, `tools/`, `bin/` | 0.40 |
| Graph topology | Leaf node (in=0, out>0) | 0.25 |
| Graph topology | Isolated (in=0, out=0) | 0.50 |
| Graph topology | Production (in>0) | min(1.0, 0.7 + 0.1*in) |

Path-based layers take priority over graph topology.

**Submodularity preservation:** Since `I(f) in [0, 1]` is a
constant per-fragment multiplier, `m'(f, n) <= m(f, n)`. The
augmented score `a(f, n) = m'(f, n) + eta * R(f)` remains
monotone submodular — scaling a nonneg input to `phi(max(...))` by
a constant in [0, 1] preserves concavity of the max-of-concave
composition.

**Scope:** Only impact needs are affected. Definition, signature,
test, invariant, and background needs use unmodified `m(f, n)`.
