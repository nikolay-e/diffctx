//! The occurrence accounting of diffctx: every spelling of a definition's names in a repository's files,
//! each with its one outcome, and the token index of the head snapshot it reads.
//!
//! What stands in the way of an absence is made only here: `Outcome::NotOurs` cannot be constructed
//! outside this crate, and an `Account` is private to it. A definition's accounting comes out of the
//! run as `Accounted`, whose hedges are closed by `Accounted::settle` into one `Sealed` verdict per
//! relation of use; only a `Sealed` holds the `Proof` of a complete account. Not a stable library API.

pub mod tokens;
