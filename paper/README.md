# Research paper

Each paper version separates authored source from reproducibility and release
artifacts:

- `v*/src/`: TeX and bibliography source.
- `v*/reproducibility/`: selected committed data needed to reproduce claims.
- `v*/releases/`: intentionally committed rendered releases.

The current rendered release is
`v2/releases/diffctx-context-selection-v2.pdf`. The superseded v1 source is
not carried in the tree; it lives at the git tag `paper-v1-grid`.
Transient LaTeX output belongs in an ignored `build/` directory. Raw `.log`
and `.blg` files are not publication or reproducibility artifacts.
