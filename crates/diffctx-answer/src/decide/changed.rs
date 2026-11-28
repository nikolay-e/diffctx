//! A changed definition as the decision sees it: how it changed, its span, its bases and its hunks.

use rustc_hash::FxHashMap;

use crate::fold::Basis;

use diffctx_facts::facts::{DefChange, DefKey, DefKind, Delta, Origin, SemanticDef};

use super::verdict::Subject;

/// The span a use must lie in to be the definition's own use of itself.
/// The labels of a configuration object: a definition of its own, under it.
pub(super) fn is_label_definition(key: &DefKey) -> bool {
    key.local.container.len() == 1 && matches!(key.local.name.as_str(), "labels" | "pod-labels")
}

pub(super) fn own_span(kind: DefKind, lines: (u32, u32)) -> Option<(u32, u32)> {
    (!matches!(
        kind,
        DefKind::Class
            | DefKind::Struct
            | DefKind::Enum
            | DefKind::Trait
            | DefKind::Interface
            | DefKind::Object
            | DefKind::Module
    ))
    .then_some(lines)
}

/// Whether a definition's own signature, body, visibility and decorators held:
/// what is left to differ is layout, its doc, and its members.
pub(super) fn own_tokens_held(d: &SemanticDef) -> bool {
    matches!(&d.change, DefChange::Changed { origin: Origin::Same, delta }
        if !(delta.signature || delta.body || delta.visibility || delta.decorators))
}

/// What a definition whose own tokens held did once its members are counted: its
/// whole token sequence is its own and its members', in order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Whole {
    /// Every token a caller sees is as it was: comments, formatting, position.
    Layout,
    /// Members were added, removed or edited, each with a change of its own
    /// that says so.
    Members,
    /// Its tokens changed and no member's change says how: its data members
    /// swapped places (the type's layout, a positional constructor), a member
    /// left or came with no change of its own, or it is not one definition.
    Unaccounted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum How {
    Edited {
        layout_only: bool,
    },
    Added,
    #[default]
    Removed,
}

/// A definition the range changed, as the answer reports it. A constructor is
/// reported as its class (its callers build the class), and a class-level field
/// also touches the class that holds it.
#[derive(Clone, Debug, Default)]
pub struct Changed {
    pub name: String,
    pub kind: DefKind,
    /// The head file; the base file of a removed definition.
    pub file: String,
    /// The head span; the base span of a removed definition.
    pub lines: (u32, u32),
    pub how: How,
    pub(super) subjects: Vec<Subject>,
    /// Base identities the head's callers may still name: the key a removed
    /// definition had, the one a moved or renamed one left.
    pub(super) stale: Vec<DefKey>,
    pub(super) unpaired: bool,
    /// Defined inside a function: named only there, so a file elsewhere that
    /// spells the name is no hedge on it (#546).
    pub local: bool,
    /// Part of the module's public surface: a removed one is a contract that
    /// moved.
    pub public: bool,
    /// What the range did to the definition, for the folding table; none when
    /// it was removed, renamed, moved, re-scoped, ambiguously paired or added.
    pub(super) basis: Option<Basis>,
    /// Never set aside as layout, whatever its hunks look like: a decorator may
    /// read what a doc or layout edit touched, and a type whose data members
    /// changed order has another layout and constructor.
    pub not_layout: bool,
    /// What the range did moves the definition's place in the module's surface: it
    /// was added, renamed or moved, or its signature (which carries its kind) or
    /// its visibility changed. A body, doc or decorator edit leaves it. In Kotlin
    /// a definition not public at the head has no place in the surface to move.
    pub api: bool,
    /// What differs between the two sides of an edited definition, from the facts.
    pub delta: Option<Delta>,
    /// `moved` or `renamed`, when the pairing says so.
    pub origin: Option<&'static str>,
}

impl Changed {
    /// The parameters a compatible signature change added, when it was one.
    pub fn new_parameters(&self) -> Vec<String> {
        match &self.basis {
            Some(Basis {
                compatible: Some(names),
                ..
            }) => names.clone(),
            _ => Vec::new(),
        }
    }
}

/// The bases in the parentheses of a class signature, as written, keywords left
/// out: `class A(B, m.C[int], metaclass=M)` lists `B` and `m.C[int]`.
pub(super) fn written_bases(signature: Option<&str>) -> Vec<&str> {
    let Some(inside) = signature.and_then(|text| {
        let (open, close) = text.find('(').zip(text.rfind(')'))?;
        text.get(open + 1..close)
    }) else {
        return Vec::new();
    };
    inside
        .split(',')
        .map(str::trim)
        .filter(|base| !base.contains('='))
        .collect()
}

/// The simple names of those bases: `B` and `C`.
pub(super) fn bases_in(signature: Option<&str>) -> Vec<String> {
    written_bases(signature)
        .into_iter()
        .filter_map(|base| {
            let last = base.rsplit('.').next()?;
            let word: String = last
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!word.is_empty()).then_some(word)
        })
        .collect()
}

impl Changed {
    /// The name the answer gives it: a member field is `Class.field`, so two
    /// classes' fields of one name are two definitions.
    /// The name the answer prints beside the file. A shell script is the definition
    /// of its own file: the file names it, and the path a second time would make it
    /// two names.
    pub fn symbol(&self) -> Option<String> {
        (!self.named_by_its_path()).then(|| self.display_name())
    }

    /// Whether the definition is the program of its file, named by the file's path
    /// (a shell script): a reader spells that path, never a word of it.
    pub fn named_by_its_path(&self) -> bool {
        self.kind == DefKind::Module && self.name == self.file
    }

    /// The names a renamed definition left: files outside the analysed code may
    /// still spell them.
    pub fn left_names(&self) -> impl Iterator<Item = &str> {
        self.stale
            .iter()
            .map(|key| key.local.name.as_str())
            .filter(|name| *name != self.name)
    }

    pub(super) fn key(&self) -> Option<&DefKey> {
        self.subjects
            .first()
            .map(|s| &s.key)
            .or_else(|| self.stale.first())
    }

    /// Whether `inner` is a definition nested in this one, in the same file.
    pub fn holds(&self, inner: &Self) -> bool {
        let (Some(outer_key), Some(inner_key)) = (self.key(), inner.key()) else {
            return false;
        };
        let path = &outer_key.local.container;
        self.file == inner.file
            && inner_key.local.container.starts_with(path)
            && inner_key.local.container.get(path.len()) == Some(&outer_key.local.name)
    }

    pub fn display_name(&self) -> String {
        match (self.kind, self.key().and_then(|k| k.local.container.last())) {
            (DefKind::Field | DefKind::Variable | DefKind::Constant, Some(class)) => {
                format!("{}.{}", class.as_str(), self.name)
            }
            _ => self.name.clone(),
        }
    }
}

pub(super) type Hunks = FxHashMap<String, Vec<(u32, u32)>>;
