//! What a use of a changed definition is, as the rows of an answer state it
//! (`Finding`, `Evidence`), the languages the import readers know (`Lang`), and the two
//! regex readers of imports the selection graph and the overlay still take: Python's
//! (`python_bindings`) and the definitions a JavaScript file or page declares
//! (`js_facts`). The binding model that resolved a use through them is gone: the cells
//! (`decide.rs`) resolve, and a language no cell reads is answered by the text tier.
//! Swift, SQL and the scripts of a page are cells now, not the text tier.

use std::path::Path;

use rustc_hash::FxHashMap;

mod js;
mod python;

pub(crate) use js::js_facts;
pub use python::python_bindings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Python,
    Js,
}

pub fn lang_of(path: &str) -> Option<Lang> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)?;
    match ext.as_str() {
        "py" | "pyi" => Some(Lang::Python),
        // A page's inline scripts are JavaScript (#420).
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "html" | "htm" => {
            Some(Lang::Js)
        }
        _ => None,
    }
}

/// What a local name is bound to by an import: a whole module, or one name
/// a module exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Module(String),
    Member { module: String, name: String },
}

#[derive(Clone, Debug, Default)]
pub struct FileBindings {
    pub names: FxHashMap<String, Target>,
    pub star_from: Vec<String>,
}

/// The owner of a property set on the global object (`self.x = …`): any
/// script the realm loads reaches it by name, bare or through the global
/// object, with no import to bind it (#426).
pub(crate) const GLOBAL_OWNER: &str = "<global>";

/// A read of a member through a receiver nothing types: not listed as a
/// caller, and not nothing either (#399).
pub(crate) const UNRESOLVED_READ: &str = "unresolved_read";

const GLOBAL_OBJECTS: &[&str] = &["self", "globalThis", "window", "global"];

/// How a reference relates to the changed definition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Evidence {
    /// Available binding evidence points elsewhere.
    Rejected,
    /// The reference may reach the definition; its binding or receiver is
    /// not known. The reason says which.
    Candidate(&'static str),
    /// A binding links the reference to the definition.
    Resolved,
}

impl Evidence {
    fn rank(self) -> u8 {
        match self {
            Self::Rejected => 0,
            // A call through the same receiver says more than a read of it.
            Self::Candidate(UNRESOLVED_READ) => 1,
            Self::Candidate(_) => 2,
            Self::Resolved => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Call,
    Reference,
    /// A class that implements the changed abstract member.
    Implementation,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub evidence: Evidence,
    pub relation: Relation,
    /// What the use does, where the resolver says: a use the cell found. A
    /// model's finding says only `relation`.
    pub kind: Option<crate::lang::RelationKind>,
    /// The rule of the folding table that says the change cannot have affected
    /// the use, the relation kind it is counted under, and how many definitions
    /// share the name a candidate may be.
    pub fold: Option<(&'static str, &'static str, usize)>,
    pub lines: Vec<u32>,
}

pub(crate) fn is_import_line(code: &str) -> bool {
    let head = code.trim_start();
    head.starts_with("import ")
        || head.starts_with("from ")
        || (head.starts_with("export ") && head.contains(" from "))
}

/// Folds one occurrence into the finding: stronger evidence replaces the
/// lines, equal evidence adds its line, a call outranks a reference.
pub(crate) fn absorb(
    best: &mut Option<Finding>,
    evidence: Evidence,
    relation: Relation,
    line: u32,
) {
    let Some(f) = best else {
        *best = Some(Finding {
            evidence,
            relation,
            kind: None,
            fold: None,
            lines: vec![line],
        });
        return;
    };
    if evidence.rank() > f.evidence.rank() {
        *f = Finding {
            evidence,
            relation,
            kind: None,
            fold: None,
            lines: vec![line],
        };
        return;
    }
    if evidence.rank() < f.evidence.rank() {
        return;
    }
    if relation == Relation::Call {
        f.relation = Relation::Call;
    }
    if !f.lines.contains(&line) {
        f.lines.push(line);
    }
}
