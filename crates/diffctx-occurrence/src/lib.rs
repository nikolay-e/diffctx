//! The occurrence accounting of diffctx: every spelling of a definition's names in a repository's files,
//! each with its one outcome, and the token index of the head snapshot it reads. The proof of a
//! definition's `Complete` is made only here (`Accounted::settle` seals it) and so is `Outcome::NotOurs`;
//! the answer crate adds the gaps its own search found. The answer crate also supplies what the account
//! is read against (the names, the files it reaches, the files the run lost), and the seal vouches for
//! the account of those, not for them. Not a stable library API.

pub mod tokens;
