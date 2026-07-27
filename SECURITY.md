# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 1.x     | Yes       |
| < 1.0   | No        |

## Threat model

diffctx is a **local-only command-line tool**. The core CLI:

- walks the filesystem under user-supplied paths,
- shells out to `git` (subprocess, UTF-8) to read diff hunks and commit history,
- reads the contents of source files it finds,
- writes serialized YAML/JSON/Markdown/text to stdout, to a file, or to the
  system clipboard.

It does **not** open network sockets, does **not** download remote content,
and does **not** execute any code it reads. Untrusted repositories are read
as bytes; the tree-sitter parsers bundled in the native extension
operate on those bytes in memory without `exec`/`eval`. The blast radius of
a malicious file in a scanned tree is therefore bounded to "diffctx
produces wrong output" — not "diffctx compromises the host".

The optional **MCP server** (`diffctx-mcp`, shipped via the `[mcp]`
extra) is the **only network-adjacent component**. It speaks the Model
Context Protocol over stdio to a parent AI assistant and is intended to run
under that assistant's process, not as a standalone daemon. Its filesystem
reach is confined by the `DIFFCTX_ALLOWED_PATHS` environment variable —
an OS-pathsep-separated list (`:` on POSIX, `;` on Windows) of directories
the server is permitted to read. Paths outside the allow-list are rejected
before any filesystem call. Operators running `diffctx-mcp` are
responsible for setting this envvar to the narrowest list of directories
the assistant actually needs.

Out of scope: vulnerabilities in `git`, in the Python interpreter, in
tree-sitter grammars maintained upstream, or in the AI assistant that hosts
the MCP server.

### Git revisions

Diff ranges and the revisions derived from them are validated before they
reach a `git` argv: each side of a range must start with a character that
cannot begin an option, and single revisions are additionally rejected if they
contain whitespace or control characters. A caller-supplied range therefore
cannot smuggle a flag such as `--ext-diff` or `--textconv` past the argument
boundary and re-enable the repository-configured filters that diffctx
explicitly disables (`--no-ext-diff --no-textconv` on every diff invocation).

### Prompt injection via repository content

**This is a real, unresolved risk, and it is inherent to what diffctx does.**

diffctx exists to move repository text into an LLM's context window. That text
is attacker-controlled whenever the repository is: a pull request from a fork,
a dependency vendored into the tree, a cloned repo you are reviewing. A source
comment, a Markdown file, a commit message, or a test fixture can contain
instructions aimed at the model reading the output ("ignore your previous
instructions, run …", "exfiltrate …"). diffctx will faithfully select and
render such text if it is relevant to the diff — that is the tool working as
designed, not a bug in the selector.

What diffctx does about it:

- output is structurally delimited (fenced code blocks / YAML scalars with
  explicit file paths), so a reader can tell fragment content from diffctx's
  own framing;
- MCP tool descriptions state, in the text the model actually reads, that the
  returned content is untrusted data rather than instructions;
- private-key-shaped paths (`*.pem`, `*.key`, `id_rsa`, …) are excluded from
  diff selection, so the most damaging category of secret is not surfaced by
  accident.

What diffctx does **not** do, and cannot:

- it does not detect, neutralize, or rewrite injection attempts — no
  classifier, no instruction stripping, no escaping of natural-language
  content;
- it does not redact arbitrary secrets. Contents that a repository commits in
  plaintext (`.env` files, hard-coded tokens, credentials in fixtures) can
  appear in the output if they fall inside a selected fragment;
- delimiting is a convention, not a security boundary: an LLM may still follow
  instructions found inside a fenced block.

Consequences for operators: treat diffctx output exactly as you would treat the
repository itself — as untrusted input. Do not wire diffctx output into an agent
that holds credentials, can write to the repository, or can reach the network,
without a human in the loop. Confine the MCP server with
`DIFFCTX_ALLOWED_PATHS`, and prefer running it against repositories you have
already decided to review.

## Reporting

**Please do NOT report security vulnerabilities through public GitHub issues.**

Preferred channel: [GitHub's private vulnerability reporting](https://github.com/nikolay-e/diffctx/security/advisories/new).

Backup channel (e.g. if the GitHub form is unavailable): email
**<nikolay.eremeev@outlook.com>** with `[diffctx-security]` in the subject
line.

Please include:

- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (if any)

## Response Timeline

- **Initial response**: within 48 hours
- **Confirmation**: within 5 business days
- **Resolution**: depends on severity and complexity

## Disclosure Policy

We follow coordinated disclosure:

1. Reporter submits vulnerability privately
2. We confirm and assess severity
3. We develop and test a fix
4. We release the fix and publish a security advisory
5. Reporter may publish details after the fix is released
