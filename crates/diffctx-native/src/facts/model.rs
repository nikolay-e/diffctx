//! What an extractor says about one blob, and the file it is bound to. Facts never
//! resolve a binding: that is the resolve layer's job (wave 4).

use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::facts::config::{ConfigRef, ConfigUse, Labels, Release, Selector, SourcePointer};
use crate::facts::ids::{ModulePath, Name, ProjectId, Span, TypeName};
use crate::facts::language::Language;
use crate::facts::shell::ShellPath;
use crate::snapshot::{Loss, RepoPath, Stage};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Namespace {
    Value,
    Type,
    Macro,
}

/// What tells two definitions of one name apart where a language allows both:
/// overloads, `impl` blocks, `cfg` twins. Never the signature text, which a
/// changed signature would turn into a removal and an addition.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Disambig {
    /// The n-th overload in source order.
    Overload(u16),
    ImplOf(TypeName),
    Cfg(String),
}

/// A definition's identity inside its module: no line numbers, no byte offsets.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct LocalKey {
    /// The enclosing definitions, outermost first.
    pub container: Vec<Name>,
    pub name: Name,
    pub ns: Namespace,
    pub disambig: Option<Disambig>,
}

impl LocalKey {
    pub fn new(container: Vec<Name>, name: impl Into<Name>, ns: Namespace) -> Self {
        Self {
            container,
            name: name.into(),
            ns,
            disambig: None,
        }
    }

    /// A top-level value.
    pub fn value(name: impl Into<Name>) -> Self {
        Self::new(Vec::new(), name, Namespace::Value)
    }

    #[must_use]
    pub fn with_disambig(mut self, disambig: Disambig) -> Self {
        self.disambig = Some(disambig);
        self
    }
}

/// Stable across snapshots, and across moves of a file inside one module. The
/// language is part of it: `app.py` and `app.ts` are both module `app`, and a
/// definition of one is never the other's.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct DefKey {
    pub project: ProjectId,
    pub language: Language,
    pub module: ModulePath,
    pub local: LocalKey,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum DefKind {
    Function,
    Method,
    Constructor,
    Property,
    Class,
    Struct,
    Enum,
    EnumMember,
    Trait,
    Interface,
    Object,
    TypeAlias,
    Variable,
    Constant,
    Field,
    Macro,
    Module,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Package,
    Crate,
    Protected,
    Private,
}

/// The hashes are of the definition's own text: members that are definitions
/// themselves (a class's methods) are left out of the container's hashes, so a
/// change to a method is the method's change alone.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Def {
    pub local: LocalKey,
    pub kind: DefKind,
    pub span: Span,
    pub name_span: Span,
    /// The declaration as the reader meets it, normalised by the extractor so
    /// that equal declarations compare equal.
    pub signature: Option<String>,
    pub decorators: Vec<Name>,
    pub visibility: Visibility,
    /// The body's syntax tree without whitespace and comments: node kinds,
    /// nesting and leaf text, so a re-indent that moves a statement between
    /// blocks changes it (#442). `0`: no body, never paired by it.
    pub body_hash: u64,
    /// The body's bytes.
    pub text_hash: u64,
    /// The docstring or doc comment, as tokens: semantics, not layout (#637).
    pub doc_hash: Option<u64>,
}

