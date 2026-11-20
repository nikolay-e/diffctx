//! The budgeted selection of diffctx: the utility, Boltzmann and importance objective of a fragment, the
//! greedy that fills a token budget with them and the repairs of a selection. Reads the graph, says
//! nothing about what an answer may claim. Not a stable library API.

pub mod postpass;
pub mod select;
pub mod utility;
