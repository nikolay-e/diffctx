//! A failed read is a named limit or an `Err` in this module, never an empty
//! value: the lint that says so is denied here and in its submodules. A lookup
//! that finds nothing is spelled as the absence it is.

#![deny(clippy::disallowed_methods)]

use crate::decide::cells::Cells;
use crate::run_provenance::state::ScoredState;
use diffctx_base::types::{Fragment, FragmentId, FragmentKind};
use diffctx_facts::parsers::embedded::names_word;

pub mod build;
pub(crate) mod callers;
pub(crate) mod cells;
pub(crate) mod changes;
pub(crate) mod coverage;
pub mod deliver;
pub(crate) mod dispatch;
pub(crate) mod entry;
pub(crate) mod key_search;
mod keys;
mod mentions;
pub mod output;
mod php;
pub(crate) mod removed;
mod runs;
mod rust;
pub mod symbol;
mod tests_reach;
mod text;
pub mod unanalysed;
pub use mentions::Class as MentionClass;
pub use text::render_markdown;

/// The whole serialized answer, not its fragment tokens: a hook injects this
/// into a context the reader did not ask to spend, and locate taught that a
/// budget on the parts leaves the envelope unbounded (#300).
pub const IMPACT_TOKEN_CAP: u32 = 2_000;

/// The line log is one git call per changed symbol; past this many the range
/// is a rewrite, and commit overlap on it says nothing a reader would act on.
const MAX_BLAMED_SYMBOLS: usize = 50;

/// Call sites named per file in the text form before the rest fold into a
/// count.
const MAX_CALLERS_SHOWN_PER_FILE: usize = 3;

/// Contracts are a list of facts the reader can grep for; past this many the
/// list is the diff's own `pub` lines read back.
const MAX_CONTRACTS: usize = 12;
/// A declaration longer than this many lines before its body opens is
/// compared on its first lines only.
const MAX_SIGNATURE_LINES: usize = 12;
/// Paths listed before the rest fold into a count: the reader has the diff.
const MAX_CHANGED_FILES_LISTED: usize = 24;
/// Lines the removed names are read from per file: one caller per function is
/// the answer, and a flood of one common word is not. A file past it is said.
const MAX_REMOVED_HITS_PER_FILE: usize = 20;
/// Past this many lines a removed name is a common word: unresolved, not
/// traced.
pub(crate) const MAX_REMOVED_NAME_HITS: usize = 500;
/// Removed names whose callers are searched for. A module of tens of thousands
/// of definitions is a generated file: its first names are traced and the rest
/// are named as not (fj#211).
const MAX_REMOVED_NAMES_TRACED: usize = 2000;
/// Unanalysed files kept when the answer must shrink; the rest are counted.
const MAX_UNANALYSED_LISTED: usize = 6;
/// Unanalysed paths the text names per language; the reader holds the diff.
pub(crate) const UNANALYSED_EXAMPLES: usize = 2;

/// A symbol name with more definitions than this cannot be traced by name.
const MAX_DEFINITIONS: usize = 2;

/// Whether the first fragment references the second's definition: through
/// a resolved binding in the languages that have a binding model, through
/// the lexical model elsewhere.
type Link<'l> = dyn Fn(&Fragment, &Fragment) -> bool + 'l;

/// How far up the call graph a test may sit and still count as reaching a
/// caller: the test, a function it calls, and one more.
pub(crate) const MAX_TEST_HOPS: usize = 2;

/// JSON Schema 2020-12 for `diffctx.impact.v2`, pinned by `tests/context_schema.rs`.
pub use crate::answer::v2::impact_v2_schema as impact_schema;