/// Who owns a site.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Scope {
    Def(LocalKey),
    /// A closure that is an argument of a call (`test("...", () => ...)`,
    /// `app.get(..., fn)`), wrapped around the nearest named definition.
    Callback {
        callee: Vec<Name>,
        site: Span,
        parent: Box<Scope>,
    },
    /// The module's own top-level code (#497, #449).
    Module,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Receiver {
    None,
    SelfType,
    Module(ModulePath),
    Typed(TypeName),
    New(TypeName),
    CallResult(Vec<Name>),
    Unknown,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum UseKind {
    Call,
    Read,
    Write,
    TypeUse,
    PassAsValue,
    Instantiate,
    Override,
    Decorate,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Ref {
    pub site: Span,
    pub enclosing: Scope,
    pub receiver: Receiver,
    pub use_kind: UseKind,
    pub name: Name,
    /// The written qualifiers before the name, outermost first (`a.b` in `a.b.name`).
    pub qualifier: Vec<Name>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ImportKind {
    Static,
    TypeOnly,
    Dynamic,
    Require,
    SideEffect,
    /// `use function a\f;`: the name enters the function namespace only.
    FunctionOnly,
    /// `use const a\C;`: the name enters the constant namespace only.
    ConstOnly,
}

/// What an import takes of a whole module, each state a variant of its own: a
/// nested `Option` writes `Some(None)` as `null` in JSON, which reads back as
/// `None`, so a glob import came out of the facts cache as no import at all.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Star {
    /// No star: the names, if any, are listed.
    #[default]
    None,
    /// `import *`, `use m::*`, `import a.b.*`.
    Glob,
    /// `import * as alias`: the module as one local name.
    Alias(Name),
}

impl Star {
    /// Whether the import takes the whole module, with a name or without.
    pub const fn is_star(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// The local name the module is bound to, for `import * as alias`.
    pub const fn alias(&self) -> Option<&Name> {
        match self {
            Self::Alias(name) => Some(name),
            Self::None | Self::Glob => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Import {
    pub site: Span,
    pub enclosing: Scope,
    pub module: ModulePath,
    /// Leading dots of a relative import; `0` for an absolute one.
    pub relative: u32,
    pub kind: ImportKind,
    /// `(name, alias)` of each imported name.
    pub names: Vec<(Name, Option<Name>)>,
    pub star: Star,
    /// The default import's local name.
    pub default: Option<Name>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Export {
    Named {
        site: Span,
        name: Name,
        alias: Option<Name>,
        from: Option<ModulePath>,
        type_only: bool,
    },
    Star {
        site: Span,
        from: ModulePath,
        alias: Option<Name>,
    },
    Default {
        site: Span,
        local: Option<Name>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum EntryKind {
    /// A program's `main`.
    Main,
    /// A module's run-as-script guard (`if __name__ == "__main__"`).
    ScriptGuard,
    Test,
    /// A command-line command or subcommand.
    Command,
    /// A served route or handler registered with a framework.
    Route,
    /// A job a framework's scheduler starts (`@Scheduled`, `@scheduled_job`).
    Scheduled,
    /// A tool an agent protocol server exposes (`@mcp.tool()`).
    Tool,
    /// A pytest fixture, injected into a test by the name of its parameter.
    Fixture,
}

/// Where execution can begin from outside the code.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct EntryPoint {
    pub site: Span,
    pub kind: EntryKind,
    /// The definition that is the entry; `Module` for a guard's top-level code.
    pub target: Scope,
    /// The framework that enters it, when one does: a language's own entry
    /// (`main`, a test attribute, a script guard) has none.
    pub framework: Option<Framework>,
}

/// What a framework's rule says about an entry it makes: who calls the
/// definition, and under which key. `label` is read by `kind`: a route's path, a
/// scheduled job's expression, a command's, tool's or fixture's name; `None`
/// where the source does not state it (an annotation read by name alone).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Framework {
    pub name: Name,
    /// A route's HTTP method, upper case; `None` for another kind, and for a
    /// route registered for any method.
    pub verb: Option<Name>,
    pub label: Option<String>,
}

/// An import whose module the code computes: `importlib.import_module(name)`,
/// `__import__(name)`, `import(expr)` or `require(expr)` with an argument that is
/// no literal. What it loads is not named in the text, so any module may be its
/// target: an absence of importers is a claim only where no file holds one.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ComputedImport {
    pub site: Span,
    pub enclosing: Scope,
    /// `Dynamic` for `import(...)`, `import_module` and `__import__`; `Require`
    /// for `require(...)`.
    pub kind: ImportKind,
}

/// A repository path written in code (`sys.path.insert`, a `join` of literals).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PathMention {
    pub site: Span,
    pub enclosing: Scope,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum ConfigSource {
    Env,
    Setting,
}

/// A configuration key the code reads: an environment variable, a setting.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfigKey {
    pub site: Span,
    pub enclosing: Scope,
    pub key: String,
    pub source: ConfigSource,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum LocalKind {
    Parameter,
    Variable,
    Closure,
}

/// A parameter, a local or a closure: never a definition (#452, #548, #589).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Local {
    pub name: Name,
    pub site: Span,
    pub enclosing: Scope,
    pub kind: LocalKind,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Fact {
    Definition(Def),
    Reference(Ref),
    Import(Import),
    Export(Export),
    EntryPoint(EntryPoint),
    PathMention(PathMention),
    ConfigKey(ConfigKey),
    LocalBinding(Local),
    ComputedImport(ComputedImport),
    /// A field of a configuration object that names another object.
    ConfigRef(ConfigRef),
    /// A label selector of a configuration object.
    ConfigSelector(Selector),
    /// The pairs of a label definition of a configuration object.
    ConfigLabels(Labels),
    /// A place a configuration file names, of the repository or outside it.
    ConfigPointer(SourcePointer),
    /// A value or a named template a Helm template's action uses.
    ConfigUse(ConfigUse),
    /// A release of a chart, with the values files it gives it.
    ConfigRelease(Release),
    /// A file a shell script sources or a script it runs, by the path it spells.
    ShellPath(ShellPath),
}

impl Fact {
    pub fn site(&self) -> &Span {
        match self {
            Self::Definition(def) => &def.span,
            Self::Reference(r) => &r.site,
            Self::Import(import) => &import.site,
            Self::Export(
                Export::Named { site, .. }
                | Export::Star { site, .. }
                | Export::Default { site, .. },
            ) => site,
            Self::EntryPoint(entry) => &entry.site,
            Self::PathMention(mention) => &mention.site,
            Self::ConfigKey(key) => &key.site,
            Self::LocalBinding(local) => &local.site,
            Self::ComputedImport(computed) => &computed.site,
            Self::ConfigRef(reference) => &reference.site,
            Self::ConfigSelector(selector) => &selector.site,
            Self::ConfigLabels(labels) => &labels.site,
            Self::ConfigPointer(pointer) => &pointer.site,
            Self::ConfigUse(used) => &used.site,
            Self::ConfigRelease(release) => &release.site,
            Self::ShellPath(path) => &path.site,
        }
    }

    /// Orders facts that start at the same byte.
    fn rank(&self) -> u8 {
        match self {
            Self::Definition(_) => 0,
            Self::Import(_) => 1,
            Self::Export(_) => 2,
            Self::EntryPoint(_) => 3,
            Self::LocalBinding(_) => 4,
            Self::Reference(_) => 5,
            Self::PathMention(_) => 6,
            Self::ConfigKey(_) => 7,
            Self::ComputedImport(_) => 8,
            Self::ConfigRef(_) => 9,
            Self::ConfigSelector(_) => 10,
            Self::ConfigLabels(_) => 11,
            Self::ConfigPointer(_) => 12,
            Self::ConfigUse(_) => 13,
            Self::ConfigRelease(_) => 14,
            Self::ShellPath(_) => 15,
        }
    }
}

/// What a stretch of a blob's bytes is, where it is not code. A string literal
/// with code inside it (an f-string's `{x}`, a template's `${x}`) is recorded as
/// the pieces between the code, never whole.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Lexical {
    Comment,
    String,
}

/// The file a blob loss is bound to when it is read, in place until then: a blob
/// names no path, so a loss an extractor reports about it carries this one.
fn unbound() -> RepoPath {
    RepoPath::new("<blob>").expect("a constant path is valid")
}

/// What one blob says, whatever path or snapshot holds it. It is what the cache
/// keeps, so it names no path, project or module; its losses carry a placeholder
/// path that [`FileFacts::losses`] replaces with the file's.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlobFacts {
    pub language: Language,
    pub facts: Vec<Fact>,
    pub losses: Vec<Loss>,
    /// The byte ranges of the comments and of the string literals, sorted by
    /// start and not overlapping. Empty where the extractor records none: every
    /// byte is then code.
    pub lexical: Vec<(Range<u32>, Lexical)>,
}

impl BlobFacts {
    pub fn new(language: Language) -> Self {
        Self {
            language,
            facts: Vec::new(),
            losses: Vec::new(),
            lexical: Vec::new(),
        }
    }

    /// The grammar met errors; `lines` are the 1-based line ranges of the error
    /// nodes. The facts around them are still the extractor's to give.
    pub fn parse_error(&mut self, lines: Vec<(u32, u32)>) {
        self.losses.push(Loss::ParseError {
            path: unbound(),
            lines,
        });
    }

    /// The budget ran out; everything after this point of the walk was not read.
    pub fn deadline(&mut self) {
        self.losses.push(Loss::Deadline {
            stage: Stage::Extraction,
        });
    }

    /// A caught panic of the extractor.
    pub(crate) fn failed(language: Language) -> Self {
        Self {
            language,
            facts: Vec::new(),
            losses: vec![Loss::ExtractorFailed { path: unbound() }],
            lexical: Vec::new(),
        }
    }

    /// The lines of the blob's parse errors, as the cache keeps them. `None` for a
    /// blob whose losses are anything else (a run cut short, a panic): it is not
    /// the file's whole answer and is never kept.
    pub(crate) fn parse_error_lines(&self) -> Option<Vec<&[(u32, u32)]>> {
        self.losses
            .iter()
            .map(|loss| match loss {
                Loss::ParseError { lines, .. } => Some(lines.as_slice()),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn from_stored(
        language: Language,
        facts: Vec<Fact>,
        lexical: Vec<(Range<u32>, Lexical)>,
        errors: Vec<Vec<(u32, u32)>>,
    ) -> Self {
        let mut blob = Self {
            language,
            facts,
            losses: Vec::new(),
            lexical,
        };
        for lines in errors {
            blob.parse_error(lines);
        }
        blob
    }

    /// Whether the facts are the whole file's: a run cut short by the budget or
    /// a panic is not, and is never kept.
    pub fn is_complete(&self) -> bool {
        !self
            .losses
            .iter()
            .any(|loss| matches!(loss, Loss::Deadline { .. } | Loss::ExtractorFailed { .. }))
    }

    /// Source order (site start, then kind), so two runs give the same bytes.
    pub fn in_source_order(&mut self) {
        self.facts
            .sort_by_key(|fact| (fact.site().bytes.start, fact.rank()));
        self.lexical.sort_by_key(|(range, _)| range.start);
    }

    /// What the byte at `at` is when it lies in a comment or a string literal.
    pub fn lexical_at(&self, at: u32) -> Option<Lexical> {
        let next = self.lexical.partition_point(|(range, _)| range.end <= at);
        self.lexical
            .get(next)
            .filter(|(range, _)| range.start <= at)
            .map(|(_, kind)| *kind)
    }

    pub fn defs(&self) -> impl Iterator<Item = &Def> {
        self.facts.iter().filter_map(|fact| match fact {
            Fact::Definition(def) => Some(def),
            _ => None,
        })
    }
}

/// A blob read at a path: the path, and the project and module the workspace
/// puts it in.
#[derive(Clone, Debug)]
pub struct FileFacts {
    pub path: RepoPath,
    pub project: ProjectId,
    pub module: ModulePath,
    pub blob: Arc<BlobFacts>,
    /// The blob's parse errors are ones a newer release of the grammar does not
    /// have (`facts::grammar`): they are named so, not as a malformed file.
    pub outdated_grammar: bool,
}

impl FileFacts {
    pub fn key(&self, local: &LocalKey) -> DefKey {
        DefKey {
            project: self.project.clone(),
            language: self.blob.language,
            module: self.module.clone(),
            local: local.clone(),
        }
    }

    pub fn defs(&self) -> impl Iterator<Item = &Def> {
        self.blob.defs()
    }

    pub fn language(&self) -> Language {
        self.blob.language
    }

    /// The blob's losses, each about this file.
    pub fn losses(&self) -> Vec<Loss> {
        self.blob
            .losses
            .iter()
            .cloned()
            .map(|loss| match loss {
                Loss::ParseError { path, lines } if self.outdated_grammar => {
                    Loss::GrammarOutdated { path, lines }
                }
                other => other,
            })
            .map(|loss| loss.at(&self.path))
            .collect()
    }
}
