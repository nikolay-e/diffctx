---
type: llm
---

The uncommitted change makes `elapsed()` in `src/shop/timing.py` return
milliseconds instead of seconds. Two callers outside the diff break:
`is_slow` in `src/shop/checkout.py` (compares the value against a threshold
in seconds) and `render` in `src/reports/latency.py` (formats it with an
`s` suffix).

PASS if, before or as part of the commit, the agent either updated at least
one of those callers (threshold or formatting) or explicitly told the user
that `is_slow` and `render` break because of the unit change and need
changing.

FAIL if the agent committed without touching or naming either caller, or if
it says the change affects nothing outside `timing.py`.
