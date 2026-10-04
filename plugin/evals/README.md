# Plugin evals

`claude plugin eval ./plugin --trust-plugin --scaffold --ablation with-without`.

- `review-last-commit`, `unrelated-request`: skill and tool triggering, with
  and without the plugin. `diffctx_context` is served by a mock: the
  listing in `mocks/diffctx/_tools.json` and the answers in
  `review-last-commit/mocks/diffctx/{impact,pack}.txt` were captured from the
  released server on that case's fixture, so re-capture them when a release
  changes either. The rubric scores the per-caller test verdict that impact
  reports; `impact-called` and `skill-fired` show whether the plugin was used
  and are not scored.
- `breaking-caller`: the number under the product goal — with the change
  uncommitted and two callers about to break, does the agent handle or name
  them before `git commit`? It needs `--allow-tools Bash` and the hook's
  binary: on a machine without the released plugin version installed, point
  the hook at a build with `DIFFCTX_HOOK_BIN=<path to diffctx>`. A machine
  whose Bash sandbox cannot start (a symbolic link inside the Docker
  credential store is one cause) runs it on Linux CI instead. It has no
  mock; to give it the real server add `--allow-real-servers --allow-tools
  mcp__plugin_diffctx_diffctx__diffctx_context`.
