# Repository ownership boundaries

The repository contains a shipped product and a reproducible research track.
Their source, data, and generated outputs have different lifecycles:

| Path | Owner and lifecycle |
|---|---|
| `crates/diffctx-native/` | Authoritative Rust algorithmic core and native CLI |
| `Cargo.toml`, `Cargo.lock` | Rust workspace definition and committed release lock |
| `src/diffctx/` | Python package, CLI, MCP server, and thin Rust adapters |
| `tests/` | Product tests, `tests/eval/`, and stable declarative golden cases |
| `eval/` | Evaluation CLI, orchestration modules (`eval/workflows/`, Python — not CI YAML), dataset tooling, harness, baselines, and analysis |
| `datasets/` | Versioned immutable corpora, split manifests, and revision pins |
| `paper/` | Paper source, selected reproducibility artifacts, and releases |
| `docs/` | Engineering, product, and architecture documentation |
| `scripts/` | Repository automation (image builds, cache baking, sensitivity checks) |
| `packaging/` | Release-channel manifests (npm wrapper) with pinned checksums |
| `results/` | Local or CI-generated evaluation output; not source-controlled |
| `.github/` | CI/CD workflows and issue templates; owned by repo automation |

## Executable surfaces

- `python -m diffctx` and the `diffctx` console script enter through
  `diffctx.cli:main`.
- `diffctx-mcp` enters through `diffctx.mcp.server:main`.
- `python -m eval <subcommand>` is the single evaluation command dispatcher.
- The standalone Rust binary is built from `crates/diffctx-native/`.

The `diffctx-native` crate intentionally combines the Rust library, native
binary, optional PyO3 bridge, and native diagnostics. It is not presented as a
pure domain-only `core` crate.

## Python/Rust boundary

Rust owns diff discovery, graph construction, scoring, selection, and rendering
for diff-context mode. Python owns packaging, tree-mapping presentation, the
user CLI, and MCP integration. `diffctx._native` contains only adapters around
the native extension; it is private and is not a second algorithmic
implementation.
