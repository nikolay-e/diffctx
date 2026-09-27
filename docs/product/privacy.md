# Privacy

diffctx collects nothing. It has no account, no telemetry, no analytics and no
server of its own, and it makes no model or API calls.

- **CLI, Python API and MCP server** run on your machine. They read the git
  repository you point them at (`git diff`, `git show`, file contents),
  honour `.gitignore` and `.diffctx/ignore`, and write their output to
  stdout, a file you name, or your clipboard when you ask. Nothing is sent
  anywhere.
- **Claude plugin** starts that same MCP server through `uvx`. The first start
  downloads the pinned `diffctx` package and its pinned dependencies from
  PyPI; after that nothing is fetched. On claude.ai the plugin's skills run
  the CLI inside Claude's own code-execution sandbox instead, under
  Anthropic's terms for that sandbox.
- **GitHub Action** runs inside your own workflow and hands its result back
  as a step output and a file; what happens to it next is your workflow's
  choice.
- **This site** is static pages on GitHub Pages with no scripts that report
  anything; GitHub's own [privacy statement](https://docs.github.com/site-policy/privacy-policies/github-general-privacy-statement)
  covers the hosting.

Questions: [open an issue](https://github.com/nikolay-e/diffctx/issues).
