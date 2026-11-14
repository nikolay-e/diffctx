//! A path a shell script spells, for the resolver to follow: the file it sources
//! or the script it runs.
//!
//! A script says where a file is relative to something, and the something is
//! what decides whether the path can be followed: its own directory
//! (`$(dirname "$0")`), nowhere in particular (`./lib.sh`), or a value nothing in
//! the script settles (`$LIB_DIR/lib.sh`). The head says which, and the tail is
//! the literal rest.

use serde::{Deserialize, Serialize};

use super::{Scope, Span};

/// What a path spelled in a script starts from.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum PathHead {
    /// Nothing: the path is literal, relative to wherever the script runs.
    Literal,
    /// The script's own directory: `$(dirname "$0")`, `${BASH_SOURCE%/*}`, or a
    /// variable every assignment of which is one of those.
    ScriptDir,
    /// A value nothing here reads: `$LIB_DIR`, `$(brew --prefix)`.
    Unknown,
}

/// What the script does with the path.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum PathUse {
    /// `source path` or `. path`: the file's functions and variables become the
    /// script's own.
    Source,
    /// `./path`, `bash path`: the script runs in a shell of its own.
    Run,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ShellPath {
    pub site: Span,
    /// The function the command is in, else the module.
    pub enclosing: Scope,
    pub head: PathHead,
    pub tail: String,
    pub how: PathUse,
}
