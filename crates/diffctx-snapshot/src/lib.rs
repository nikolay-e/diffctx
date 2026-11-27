//! The one reader of a repository state: git, the working tree, admission and what a run could not read,
//! with the run's deadline and caps, the diff text and the files a run may show. Not a stable library API.

pub mod candidate_files;
pub mod diff_parse;
pub mod grep;
pub mod resource;
pub mod snapshot;
pub mod source;
