//! Typed facts a language's parse tree states about a file — its package,
//! what it imports, what it extends — for the edge layer to consume without
//! re-reading the text with regular expressions.
//!
//! The fragmenter parses every file and drops the tree; the edge builders
//! then regex-scanned the same text, and each language's reader grew patch
//! by patch (Scala was on its fifth, #243). A fact walked off the tree
//! carries what the grammar knows: a brace group spanning lines, a rename,
//! an import inside a comment that is not an import at all.

pub mod java;
pub mod kotlin;
pub mod python;
pub mod scala;
pub mod swift;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Function,
    Class,
    /// A name an assignment binds or a body states as a member — `TABLE =
    /// …`, `self.x = …`, a field, an interface member: a declaration the
    /// fragmenter does not cut on its own.
    Assignment,
}

#[derive(Debug, Clone)]
pub struct Declaration {
    pub name: String,
    pub kind: DeclKind,
    /// Decorators included.
    pub lines: (u32, u32),
    /// The class whose body declares it: a method, a field.
    pub class: Option<String>,
    /// The function it is local to: only that function's lines reach it.
    pub scope: Option<(u32, u32)>,
}

/// The declaration named `name` a fragment spanning `lines` stands for: the
/// first one opening inside it, else the innermost one around it (a chunk
/// of a long function).
pub fn declared<'d>(
    declarations: &'d [Declaration],
    name: &str,
    lines: (u32, u32),
) -> Option<&'d Declaration> {
    let named = || declarations.iter().filter(|d| d.name == name);
    named()
        .filter(|d| lines.0 <= d.lines.0 && d.lines.0 <= lines.1)
        .min_by_key(|d| d.lines.0)
        .or_else(|| {
            named()
                .filter(|d| d.lines.0 <= lines.0 && d.lines.1 >= lines.1)
                .max_by_key(|d| d.lines.0)
        })
}
