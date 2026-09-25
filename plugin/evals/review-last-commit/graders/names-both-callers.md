---
type: llm
---

The last commit changed `elapsed()` in `src/shop/timing.py` to return
milliseconds instead of seconds.

PASS only if the reply identifies BOTH affected callers:
`is_slow` in `src/shop/checkout.py` (it imports `elapsed` under the alias
`took` and compares it against a threshold in seconds), and `render` in
`src/reports/latency.py` (it formats the value with an `s` suffix).

FAIL if either caller is missing, or if the reply says the change has no
effect outside `timing.py`.
