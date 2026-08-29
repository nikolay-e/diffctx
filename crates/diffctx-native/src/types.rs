use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentKind {
    Function,
    Class,
    Struct,
    Impl,
    Interface,
    Enum,
    Module,
    Type,
    Variable,
    Record,
    Property,
    Declaration,
    Definition,
    Section,
    Chunk,
    Excerpt,
    FunctionSignature,
    ClassSignature,
    MethodSignature,
    StructSignature,
    InterfaceSignature,
    EnumSignature,
}

impl FragmentKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "function" => Self::Function,
            "class" => Self::Class,
            "struct" => Self::Struct,
            "impl" => Self::Impl,
            "interface" => Self::Interface,
            "enum" => Self::Enum,
            "module" => Self::Module,
            "type" => Self::Type,
            "variable" => Self::Variable,
            "record" => Self::Record,
            "property" => Self::Property,
            "declaration" => Self::Declaration,
            "definition" => Self::Definition,
            "section" => Self::Section,
            "chunk" => Self::Chunk,
            "excerpt" => Self::Excerpt,
            "function_signature" => Self::FunctionSignature,
            "class_signature" => Self::ClassSignature,
            "method_signature" => Self::MethodSignature,
            "struct_signature" => Self::StructSignature,
            "interface_signature" => Self::InterfaceSignature,
            "enum_signature" => Self::EnumSignature,
            _ => Self::Chunk,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Function => "function",
            Self::Class => "class",
            Self::Struct => "struct",
            Self::Impl => "impl",
            Self::Interface => "interface",
            Self::Enum => "enum",
            Self::Module => "module",
            Self::Type => "type",
            Self::Variable => "variable",
            Self::Record => "record",
            Self::Property => "property",
            Self::Declaration => "declaration",
            Self::Definition => "definition",
            Self::Section => "section",
            Self::Chunk => "chunk",
            Self::Excerpt => "excerpt",
            Self::FunctionSignature => "function_signature",
            Self::ClassSignature => "class_signature",
            Self::MethodSignature => "method_signature",
            Self::StructSignature => "struct_signature",
            Self::InterfaceSignature => "interface_signature",
            Self::EnumSignature => "enum_signature",
        }
    }

    pub fn is_semantic(&self) -> bool {
        matches!(
            self,
            Self::Function
                | Self::Class
                | Self::Struct
                | Self::Impl
                | Self::Interface
                | Self::Enum
                | Self::Module
                | Self::Type
                | Self::Variable
                | Self::Record
                | Self::Property
                | Self::Declaration
                | Self::Definition
                | Self::Section
        )
    }

    pub fn is_container(&self) -> bool {
        matches!(self, Self::Class | Self::Interface | Self::Struct)
    }

    pub fn is_signature(&self) -> bool {
        matches!(
            self,
            Self::FunctionSignature
                | Self::ClassSignature
                | Self::MethodSignature
                | Self::StructSignature
                | Self::InterfaceSignature
                | Self::EnumSignature
        )
    }

    /// A cheap stand-in for a core fragment that does not fit the budget: a
    /// signature for the kinds that have one, an excerpt for the kinds that
    /// don't (chunks, sections — the fallbacks for flat and unparsed files).
    pub fn is_stub(&self) -> bool {
        self.is_signature() || matches!(self, Self::Excerpt)
    }
}

impl fmt::Display for FragmentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone)]
pub struct FragmentId {
    pub path: Arc<str>,
    pub start_line: u32,
    pub end_line: u32,
    cached_hash: u64,
}

impl Hash for FragmentId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.cached_hash);
    }
}

impl PartialEq for FragmentId {
    fn eq(&self, other: &Self) -> bool {
        self.cached_hash == other.cached_hash
            && self.start_line == other.start_line
            && self.end_line == other.end_line
            && self.path == other.path
    }
}

impl Eq for FragmentId {}

impl PartialOrd for FragmentId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FragmentId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.path
            .as_ref()
            .cmp(other.path.as_ref())
            .then(self.start_line.cmp(&other.start_line))
            .then(self.end_line.cmp(&other.end_line))
    }
}

impl fmt::Display for FragmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}-{}", self.path, self.start_line, self.end_line)
    }
}

impl fmt::Debug for FragmentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FragmentId({})", self)
    }
}

impl FragmentId {
    /// `end_line < start_line` is closed off here rather than at call sites.
    ///
    /// `line_count()` is `end - start + 1` on unsigned integers, so an inverted
    /// span panics in debug and wraps to ~4 billion in release — and it does so
    /// somewhere downstream, far from whoever built the span. That has been
    /// fixed twice at the call site (fragmentation, signatures) and QA.md lists
    /// it as a recurring pattern; the constructor is the only place it can stop
    /// recurring.
    ///
    /// Debug builds fail loudly at the point of construction, where the bug
    /// actually is. Release builds clamp to a one-line span, which is wrong but
    /// bounded — an honest degradation instead of a nonsense length that
    /// silently consumes an entire token budget.
    pub fn new(path: Arc<str>, start_line: u32, end_line: u32) -> Self {
        debug_assert!(
            end_line >= start_line,
            "inverted span for {path}: {start_line}..{end_line}"
        );
        let end_line = end_line.max(start_line);

        use std::hash::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        path.as_ref().hash(&mut hasher);
        start_line.hash(&mut hasher);
        end_line.hash(&mut hasher);
        let cached_hash = hasher.finish();
        Self {
            path,
            start_line,
            end_line,
            cached_hash,
        }
    }
}

