"""Baseline EvalFns for cross-method comparison.

Each baseline exports a `make_<name>_eval_fn(repos_dir)` factory that returns
a callable matching the runner's `EvalFn` protocol — same signature as
`eval.harness.diffctx_eval_fn.make_diffctx_eval_fn`. They share the repo
clone/patch/revert lifecycle from `eval.harness.common` and emit `EvalResult`s
with file-level metrics only (`selected_fragments=None`).
"""
