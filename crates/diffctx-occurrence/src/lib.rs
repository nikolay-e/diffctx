//! The occurrence accounting of diffctx: every spelling of a definition's names in a repository's files,
//! each with its one outcome, and the token index of the head snapshot it reads. A definition's
//! The proof of a definition's `Complete` is made only here (`Accounted::settle` seals it) and so is `Outcome::NotOurs`; the answer crate adds the gaps its own search found. Not a stable library API.

pub mod tokens;
