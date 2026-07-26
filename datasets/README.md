# Versioned evaluation data

This directory contains immutable inputs required for offline reproducibility.
Executable evaluation logic belongs in `eval/`; generated run output belongs in
the ignored `results/` directory or CI/release artifacts.

- `dcbench/v1/`: first-party annotated instances, provenance, and checksums.
- `real-world-diff/v1/`: the legacy corpus, labels, provenance, and checksums.
- `eval-splits/`: frozen manifests with versioned integrity metadata.
- `external-revisions.json`: pinned revisions for externally hosted datasets.
