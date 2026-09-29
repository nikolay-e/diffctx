# Plugin evals

`claude plugin eval ./plugin --trust-plugin --scaffold --ablation with-without`.

- `review-last-commit`, `unrelated-request`: skill triggering, with and
  without the plugin.
- `breaking-caller`: the number under the product goal — with the change
  uncommitted and two callers about to break, does the agent handle or name
  them before `git commit`? It needs `--allow-tools Bash` and the hook's
  binary: on a machine without the released plugin version installed, point
  the hook at a build with `DIFFCTX_HOOK_BIN=<path to diffctx>`. A machine
  whose Bash sandbox cannot start (a symbolic link inside the Docker
  credential store is one cause) runs it on Linux CI instead.
