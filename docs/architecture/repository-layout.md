# Repository ownership boundaries

The repository contains a shipped product and a reproducible research track,
and their source, data and generated outputs have different lifecycles. The
per-directory inventory that used to sit here was deleted on 2026-09-06: `ls`
and the diff-context map already answer "what is in this tree", and the table
had started to disagree with them (it described `docs/` as documentation
without mentioning it is the published GitHub Pages site). What follows is
only what a reader cannot recover by looking.

## Executable surfaces

- `python -m diffctx` and the `diffctx` console script enter through
  `diffctx.cli:main`.
- `diffctx-mcp` enters through `diffctx.mcp.server:main`.
- `python -m eval <subcommand>` is the single evaluation command dispatcher.
- The standalone Rust binary is built from `crates/diffctx-native/`.

The `diffctx-native` crate intentionally combines the Rust library, native
binary, optional PyO3 bridge, and native diagnostics.

## Python/Rust boundary

Rust owns diff discovery, graph construction, scoring, selection, and rendering
for diff-context mode. Python owns packaging, tree-mapping presentation, the
user CLI, and MCP integration. `diffctx._native` contains only adapters around
the native extension; it is private and is not a second algorithmic
implementation.
