//! The occurrence accounting of diffctx: every spelling of a definition's names in a repository's files,
//! each with its one outcome, and the token index of the head snapshot it reads. A definition's
//! `Complete` is made only here (`Accounted::settle` seals it) and so is `Outcome::NotOurs`. Not a stable library API.

pub mod tokens;