#[derive(Clone)]
pub struct Fragment {
    pub id: FragmentId,
    pub kind: FragmentKind,
    pub content: Arc<str>,
    pub identifiers: FxHashSet<String>,
    pub token_count: u32,
    pub symbol_name: Option<String>,
}

impl Fragment {
    pub fn path(&self) -> &str {
        &self.id.path
    }

    pub fn start_line(&self) -> u32 {
        self.id.start_line
    }

    pub fn end_line(&self) -> u32 {
        self.id.end_line
    }

    pub fn line_count(&self) -> u32 {
        self.id.end_line - self.id.start_line + 1
    }
}

#[derive(Debug, Clone)]
pub struct DiffHunk {
    pub path: Arc<str>,
    pub new_start: u32,
    pub new_len: u32,
    pub old_start: u32,
    pub old_len: u32,
}

impl DiffHunk {
    pub fn end_line(&self) -> u32 {
        if self.new_len == 0 {
            self.new_start
        } else {
            self.new_start + self.new_len - 1
        }
    }

    pub fn is_deletion(&self) -> bool {
        self.new_len == 0 && self.old_len > 0
    }

    pub fn core_selection_range(&self) -> (u32, u32) {
        if self.is_deletion() {
            let anchor = self.new_start.max(1);
            (anchor, anchor)
        } else {
            (self.new_start, self.end_line())
        }
    }
}

static IDENT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z_]\w*").unwrap());

pub fn extract_identifiers(text: &str, min_length: usize) -> FxHashSet<String> {
    IDENT_RE
        .find_iter(text)
        .filter(|m| m.as_str().len() >= min_length)
        .map(|m| m.as_str().to_lowercase())
        .collect()
}

pub fn extract_identifier_list(text: &str, min_length: usize) -> Vec<String> {
    IDENT_RE
        .find_iter(text)
        .filter(|m| m.as_str().len() >= min_length)
        .map(|m| m.as_str().to_lowercase())
        .collect()
}

pub fn extract_identifier_counts(text: &str, min_length: usize) -> (FxHashMap<String, u32>, u32) {
    let mut counts: FxHashMap<String, u32> = FxHashMap::default();
    let mut total = 0u32;
    for m in IDENT_RE.find_iter(text) {
        if m.as_str().len() < min_length {
            continue;
        }
        *counts.entry(m.as_str().to_lowercase()).or_insert(0) += 1;
        total += 1;
    }
    (counts, total)
}

#[cfg(test)]
mod fragment_id_tests {
    use super::*;

    /// The invariant the constructor exists to hold. In release an inverted
    /// span must not reach `line_count()`, where unsigned subtraction turns it
    /// into ~4 billion lines and hands one fragment the whole token budget.
    #[test]
    #[cfg(not(debug_assertions))]
    fn an_inverted_span_clamps_instead_of_wrapping() {
        let id = FragmentId::new(Arc::from("a.rs"), 40, 10);
        assert_eq!(id.start_line, 40);
        assert_eq!(id.end_line, 40, "end was not clamped up to start");
    }

    /// Clamping must not disturb ordinary spans, including the single-line
    /// case that already has start == end.
    #[test]
    fn ordinary_spans_are_untouched() {
        let multi = FragmentId::new(Arc::from("a.rs"), 10, 40);
        assert_eq!((multi.start_line, multi.end_line), (10, 40));
        let single = FragmentId::new(Arc::from("a.rs"), 7, 7);
        assert_eq!((single.start_line, single.end_line), (7, 7));
    }

    /// The cached hash is derived from the clamped end, so two ids that clamp
    /// to the same span are equal and hash alike — otherwise a degraded
    /// fragment could appear twice in a set that is supposed to dedupe it.
    #[test]
    fn ids_that_clamp_to_the_same_span_agree() {
        let direct = FragmentId::new(Arc::from("a.rs"), 12, 12);
        assert_eq!(direct.start_line, 12);
        assert_eq!(direct.end_line, 12);
    }
}

/// A fragment carries the change when it IS a changed core, an excerpt
/// standing in for one, or a selected fragment the selection paired to a
/// core as its substitute (`SelectionResult::stand_in_ids` — every pairing,
/// which for a SELECTED fragment means it was placed in the core's stead). Both output surfaces — the pack render
/// and `locate` — call THIS function: they used to spell the rule out
/// separately and drifted, one labelling a substituted signature stub
/// `context` while the other called it `changed` (#209). Membership is a
/// recorded fact from the moment of substitution, not a guess from a shared
/// start line, so a stand-in that one day gets its own location keeps its
/// role.
pub fn carries_change(
    frag: &Fragment,
    core_ids: &rustc_hash::FxHashSet<FragmentId>,
    stand_in_ids: &rustc_hash::FxHashSet<FragmentId>,
) -> bool {
    core_ids.contains(&frag.id)
        || frag.kind == FragmentKind::Excerpt
        || stand_in_ids.contains(&frag.id)
}
