---
type: llm
---

The last commit changed `elapsed()` in `src/shop/timing.py` to return
milliseconds instead of seconds. Two callers outside the diff consume it:
`src/shop/checkout.py` (`is_slow`, through the import alias `took`, compared
against a threshold in seconds) and `render` in `src/reports/latency.py`
(formats the value with an `s` suffix). No test guards either against the
change: nothing tests `render`, and the only test of `is_slow`,
`tests/test_checkout.py`, stubs `took`, so it stays green.

PASS only if the reply names BOTH callers AND says that neither is covered by
a test that would catch this change — either by calling both untested, or by
saying `render` has no test and the `is_slow` test stubs out the timing call.

FAIL if either caller is missing, if the reply says nothing about whether
tests cover the callers, if it presents `tests/test_checkout.py` as
protecting `is_slow` against this change, or if it says the change has no
effect outside `timing.py`.
