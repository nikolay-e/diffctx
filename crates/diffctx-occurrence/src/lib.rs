//! The occurrence accounting of diffctx: every spelling of a definition's names in a repository's files,
//! each with its one outcome, and the token index of the head snapshot it reads. `Verdict::Complete` and
//! `Outcome::NotOurs` are made only here. Not a stable library API.

pub mod tokens;
