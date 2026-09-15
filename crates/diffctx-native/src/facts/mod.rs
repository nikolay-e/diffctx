//! Typed facts a language's parse tree states about a file — its package,
//! what it imports, what it extends — for the edge layer to consume without
//! re-reading the text with regular expressions.
//!
//! The fragmenter parses every file and drops the tree; the edge builders
//! then regex-scanned the same text, and each language's reader grew patch
//! by patch (Scala was on its fifth, #243). A fact walked off the tree
//! carries what the grammar knows: a brace group spanning lines, a rename,
//! an import inside a comment that is not an import at all.

pub mod scala;

use rustc_hash::FxHashSet;

/// One import clause: `prefix.{selectors}` plus whether it ends in a wildcard.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportFact {
    pub prefix: String,
    pub selectors: Vec<String>,
    pub wildcard: bool,
}

#[derive(Debug, Clone, Default)]
pub struct LanguageFacts {
    pub package: Option<String>,
    pub imports: Vec<ImportFact>,
    pub inherits: FxHashSet<String>,
}
