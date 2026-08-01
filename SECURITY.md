# Security Policy

## Threat model

diffctx is a **local-only command-line tool**. The core CLI:

- walks the filesystem under user-supplied paths,
- shells out to `git` (subprocess, UTF-8) to read diff hunks and commit history,
- reads the contents of source files it finds,
- writes serialized YAML/JSON/Markdown/text to stdout, to a file, or to the
  system clipboard.

It does **not** open network sockets, does **not** download remote content,
and does **not** execute any code it reads. Untrusted repositories are read
as bytes; the bundled tree-sitter parsers operate on those bytes in memory
without `exec`/`eval`.

The optional **MCP server** (`diffctx-mcp`, shipped via the `[mcp]`
extra) is the only network-adjacent component. It speaks the Model
Context Protocol over stdio to a parent AI assistant and is intended to run
under that assistant's process, not as a standalone daemon. Its filesystem
reach is confined by the `DIFFCTX_ALLOWED_PATHS` environment variable —
an OS-pathsep-separated list (`:` on POSIX, `;` on Windows) of directories
the server is permitted to read. Paths outside the allow-list are rejected
before any filesystem call; operators should set the narrowest list the
assistant actually needs.

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

### Path confinement

Every path a caller supplies to the MCP server is resolved before any decision
is made about it, and every check runs on the resolved form. This is what makes
a symlink inside the repository that points out of it non-exploitable: such a
path is lexically internal, so a `..`-and-absolute check alone admits it. That
exact shape was a live escape in the `fragment_ids` reader until a property fuzz
over hostile path forms found it reading a planted file above the repository
root; containment is now decided after resolution on both the fetch and glob
surfaces, and the fuzz runs in CI.

`.gitignore` and `.diffctx/ignore` are enforced identically on every read
surface. They are a confidentiality contract, not a display preference: a path
the repository withholds from diff mode is not readable through a glob or by
naming its fragment id either.

Error payloads follow the same boundary. They may echo the argument a caller
sent, since the caller already has it, but they never disclose what the
filesystem resolved it to and never carry a traceback — a refusal that names a
symlink's target hands the caller the very thing it was denied. Payloads reach a
model that may relay them onward.

### Prompt injection via repository content

diffctx exists to move repository text into an LLM's context window. That
text is attacker-controlled whenever the repository is (a fork PR, a vendored
dependency, a repo under review), and diffctx will faithfully select and
render injected instructions if they are relevant to the diff — that is the
tool working as designed.

Mitigations: output is structurally delimited (fenced blocks / YAML scalars
with explicit file paths); MCP tool descriptions state, in the text the model
actually reads, that returned content is untrusted data; private-key-shaped
paths (`*.pem`, `*.key`, `id_rsa`, …) are excluded from selection. diffctx
does **not** detect or neutralize injection attempts, and does **not** redact
arbitrary secrets committed in plaintext — delimiting is a convention, not a
security boundary.

Treat diffctx output exactly as you would treat the repository itself: as
untrusted input. Do not wire it into an agent that holds credentials, can
write, or can reach the network without a human in the loop.

### Verifying a release

Every artifact attached to a GitHub release — wheels, sdist, standalone
binaries, and the SBOM — carries signed SLSA build provenance produced by the
release workflow's own OIDC identity:

```bash
gh attestation verify diffctx-<version>-<target>.zip --repo nikolay-e/diffctx
```

A failure means the file was not produced by that workflow from this repository,
whatever the filename says. The standalone binaries are the case this matters
most for: unlike the wheels, which additionally carry PyPI attestations
(visible as "Verified details" on the project page), nothing else vouches for a
file downloaded from the releases page.

A CycloneDX SBOM (`diffctx-sbom.cyclonedx.json`) ships with each release. It
answers what is inside the artifact, which provenance does not — the pair is
what makes it possible to answer "are we affected" about a published CVE
without reading the dependency tree by hand.

## Reporting

**Please do NOT report security vulnerabilities through public GitHub
issues.**

Preferred channel: [GitHub's private vulnerability reporting](https://github.com/nikolay-e/diffctx/security/advisories/new).

Backup channel: email **<nikolay.eremeev@outlook.com>** with
`[diffctx-security]` in the subject line.

We follow coordinated disclosure: report privately, we confirm and fix, the
advisory and reporter's write-up follow the release.
