//! What an impact answer may claim about the uses of a definition of a language
//! the resolvers read: Python, and TypeScript with JavaScript.
//!
//! The resolver says what each reference lands on; this module turns that into
//! the statements the answer prints. Every statement about a definition's
//! callers is one of five claims: `resolved` (a static chain leads to it),
//! `candidate` (the receiver is not typed, so the definition is one it may be),
//! `unresolved` (names like it that landed on nothing), `partial` (the answer
//! is incomplete) or `absent` (nothing calls it, and the search was complete).
//! An absence, "nothing calls it" or "untested", is printed only from that last
//! one, and a caller that is itself changed in the range is `also_changed`,
//! never a caller outside the diff.
//!
//! The changed definitions are the facts layer's `semantic_changes`; the callers
//! are the resolver's bindings. The ranking graph and the text filters take no
//! part.
//!
//! An absence is never a default. [`Complete`] is the witness that the search for a
//! definition's uses left nothing unread, and it cannot be copied out of the
//! [`Coverage`] that holds it. [`Coverage::close`] makes it for a definition, from
//! the gaps the search found and the sealed verdict of the occurrence accounting
//! ([`diffctx_occurrence::tokens::occurrence::Sealed`]), which only that crate makes; this
//! crate's `Coverage::of_run` makes it for what is no definition's.
//! An absence or an exact count is printed only from a coverage that holds the
//! witness. Every other outcome names its [`Gap`]. A failed read is an `Err`
//! or a gap here, never an empty value: the lint that says so is denied in this
//! module.

#![deny(clippy::disallowed_methods)]

pub(crate) mod cell;
pub(crate) mod cells;
pub mod changed;
pub mod coverage;
pub mod gap;
pub(crate) mod losses;
#[cfg(test)]
mod tests;
pub(crate) mod verdict;
