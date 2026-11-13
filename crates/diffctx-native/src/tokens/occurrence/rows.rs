//! What the outcome of an occurrence reads per language: rows of data, never code.

use crate::decide::Family;
use crate::facts::LocalKind;

/// The locals whose site is only the declaration of the name: no pattern shares it
/// with a field read (`let Retry { attempt } = r;`, `const { attempt } = r;` is why
/// TypeScript's `Variable` is not here). Java and Kotlin sites span the scope, not the name.
pub(super) const DECLARING_LOCALS: &[(Family, LocalKind)] = &[
    (Family::Python, LocalKind::Variable),
    (Family::Python, LocalKind::Closure),
    (Family::TypeScript, LocalKind::Closure),
];
