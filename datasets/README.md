# Versioned evaluation data

This directory contains immutable inputs required for offline reproducibility.
Executable evaluation logic belongs in `eval/`; generated run output belongs in
the ignored `results/` directory or CI/release artifacts.

- `dcbench/v1/`: first-party annotated instances, provenance, and checksums.
- `real-world-diff/v1/`: the legacy corpus, labels, provenance, and checksums.
- `eval-splits/`: frozen manifests with versioned integrity metadata.
- `external-revisions.json`: pinned revisions for externally hosted datasets.

## Licence constraints on external datasets

Nothing from an external dataset is copied into this repository — only pinned
revisions and instance identifiers. That is what keeps a NoDerivatives licence
satisfiable, and it is worth stating because one of the pins carries one:

| dataset | licence | consequence |
|---|---|---|
| `SWE-Explore-Bench/SWE-Explore-Bench` | CC-BY-NC-ND-4.0 | **ND**: no adapted copy may be redistributed, so fixtures for it are hand-written to the documented schema rather than sampled. **NC**: whether this project's use qualifies as non-commercial is an open question for the maintainer, not something the adapter settles. Note the upstream *code* is MIT while the *data* is not. |

Everything else pinned here is permissively licensed by its publisher; check
before adding a pin, since the licence lives with the dataset and not with the
repository that publishes its evaluation harness.
