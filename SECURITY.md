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
as bytes; tree-sitter parsers (when the `[tree-sitter]` extra is installed)
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
