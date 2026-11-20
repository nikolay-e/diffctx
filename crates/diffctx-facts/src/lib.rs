//! The only tree-sitter user of diffctx: a file's text into facts per language (definitions, imports,
//! references), the facts cache keyed by blob, the grammar registry and parse strategies, the definitions
//! laid over fragments and the change classes. Reads a repository only through diffctx-snapshot. Not a
//! stable library API.

pub mod change_class;
pub mod facts;
pub mod overlay;
pub mod parsers;
