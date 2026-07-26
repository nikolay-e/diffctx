# Regression suites

Regression identifiers are qualified by suite:

- `enrichment/regression_NNN_*` covers dependency and context enrichment.
- `selection/regression_NNN_*` covers changed-file retention, focus, and noise
  exclusion.

The suite path is part of the stable case identity, so independently evolved
number sequences remain unambiguous in logs and external references.
