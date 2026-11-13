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

/// Where a bare name heading a chain (`name.x`) can be shown to be no member, as (family,
/// its locals reach the whole function and not a block, a class member needs a receiver).
pub(super) const BARE_HEADS: &[(Family, bool, bool)] = &[
    (Family::Python, true, true),
    (Family::TypeScript, false, true),
];
