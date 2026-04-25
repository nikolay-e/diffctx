use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashSet;

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
    pub fn new(path: Arc<str>, start_line: u32, end_line: u32) -> Self {
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

    pub fn path_buf(&self) -> PathBuf {
        PathBuf::from(self.path.as_ref())
    }
}

#[derive(Clone)]
pub struct Fragment {
    pub id: FragmentId,
    pub kind: FragmentKind,
    pub content: String,
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
