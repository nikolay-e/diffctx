//! PHP: every reference and import of the facts layer bound to the definitions
//! it lands on.
//!
//! The facts state names and the chain written before them (see
//! `facts::lang::php`); this layer walks the chain through what PHP itself
//! resolves a name by: the namespace the code is in, the file's `use` aliases (a
//! class, a function and a constant each in its own table), `self`, `static` and
//! `parent`, a receiver's declared type (a parameter, a property, what a method
//! or a function returns, what `new` builds), the members of a type through its
//! parents, interfaces and traits.
//!
//! `composer.json` is read as data: its PSR-4 and PSR-0 maps place a class name
//! in a file whose own `namespace` line does not, tell a name under the
//! repository's namespaces (a class that should be here and is not found: an
//! unknown) from a package's (outside), and pick the autoloaded one of two
//! classes of one name; `classmap` and `files` entries count as autoloaded.
//!
//! What the syntax cannot settle stays a candidate with its reason: a method
//! called through a receiver nothing types, a call through a supertype that a
//! subtype overrides. A dynamic name (`$o->$m()`, `new $c`, `$f()`,
//! `call_user_func`), a call a `__call` or `__callStatic` catches and a trait used
//! with `insteadof` or `as` are unresolved with an unknown reason: nothing is
//! claimed, and the dynamic ones are named `*`, which no definition is, so the
//! cell reads them for every definition they could reach.
//!
//! Two projects are linked only through a declared dependency
//! (`WorkspaceModel::may_reference`); a `private` member stops at its file.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution,
    UnresolvedReason,
};
use crate::facts::lang::php::PROPERTIES;
use crate::facts::{
    Def, DefKey, DefKind, Fact, FactsCache, FileFacts, ImportKind, Language, Name, Receiver, Ref,
    Scope, Span, UseKind, Visibility, expired, facts_of,
};
use crate::snapshot::{
    AnalysisContext, Bytes, Loss, RepoPath, Result, SideTag, Stage, WorkspaceModel,
};

/// A call through an untyped receiver is a candidate for at most this many
/// members; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// Supertype hops followed to find an inherited member, and links of a chain of
/// values typed by one another.
const MAX_DEPTH: usize = 6;

/// The name a reference whose name is not written takes: what no definition is
/// called, so the cell matches it against every definition it could reach.
pub const DYNAMIC: &str = "*";

const DYNAMIC_CALLERS: &[&str] = &[
    "call_user_func",
    "call_user_func_array",
    "forward_static_call",
    "forward_static_call_array",
];

const SCALARS: &[&str] = &[
    "int", "float", "string", "bool", "array", "callable", "iterable", "mixed", "object", "void",
    "never", "null", "false", "true", "resource",
];

/// The bindings of one snapshot's PHP files.
pub struct PhpResolver {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    read: Vec<RepoPath>,
}

impl PhpResolver {
    /// Resolves every PHP file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        let mut manifests: Vec<(RepoPath, Option<Bytes>)> = Vec::new();
        for entry in &listing.entries {
            if entry.path.file_name() == "composer.json" {
                match reader.read(&entry.path) {
                    Ok(read) => manifests.push((entry.path.clone(), read)),
                    Err(_) => losses.push(Loss::ReadFailed {
                        path: entry.path.clone(),
                        side,
                    }),
                }
                continue;
            }
            if Language::from_path(&entry.path) != Some(Language::Php) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                losses.extend(file.losses());
                read.push(entry.path.clone());
                files.push(file);
            }
        }
        let composer = Composer::read(&manifests, &mut losses);
        let engine = Engine::new(
            Index::build(files, &ctx.workspace),
            composer,
            &ctx.workspace,
        );
        let (definitions, bindings, stopped) = engine.run(ctx);
        if stopped {
            losses.push(Loss::Deadline {
                stage: Stage::Search,
            });
        }
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        Ok(Self {
            definitions,
            bindings,
            by_target,
            losses,
            read,
        })
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The bindings that resolve to `key` or have it among their candidates.
    #[allow(
        clippy::disallowed_methods,
        reason = "a key the index has no entry for has no references"
    )]
    pub fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        self.by_target
            .get(key)
            .map(|at| at.iter().map(|i| &self.bindings[*i]).collect())
            .unwrap_or_default()
    }

    /// What the answer is partial for: files the extractor could not fully read,
    /// reads that failed, a run cut short.
    pub fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// The files the resolver read: what an absence may rest on.
    pub fn read_paths(&self) -> &[RepoPath] {
        &self.read
    }
}

// ---- composer.json

/// What one `composer.json` says about where classes live.
#[derive(Default, Debug)]
struct ComposerRec {
    /// `(namespace prefix without the trailing backslash, directories)`.
    psr4: Vec<(String, Vec<String>)>,
    psr0: Vec<(String, Vec<String>)>,
    /// Files and directories whose classes are autoloaded whatever their names.
    classmap: Vec<String>,
    /// Files loaded on every request.
    files: Vec<String>,
}

#[derive(Default, Debug)]
struct Composer {
    recs: Vec<ComposerRec>,
}

fn join(dir: &str, rest: &str) -> String {
    let rest = rest.trim_start_matches("./").trim_end_matches('/');
    match (dir.is_empty(), rest.is_empty()) {
        (_, true) => dir.to_string(),
        (true, false) => rest.to_string(),
        (false, false) => format!("{dir}/{rest}"),
    }
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

fn strings(value: Option<&serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::String(one)) => vec![one.clone()],
        Some(serde_json::Value::Array(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

impl Composer {
    fn read(manifests: &[(RepoPath, Option<Bytes>)], losses: &mut Vec<Loss>) -> Self {
        let mut recs = Vec::new();
        for (path, bytes) in manifests {
            let Some(bytes) = bytes else { continue };
            let Ok(root) = serde_json::from_slice::<serde_json::Value>(bytes) else {
                losses.push(Loss::ManifestUnread {
                    path: path.clone(),
                    reason: "composer.json is not JSON".to_string(),
                });
                continue;
            };
            let dir = dir_of(path.as_str()).to_string();
            let mut rec = ComposerRec::default();
            for section in ["autoload", "autoload-dev"] {
                let Some(autoload) = root.get(section) else {
                    continue;
                };
                for (key, into) in [("psr-4", &mut rec.psr4), ("psr-0", &mut rec.psr0)] {
                    if let Some(map) = autoload.get(key).and_then(|m| m.as_object()) {
                        for (prefix, dirs) in map {
                            let dirs = strings(Some(dirs)).iter().map(|d| join(&dir, d)).collect();
                            into.push((prefix.trim_end_matches('\\').to_string(), dirs));
                        }
                    }
                }
                rec.classmap.extend(
                    strings(autoload.get("classmap"))
                        .iter()
                        .map(|p| join(&dir, p)),
                );
                rec.files
                    .extend(strings(autoload.get("files")).iter().map(|p| join(&dir, p)));
            }
            recs.push(rec);
        }
        Self { recs }
    }

    /// Whether the name sits under a namespace some `composer.json` of the
    /// repository maps: a class of it should be found here.
    fn owns(&self, fqn: &str) -> bool {
        self.recs.iter().any(|rec| {
            rec.psr4
                .iter()
                .chain(&rec.psr0)
                .any(|(prefix, _)| under(fqn, prefix))
        })
    }

    /// The files the maps put the class in.
    fn paths_for(&self, fqn: &str) -> Vec<String> {
        let mut out = Vec::new();
        for rec in &self.recs {
            for (prefix, dirs) in &rec.psr4 {
                if under(fqn, prefix) {
                    let rest = fqn[prefix.len()..].trim_start_matches('\\');
                    for dir in dirs {
                        out.push(format!("{}.php", join(dir, &rest.replace('\\', "/"))));
                    }
                }
            }
            for (prefix, dirs) in &rec.psr0 {
                if under(fqn, prefix) {
                    let class = fqn.replace('\\', "/").replace('_', "/");
                    for dir in dirs {
                        out.push(format!("{}.php", join(dir, &class)));
                    }
                }
            }
        }
        out
    }

    /// Whether autoloading reaches the file for this class: a map places it
    /// there, or a `classmap` or `files` entry covers the file.
    fn autoloads(&self, fqn: &str, file: &str) -> bool {
        self.paths_for(fqn).iter().any(|p| p == file)
            || self.recs.iter().any(|rec| {
                rec.classmap
                    .iter()
                    .chain(&rec.files)
                    .any(|p| file == p || file.starts_with(&format!("{p}/")))
            })
    }
}

fn under(fqn: &str, prefix: &str) -> bool {
    prefix.is_empty()
        || fqn.eq_ignore_ascii_case(prefix)
        || fqn.get(..prefix.len() + 1).is_some_and(|head| {
            head[..prefix.len()].eq_ignore_ascii_case(prefix) && head.ends_with('\\')
        })
}

// ---- the header of a definition

/// What the header text of a definition says (see the facts' conventions).
#[derive(Default, Clone, Debug)]
struct Sig {
    words: Vec<String>,
    /// `class`, `interface`, `trait` or `enum` for a type.
    keyword: Option<String>,
    /// The class a written type names, for a value or a return type.
    declared: Option<String>,
    /// What a type extends and implements, as written.
    supers: Vec<String>,
    /// The traits a class uses, as written.
    traits: Vec<String>,
    /// A trait's use carries an adaptation block: members may be renamed.
    adapted: bool,
}

impl Sig {
    fn has(&self, word: &str) -> bool {
        self.words.iter().any(|w| w == word)
    }
}

const MODIFIERS: &[&str] = &[
    "public",
    "protected",
    "private",
    "static",
    "final",
    "abstract",
    "readonly",
    "var",
];

/// The class a written type names; none for a scalar, a union of classes or an
/// intersection.
fn class_of_type(text: &str) -> Option<String> {
    let text = text.trim().trim_start_matches('?');
    let mut parts: Vec<&str> = text
        .split('|')
        .map(str::trim)
        .filter(|p| !p.eq_ignore_ascii_case("null"))
        .collect();
    if parts.len() != 1 || parts[0].contains('&') {
        return None;
    }
    let one = parts.remove(0).trim_start_matches('?');
    (!one.is_empty() && !SCALARS.contains(&one.to_ascii_lowercase().as_str()))
        .then(|| one.to_string())
}

fn parse_sig(kind: DefKind, text: &str) -> Sig {
    let mut sig = Sig::default();
    match kind {
        DefKind::Class | DefKind::Interface | DefKind::Trait | DefKind::Enum => {
            let mut listing: Option<bool> = None;
            let mut traits = false;
            for token in text.split(|c: char| c.is_whitespace() || c == ',') {
                match token {
                    "" => {}
                    "extends" | "implements" => {
                        listing = Some(true);
                        traits = false;
                    }
                    "use" => {
                        traits = true;
                        listing = None;
                    }
                    "{}" => sig.adapted = true,
                    "class" | "interface" | "trait" | "enum" if sig.keyword.is_none() => {
                        sig.keyword = Some(token.to_string());
                    }
                    word if MODIFIERS.contains(&word) && sig.keyword.is_none() => {
                        sig.words.push(word.to_string());
                    }
                    word if traits => sig.traits.push(word.to_string()),
                    word if listing == Some(true) => sig.supers.push(word.to_string()),
                    _ => {}
                }
            }
        }
        DefKind::Method | DefKind::Constructor | DefKind::Function => {
            let head = text.split('(').next().unwrap_or("");
            sig.words = head
                .split_whitespace()
                .filter(|w| MODIFIERS.contains(w))
                .map(str::to_string)
                .collect();
            if let Some(open) = text.find('(') {
                let mut depth = 0usize;
                let mut close = None;
                for (i, c) in text[open..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                close = Some(open + i);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(close) = close {
                    let rest = text[close + 1..].trim();
                    if let Some(ret) = rest.strip_prefix(':') {
                        sig.declared = class_of_type(ret);
                    }
                }
            }
        }
        DefKind::Property | DefKind::Field | DefKind::Variable | DefKind::Constant => {
            let before = text.rsplit_once('$').map_or(text, |(before, _)| before);
            let tokens: Vec<&str> = before.split_whitespace().collect();
            sig.words = tokens
                .iter()
                .copied()
                .filter(|w| MODIFIERS.contains(w))
                .map(str::to_string)
                .collect();
            let written: Vec<&str> = tokens
                .iter()
                .copied()
                .filter(|w| !MODIFIERS.contains(w) && *w != "const")
                .collect();
            if text.contains('$') && !written.is_empty() {
                sig.declared = class_of_type(&written.join(" "));
            }
        }
        _ => {}
    }
    sig
}

// ---- the index

type Path = Vec<Name>;

struct DefRec {
    file: usize,
    def: Def,
    path: Path,
    key: DefKey,
    sig: Sig,
    /// The namespace the definition is in, as written.
    namespace: String,
}

struct ImportRec {
    local: String,
    path: Vec<String>,
    kind: ImportKind,
    /// The namespace region the `use` sits in.
    region: Option<usize>,
    site: Span,
    scope: Scope,
}

struct RequireRec {
    module: String,
    relative: bool,
    site: Span,
    scope: Scope,
}

struct FileRec {
    facts: FileFacts,
    project: Option<crate::snapshot::workspace::ProjectId>,
    /// The regions of the file, each with the namespace in effect.
    regions: Vec<(Range<u32>, String)>,
    imports: Vec<ImportRec>,
    requires: Vec<RequireRec>,
    children: HashMap<Path, Vec<usize>>,
    by_path: HashMap<Path, Vec<usize>>,
    /// Every name the file refers to.
    names: HashSet<String>,
}

impl FileRec {
    fn region_at(&self, byte: u32) -> Option<usize> {
        self.regions
            .iter()
            .position(|(range, _)| range.start <= byte && byte < range.end)
    }

    fn namespace_at(&self, byte: u32) -> &str {
        self.region_at(byte)
            .map_or("", |at| self.regions[at].1.as_str())
    }
}

#[derive(Default)]
struct Index {
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    by_file: HashMap<String, usize>,
    /// Every type by its lowercased qualified name.
    types: HashMap<String, Vec<usize>>,
    functions: HashMap<String, Vec<usize>>,
    consts: HashMap<String, Vec<usize>>,
    /// The short names of types and functions the repository declares.
    shorts: HashSet<String>,
    /// Members of types by lowercased name: what an untyped receiver may be.
    members: HashMap<String, Vec<usize>>,
    /// The namespaces declared, and every prefix of one, lowercased.
    namespaces: HashSet<String>,
}

fn is_type(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Class | DefKind::Interface | DefKind::Trait | DefKind::Enum
    )
}

fn is_callable(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Function | DefKind::Method | DefKind::Constructor
    )
}

fn is_value(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Property
            | DefKind::Variable
            | DefKind::Constant
            | DefKind::Field
            | DefKind::EnumMember
    )
}

fn qualified(namespace: &str, name: &str) -> String {
    if namespace.is_empty() {
        name.to_string()
    } else {
        format!("{namespace}\\{name}")
    }
}

impl Index {
    fn build(files: Vec<FileFacts>, workspace: &WorkspaceModel) -> Self {
        let mut index = Self::default();
        for facts in files {
            let at = index.files.len();
            let blob = std::sync::Arc::clone(&facts.blob);
            let project = workspace.project_of(facts.path.as_str());
            index.by_file.insert(facts.path.as_str().to_string(), at);
            let mut file = FileRec {
                facts,
                project,
                regions: Vec::new(),
                imports: Vec::new(),
                requires: Vec::new(),
                children: HashMap::new(),
                by_path: HashMap::new(),
                names: HashSet::new(),
            };
            for fact in &blob.facts {
                if let Fact::Import(import) = fact {
                    if import.kind == ImportKind::SideEffect {
                        let namespace = import.module.as_str().to_string();
                        let mut prefix = String::new();
                        for segment in namespace.split('\\').filter(|s| !s.is_empty()) {
                            if !prefix.is_empty() {
                                prefix.push('\\');
                            }
                            prefix.push_str(segment);
                            index.namespaces.insert(prefix.to_ascii_lowercase());
                        }
                        file.regions.push((import.site.bytes.clone(), namespace));
                    }
                }
            }
            for fact in &blob.facts {
                match fact {
                    Fact::Import(import) if import.kind == ImportKind::Require => {
                        file.requires.push(RequireRec {
                            module: import.module.as_str().to_string(),
                            relative: import.relative > 0,
                            site: import.site.clone(),
                            scope: import.enclosing.clone(),
                        });
                    }
                    Fact::Import(import) if import.kind != ImportKind::SideEffect => {
                        let region = file.region_at(import.site.bytes.start);
                        let module: Vec<String> = import
                            .module
                            .as_str()
                            .split('\\')
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect();
                        for (name, alias) in &import.names {
                            let mut path = module.clone();
                            path.push(name.as_str().to_string());
                            file.imports.push(ImportRec {
                                local: alias.as_ref().unwrap_or(name).as_str().to_string(),
                                path,
                                kind: import.kind,
                                region,
                                site: import.site.clone(),
                                scope: import.enclosing.clone(),
                            });
                        }
                    }
                    Fact::Reference(r) => {
                        file.names.insert(r.name.as_str().to_ascii_lowercase());
                    }
                    _ => {}
                }
            }
            index.files.push(file);
            for fact in &blob.facts {
                if let Fact::Definition(def) = fact {
                    index.add_def(at, def);
                }
            }
        }
        index
    }

    fn add_def(&mut self, at: usize, def: &Def) {
        let key = self.files[at].facts.key(&def.local);
        let mut path = def.local.container.clone();
        path.push(def.local.name.clone());
        let sig = parse_sig(def.kind, def.signature.as_deref().unwrap_or(""));
        let namespace = self.files[at]
            .namespace_at(def.span.bytes.start)
            .to_string();
        let id = self.defs.len();
        let name = def.local.name.as_str();
        let lower = name.to_ascii_lowercase();
        let top = def.local.container.is_empty();
        let fqn = qualified(&namespace, name).to_ascii_lowercase();
        let file = &mut self.files[at];
        file.children
            .entry(def.local.container.clone())
            .or_default()
            .push(id);
        file.by_path.entry(path.clone()).or_default().push(id);
        if top && is_type(def.kind) {
            self.types.entry(fqn).or_default().push(id);
            self.shorts.insert(lower);
        } else if top && def.kind == DefKind::Function {
            self.functions.entry(fqn).or_default().push(id);
            self.shorts.insert(lower);
        } else if top && def.kind == DefKind::Constant {
            self.consts
                .entry(qualified(&namespace, name))
                .or_default()
                .push(id);
        } else if !top && !is_type(def.kind) {
            self.members.entry(lower).or_default().push(id);
        }
        self.defs.push(DefRec {
            file: at,
            def: def.clone(),
            path,
            key,
            sig,
            namespace,
        });
    }

    /// The definition a path names: the primary of its twins.
    fn owner(&self, file: usize, path: &[Name]) -> Option<usize> {
        let ids = self.files[file].by_path.get(path)?;
        ids.iter()
            .copied()
            .find(|id| self.defs[*id].def.local.disambig.is_none())
            .or_else(|| ids.first().copied())
    }

    fn file_path(&self, file: usize) -> &str {
        self.files[file].facts.path.as_str()
    }
}

// ---- the engine

/// What a name, or a step of a chain, stands for.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Ent {
    /// A type named as a qualifier: its constants and static members.
    Type(usize),
    /// A value of a type declared in the repository.
    Instance(usize),
    /// A value of a type the repository does not declare.
    Foreign,
    /// Exists in the repository, out of reach.
    Outside,
    Unknown,
}

enum Found {
    Defs(Vec<usize>, EvidenceKind),
    /// Nothing in the repository: a package's or the language's.
    External,
    Blocked(UnresolvedReason),
    /// A name that should be here and is not found.
    Nothing,
}

/// A supertype of a type, or a trait it uses.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Super {
    Repo(usize),
    Foreign,
}

struct Engine<'w> {
    ix: Index,
    composer: Composer,
    workspace: &'w WorkspaceModel,
    supers: RefCell<HashMap<usize, Rc<Vec<Super>>>>,
}

fn unresolved(reason: UnresolvedReason) -> Resolution {
    Resolution::Unresolved { reason }
}

fn scope_names(scope: &Scope) -> Path {
    match scope {
        Scope::Module => Vec::new(),
        Scope::Def(key) => {
            let mut path = key.container.clone();
            path.push(key.name.clone());
            path
        }
        Scope::Callback { parent, .. } => scope_names(parent),
    }
}

fn plain_relation(use_kind: UseKind) -> Relation {
    match use_kind {
        UseKind::Call => Relation::Calls,
        UseKind::Decorate | UseKind::Instantiate => Relation::Instantiates,
        UseKind::Read => Relation::Reads,
        UseKind::Write => Relation::Writes,
        UseKind::TypeUse => Relation::TypeUses,
        UseKind::PassAsValue => Relation::PassesAsValue,
        UseKind::Override => Relation::Overrides,
    }
}

/// A method passed on is passed on; a constant or a property named as a value is
/// read.
fn relation_for(use_kind: UseKind, target: DefKind) -> Relation {
    match (use_kind, target) {
        (UseKind::PassAsValue, k) if !is_callable(k) => Relation::Reads,
        _ => plain_relation(use_kind),
    }
}

fn lower(name: &Name) -> String {
    name.as_str().to_ascii_lowercase()
}

impl<'w> Engine<'w> {
    fn new(ix: Index, composer: Composer, workspace: &'w WorkspaceModel) -> Self {
        Self {
            ix,
            composer,
            workspace,
            supers: RefCell::default(),
        }
    }

    fn run(&self, ctx: &AnalysisContext) -> (Vec<Definition>, Vec<Binding>, bool) {
        let mut bindings = Vec::new();
        let mut stopped = false;
        'files: for (f, file) in self.ix.files.iter().enumerate() {
            for fact in &file.facts.blob.facts {
                if expired(&ctx.run) {
                    stopped = true;
                    break 'files;
                }
                if let Fact::Reference(r) = fact {
                    self.reference(f, r, &mut bindings);
                }
            }
            for import in &file.imports {
                self.import(f, import, &mut bindings);
            }
            for require in &file.requires {
                self.require(f, require, &mut bindings);
            }
        }
        let definitions = self
            .ix
            .defs
            .iter()
            .map(|rec| Definition {
                file: self.ix.files[rec.file].facts.path.clone(),
                key: rec.key.clone(),
                kind: rec.def.kind,
                span: rec.def.span.clone(),
                name_span: rec.def.name_span.clone(),
            })
            .collect();
        (definitions, bindings, stopped)
    }

    // ---- reach

    fn reach(&self, f: usize, id: usize) -> std::result::Result<(), UnresolvedReason> {
        let rec = &self.ix.defs[id];
        if rec.file == f {
            return Ok(());
        }
        if !self
            .workspace
            .may_reference(self.ix.file_path(f), self.ix.file_path(rec.file))
        {
            return Err(UnresolvedReason::OutsideProject);
        }
        match rec.def.visibility {
            Visibility::Private => Err(UnresolvedReason::NoMember),
            _ => Ok(()),
        }
    }

    fn reachable(
        &self,
        f: usize,
        ids: Vec<usize>,
    ) -> std::result::Result<Vec<usize>, UnresolvedReason> {
        let mut blocked = None;
        let kept: Vec<usize> = ids
            .into_iter()
            .filter(|id| match self.reach(f, *id) {
                Ok(()) => true,
                Err(reason) => {
                    blocked = Some(reason);
                    false
                }
            })
            .collect();
        match (kept.is_empty(), blocked) {
            (true, Some(reason)) => Err(reason),
            _ => Ok(kept),
        }
    }

    // ---- names

    /// The `use` of a name in the same region of the file, by kind.
    fn alias(&self, f: usize, at: u32, local: &str, kinds: &[ImportKind]) -> Option<&ImportRec> {
        let file = &self.ix.files[f];
        let region = file.region_at(at);
        file.imports.iter().find(|i| {
            i.region == region
                && kinds.contains(&i.kind)
                && if i.kind == ImportKind::ConstOnly {
                    i.local == local
                } else {
                    i.local.eq_ignore_ascii_case(local)
                }
        })
    }

    /// The qualified name a class written as `qualifier\name` is, from a place.
    fn class_name(&self, f: usize, at: u32, qualifier: &[Name], name: &Name) -> (String, bool) {
        let file = &self.ix.files[f];
        let namespace = file.namespace_at(at);
        let mut parts: Vec<&str> = qualifier.iter().map(Name::as_str).collect();
        parts.push(name.as_str());
        if parts[0].is_empty() {
            return (parts[1..].join("\\"), false);
        }
        if parts[0].eq_ignore_ascii_case("namespace") {
            return (qualified(namespace, &parts[1..].join("\\")), false);
        }
        if let Some(import) = self.alias(f, at, parts[0], &[ImportKind::Static]) {
            let mut full = import.path.clone();
            full.extend(parts[1..].iter().map(|s| (*s).to_string()));
            return (full.join("\\"), true);
        }
        (qualified(namespace, &parts.join("\\")), false)
    }

    /// The class a name written at `at` of file `f` is.
    fn resolve_class(&self, f: usize, at: u32, qualifier: &[Name], name: &Name) -> Found {
        let (fqn, imported) = self.class_name(f, at, qualifier, name);
        self.find_type(
            f,
            &fqn,
            if imported {
                EvidenceKind::ImportedName
            } else if qualifier.is_empty() {
                EvidenceKind::SameScope
            } else {
                EvidenceKind::ModuleAttribute
            },
        )
    }

    fn find_type(&self, f: usize, fqn: &str, how: EvidenceKind) -> Found {
        let lowered = fqn.to_ascii_lowercase();
        let mut ids: Vec<usize> = self
            .ix
            .types
            .get(&lowered)
            .cloned()
            .unwrap_or_else(Vec::new);
        if ids.is_empty() {
            // A file the maps put the class in, whatever its own `namespace` says.
            let short = fqn.rsplit('\\').next().unwrap_or(fqn).to_ascii_lowercase();
            let top: Path = Vec::new();
            for path in self.composer.paths_for(fqn) {
                if let Some(file) = self.ix.by_file.get(&path) {
                    ids.extend(
                        self.ix.files[*file]
                            .children
                            .get(&top)
                            .into_iter()
                            .flatten()
                            .copied()
                            .filter(|i| {
                                let d = &self.ix.defs[*i].def;
                                is_type(d.kind)
                                    && d.local.name.as_str().eq_ignore_ascii_case(&short)
                            }),
                    );
                }
            }
        }
        if ids.is_empty() {
            let short = fqn.rsplit('\\').next().unwrap_or(fqn).to_ascii_lowercase();
            return if self.composer.owns(fqn) || self.ix.shorts.contains(&short) {
                Found::Nothing
            } else {
                Found::External
            };
        }
        if ids.len() > 1 {
            let loaded: Vec<usize> = ids
                .iter()
                .copied()
                .filter(|i| {
                    self.composer
                        .autoloads(fqn, self.ix.file_path(self.ix.defs[*i].file))
                })
                .collect();
            if !loaded.is_empty() {
                ids = loaded;
            }
            let own: Vec<usize> = ids
                .iter()
                .copied()
                .filter(|i| {
                    self.ix.files[self.ix.defs[*i].file].project == self.ix.files[f].project
                })
                .collect();
            if !own.is_empty() {
                ids = own;
            }
        }
        match self.reachable(f, ids) {
            Ok(ids) if !ids.is_empty() => Found::Defs(ids, how),
            Ok(_) => Found::Nothing,
            Err(reason) => Found::Blocked(reason),
        }
    }

    /// The function a call written at `at` is: the namespace's, else the global
    /// one.
    fn resolve_function(&self, f: usize, at: u32, qualifier: &[Name], name: &Name) -> Found {
        let file = &self.ix.files[f];
        let mut candidates: Vec<(String, EvidenceKind)> = Vec::new();
        if qualifier.is_empty() {
            if let Some(import) = self.alias(f, at, name.as_str(), &[ImportKind::FunctionOnly]) {
                candidates.push((import.path.join("\\"), EvidenceKind::ImportedName));
            } else {
                let namespace = file.namespace_at(at);
                candidates.push((qualified(namespace, name.as_str()), EvidenceKind::SameScope));
                candidates.push((name.as_str().to_string(), EvidenceKind::SameScope));
            }
        } else {
            let (fqn, _) = self.class_name(f, at, qualifier, name);
            candidates.push((fqn, EvidenceKind::ModuleAttribute));
        }
        for (fqn, how) in candidates {
            if let Some(ids) = self.ix.functions.get(&fqn.to_ascii_lowercase()) {
                return match self.reachable(f, ids.clone()) {
                    Ok(ids) if !ids.is_empty() => Found::Defs(ids, how),
                    Ok(_) => Found::Nothing,
                    Err(reason) => Found::Blocked(reason),
                };
            }
        }
        if self.ix.shorts.contains(&name.as_str().to_ascii_lowercase()) {
            Found::Nothing
        } else {
            Found::External
        }
    }

    // ---- types and members

    /// The class a scope is inside: the first name of its path.
    fn class_of_scope(&self, f: usize, scope: &[Name]) -> Option<usize> {
        let first = scope.first()?;
        let id = self.ix.owner(f, std::slice::from_ref(first))?;
        is_type(self.ix.defs[id].def.kind).then_some(id)
    }

    fn supers_of(&self, class: usize) -> Rc<Vec<Super>> {
        if let Some(known) = self.supers.borrow().get(&class) {
            return Rc::clone(known);
        }
        self.supers.borrow_mut().insert(class, Rc::default());
        let rec = &self.ix.defs[class];
        let at = rec.def.span.bytes.start;
        let mut out = Vec::new();
        for written in rec.sig.traits.iter().chain(&rec.sig.supers) {
            let mut segments: Vec<Name> = written.split('\\').map(Name::new).collect();
            let Some(name) = segments.pop() else { continue };
            match self.resolve_class(rec.file, at, &segments, &name) {
                Found::Defs(ids, _) if ids[0] != class => out.push(Super::Repo(ids[0])),
                Found::Defs(..) => {}
                _ => out.push(Super::Foreign),
            }
        }
        let out = Rc::new(out);
        self.supers.borrow_mut().insert(class, Rc::clone(&out));
        out
    }

    fn is_subtype(&self, sub: usize, sup: usize, depth: usize) -> bool {
        sub == sup
            || (depth < MAX_DEPTH
                && self.supers_of(sub).iter().any(|s| match s {
                    Super::Repo(id) => self.is_subtype(*id, sup, depth + 1),
                    Super::Foreign => false,
                }))
    }

    /// Whether the class, or a type above it, lives outside the repository.
    fn open_above(&self, class: usize, depth: usize) -> bool {
        depth < MAX_DEPTH
            && self.supers_of(class).iter().any(|s| match s {
                Super::Foreign => true,
                Super::Repo(id) => self.open_above(*id, depth + 1),
            })
    }

    /// Whether a trait of the class, or of a type above it, renames members.
    fn adapted_above(&self, class: usize, depth: usize) -> bool {
        depth < MAX_DEPTH
            && (self.ix.defs[class].sig.adapted
                || self.supers_of(class).iter().any(|s| match s {
                    Super::Repo(id) => self.adapted_above(*id, depth + 1),
                    Super::Foreign => false,
                }))
    }

    fn children_of(&self, holder: usize) -> Vec<usize> {
        let rec = &self.ix.defs[holder];
        self.ix.files[rec.file]
            .children
            .get(&rec.path)
            .cloned()
            .unwrap_or_else(Vec::new)
    }

    /// The members called `name` of the class, or of what it inherits, of the
    /// kind the use wants: the first level that has any.
    fn members_of(
        &self,
        class: usize,
        name: &Name,
        calls: bool,
        depth: usize,
    ) -> Option<(Vec<usize>, bool)> {
        if depth > MAX_DEPTH {
            return None;
        }
        let own: Vec<usize> = self
            .children_of(class)
            .into_iter()
            .filter(|c| {
                let k = &self.ix.defs[*c];
                if calls {
                    is_callable(k.def.kind)
                        && k.def
                            .local
                            .name
                            .as_str()
                            .eq_ignore_ascii_case(name.as_str())
                } else {
                    is_value(k.def.kind) && k.def.local.name == *name
                }
            })
            .collect();
        if !own.is_empty() {
            return Some((own, false));
        }
        for s in self.supers_of(class).iter() {
            if let Super::Repo(id) = s {
                if let Some((ids, _)) = self.members_of(*id, name, calls, depth + 1) {
                    return Some((ids, true));
                }
            }
        }
        None
    }

    /// Whether the class, or what it inherits, has a magic member.
    fn has_magic(&self, class: usize, magic: &str) -> bool {
        self.members_of(class, &Name::new(magic), true, 0).is_some()
    }

    fn holder_of(&self, member: usize) -> Option<usize> {
        let rec = &self.ix.defs[member];
        if rec.def.local.container.is_empty() {
            return None;
        }
        self.ix
            .owner(rec.file, &rec.def.local.container)
            .filter(|h| is_type(self.ix.defs[*h].def.kind))
    }

    /// The type a class named as written stands for from a place.
    fn type_ent(&self, f: usize, at: u32, scope: &[Name], written: &str) -> Ent {
        let mut segments: Vec<Name> = written
            .trim_start_matches('?')
            .split('\\')
            .map(Name::new)
            .collect();
        let Some(name) = segments.pop() else {
            return Ent::Unknown;
        };
        if segments.is_empty() {
            match name.as_str().to_ascii_lowercase().as_str() {
                "self" | "static" => {
                    return self
                        .class_of_scope(f, scope)
                        .map_or(Ent::Unknown, Ent::Instance);
                }
                "parent" => {
                    return self
                        .class_of_scope(f, scope)
                        .and_then(|c| self.parent_of(c))
                        .map_or(Ent::Unknown, Ent::Instance);
                }
                _ => {}
            }
        }
        match self.resolve_class(f, at, &segments, &name) {
            Found::Defs(ids, _) => Ent::Instance(ids[0]),
            Found::Blocked(_) => Ent::Outside,
            Found::External | Found::Nothing => Ent::Foreign,
        }
    }

    /// The class a type extends.
    fn parent_of(&self, class: usize) -> Option<usize> {
        let rec = &self.ix.defs[class];
        let first = rec.sig.supers.first()?;
        if rec.sig.keyword.as_deref() == Some("interface") {
            return None;
        }
        let mut segments: Vec<Name> = first.split('\\').map(Name::new).collect();
        let name = segments.pop()?;
        match self.resolve_class(rec.file, rec.def.span.bytes.start, &segments, &name) {
            Found::Defs(ids, _) => Some(ids[0]),
            _ => None,
        }
    }

    /// What a call of `path` (`[f]`, `[Class, m]`, `[Class, ->, p, m]`) returns.
    fn call_result(&self, f: usize, at: u32, scope: &[Name], path: &[Name], depth: usize) -> Ent {
        if depth > MAX_DEPTH {
            return Ent::Unknown;
        }
        let Some((name, owner)) = path.split_last() else {
            return Ent::Unknown;
        };
        if owner.is_empty() {
            return match self.resolve_function(f, at, &[], name) {
                Found::Defs(ids, _) if ids.len() == 1 => self.declared_type(ids[0]),
                Found::External | Found::Nothing => Ent::Foreign,
                _ => Ent::Unknown,
            };
        }
        let split = owner.iter().position(|n| n.as_str() == PROPERTIES);
        let (class, properties) = match split {
            Some(at) => (&owner[..at], &owner[at + 1..]),
            None => (owner, &owner[owner.len()..]),
        };
        let written: Vec<&str> = class.iter().map(Name::as_str).collect();
        let mut entity = self.type_ent(f, at, scope, &written.join("\\"));
        for property in properties {
            entity = self.step(f, &entity, property);
        }
        match entity {
            Ent::Instance(class) => match self.members_of(class, name, true, 0) {
                Some((ids, _)) if ids.len() == 1 => self.declared_type(ids[0]),
                _ => Ent::Unknown,
            },
            other => other,
        }
    }

    /// What a value is or a function returns, by the type its declaration
    /// writes. A name outside a class resolves `self` to nothing.
    fn declared_type(&self, id: usize) -> Ent {
        let rec = &self.ix.defs[id];
        let Some(declared) = &rec.sig.declared else {
            return Ent::Unknown;
        };
        let holder = self.holder_of(id);
        match (declared.to_ascii_lowercase().as_str(), holder) {
            ("self" | "static", Some(h)) => Ent::Instance(h),
            ("parent", Some(h)) => self.parent_of(h).map_or(Ent::Unknown, Ent::Instance),
            _ => {
                let holder_scope: Path =
                    holder.map_or_else(Vec::new, |h| vec![self.ix.defs[h].def.local.name.clone()]);
                self.type_ent(rec.file, rec.def.span.bytes.start, &holder_scope, declared)
            }
        }
    }

    /// One step of a chain: the property of an instance, typed by its declaration.
    fn step(&self, f: usize, from: &Ent, name: &Name) -> Ent {
        match from {
            Ent::Instance(class) | Ent::Type(class) => {
                match self.members_of(*class, name, false, 0) {
                    Some((ids, _)) if self.reach(f, ids[0]).is_ok() => self.declared_type(ids[0]),
                    _ => Ent::Unknown,
                }
            }
            Ent::Foreign => Ent::Foreign,
            Ent::Outside => Ent::Outside,
            Ent::Unknown => Ent::Unknown,
        }
    }

    // ---- references

    fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let scope = scope_names(&r.enclosing);
        let (relation, resolution, name) = if r.name.as_str().starts_with('$') {
            (
                plain_relation(r.use_kind),
                unresolved(UnresolvedReason::Opaque),
                Name::new(DYNAMIC),
            )
        } else if self.dynamic_caller(f, r) {
            (
                Relation::Calls,
                unresolved(UnresolvedReason::Opaque),
                Name::new(DYNAMIC),
            )
        } else {
            let (relation, resolution) = match r.use_kind {
                UseKind::Override => self.override_of(f, r),
                UseKind::TypeUse => self.type_use(f, r),
                _ => self.value_use(f, &scope, r),
            };
            (relation, resolution, r.name.clone())
        };
        out.push(Binding {
            file: file.facts.path.clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation,
            name,
            resolution,
            entry_guard: false,
        });
    }

    /// A call of a function that calls what its argument names.
    fn dynamic_caller(&self, f: usize, r: &Ref) -> bool {
        r.use_kind == UseKind::Call
            && matches!(r.receiver, Receiver::None)
            && DYNAMIC_CALLERS.contains(&r.name.as_str().to_ascii_lowercase().as_str())
            && matches!(
                self.resolve_function(f, r.site.bytes.start, &r.qualifier, &r.name),
                Found::External
            )
    }

    fn class_found(&self, found: Found) -> Resolution {
        match found {
            Found::Defs(ids, how) => self.resolved_or_candidates(&ids, how),
            Found::Blocked(reason) => unresolved(reason),
            Found::External => unresolved(UnresolvedReason::External),
            Found::Nothing => unresolved(UnresolvedReason::NoBinding),
        }
    }

    fn type_use(&self, f: usize, r: &Ref) -> (Relation, Resolution) {
        (
            Relation::TypeUses,
            self.class_found(self.resolve_class(f, r.site.bytes.start, &r.qualifier, &r.name)),
        )
    }

    fn resolved(&self, id: usize, how: EvidenceKind) -> Resolution {
        Resolution::Resolved {
            target: self.ix.defs[id].key.clone(),
            evidence: Evidence::new(how),
        }
    }

    fn resolved_or_candidates(&self, ids: &[usize], how: EvidenceKind) -> Resolution {
        match ids {
            [one] => self.resolved(*one, how),
            [] => unresolved(UnresolvedReason::NoBinding),
            many => self.candidates(many, CandidateReason::Overloaded),
        }
    }

    fn candidates(&self, ids: &[usize], reason: CandidateReason) -> Resolution {
        let mut targets: Vec<DefKey> = Vec::new();
        for id in ids {
            let key = self.ix.defs[*id].key.clone();
            if !targets.contains(&key) {
                targets.push(key);
            }
        }
        Resolution::Candidate { targets, reason }
    }

    fn value_use(&self, f: usize, scope: &[Name], r: &Ref) -> (Relation, Resolution) {
        let plain = plain_relation(r.use_kind);
        let at = r.site.bytes.start;
        let call = matches!(r.use_kind, UseKind::Call | UseKind::PassAsValue);
        if matches!(r.use_kind, UseKind::Instantiate | UseKind::Decorate)
            || matches!(r.receiver, Receiver::Module(_))
            || (matches!(r.receiver, Receiver::None) && r.qualifier.is_empty())
        {
            return self.bare(f, scope, r, plain);
        }
        let static_access = matches!(r.receiver, Receiver::None);
        let (entity, how, statics) = match &r.receiver {
            Receiver::SelfType => (
                self.class_of_scope(f, scope)
                    .map_or(Ent::Unknown, Ent::Instance),
                EvidenceKind::SelfReceiver,
                false,
            ),
            Receiver::Typed(written) | Receiver::New(written) => (
                self.type_ent(f, at, scope, written.as_str()),
                EvidenceKind::TypedReceiver,
                false,
            ),
            Receiver::CallResult(path) => (
                self.call_result(f, at, scope, path, 0),
                EvidenceKind::Constructed,
                false,
            ),
            Receiver::Unknown => (Ent::Unknown, EvidenceKind::TypedReceiver, false),
            Receiver::None | Receiver::Module(_) => {
                let written: Vec<&str> = r.qualifier.iter().map(Name::as_str).collect();
                let dispatch = written.len() == 1 && written[0].eq_ignore_ascii_case("static");
                let entity = match self.type_ent(f, at, scope, &written.join("\\")) {
                    Ent::Instance(class) => Ent::Type(class),
                    other => other,
                };
                (entity, EvidenceKind::ModuleAttribute, !dispatch)
            }
        };
        // The properties written between a typed head and the name.
        let mut entity = entity;
        let mut how = how;
        if !static_access {
            for name in &r.qualifier {
                entity = self.step(f, &entity, name);
                how = EvidenceKind::TypedReceiver;
            }
        }
        self.apply(f, r, entity, how, plain, call, statics)
    }

    #[allow(clippy::too_many_arguments)]
    fn apply(
        &self,
        f: usize,
        r: &Ref,
        entity: Ent,
        how: EvidenceKind,
        plain: Relation,
        call: bool,
        statics: bool,
    ) -> (Relation, Resolution) {
        match &entity {
            Ent::Instance(class) | Ent::Type(class) => {
                match self.members_of(*class, &r.name, call, 0) {
                    Some((ids, inherited)) => {
                        let how = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            how
                        };
                        self.member_result(f, r, *class, &ids, how, statics)
                    }
                    None => (plain, self.missing(*class, r, call, statics)),
                }
            }
            Ent::Foreign => (plain, unresolved(UnresolvedReason::External)),
            Ent::Outside => (plain, unresolved(UnresolvedReason::OutsideProject)),
            Ent::Unknown => (plain, self.untyped(f, r, call)),
        }
    }

    /// Why a type has no member of the name.
    fn missing(&self, class: usize, r: &Ref, call: bool, statics: bool) -> Resolution {
        let rec = &self.ix.defs[class];
        if rec.def.kind == DefKind::Enum && matches!(r.name.as_str(), "cases" | "from" | "tryFrom")
        {
            return unresolved(UnresolvedReason::Builtin);
        }
        if self.adapted_above(class, 0) {
            return unresolved(UnresolvedReason::Opaque);
        }
        let magic = match (call, r.use_kind, statics) {
            (true, _, true) => "__callStatic",
            (true, _, false) => "__call",
            (false, UseKind::Write, _) => "__set",
            (false, _, _) => "__get",
        };
        if self.has_magic(class, magic)
            || (call && !statics && self.has_magic(class, "__callStatic"))
        {
            return unresolved(UnresolvedReason::Opaque);
        }
        if self.open_above(class, 0) {
            unresolved(UnresolvedReason::ExternalBase)
        } else {
            unresolved(UnresolvedReason::NoMember)
        }
    }

    fn member_result(
        &self,
        f: usize,
        r: &Ref,
        class: usize,
        ids: &[usize],
        how: EvidenceKind,
        statics: bool,
    ) -> (Relation, Resolution) {
        let kind = self.ix.defs[ids[0]].def.kind;
        let relation = relation_for(r.use_kind, kind);
        let ids = match self.reachable(f, ids.to_vec()) {
            Ok(kept) if !kept.is_empty() => kept,
            Ok(_) => return (relation, unresolved(UnresolvedReason::NoMember)),
            Err(reason) => return (relation, unresolved(reason)),
        };
        let member = ids[0];
        if ids.len() > 1 {
            return (relation, self.candidates(&ids, CandidateReason::Overloaded));
        }
        if statics || !is_callable(self.ix.defs[member].def.kind) {
            return (relation, self.resolved(member, how));
        }
        let implementations = self.implementations(class, member);
        if implementations.is_empty() {
            return (relation, self.resolved(member, how));
        }
        let mut all = vec![member];
        all.extend(implementations);
        (relation, self.candidates(&all, CandidateReason::Dispatch))
    }

    /// The methods of the member's name in a type below the one the call is made
    /// through: each may run in its place. A `private`, `static` or `final` member
    /// is never replaced.
    fn implementations(&self, class: usize, member: usize) -> Vec<usize> {
        let rec = &self.ix.defs[member];
        if rec.def.kind != DefKind::Method
            || rec.sig.has("private")
            || rec.sig.has("static")
            || rec.sig.has("final")
        {
            return Vec::new();
        }
        let holder = self.holder_of(member);
        let Some(named) = self.ix.members.get(&lower(&rec.def.local.name)) else {
            return Vec::new();
        };
        named
            .iter()
            .copied()
            .filter(|id| *id != member)
            .filter(|id| {
                let other = &self.ix.defs[*id];
                let Some(owner) = self.holder_of(*id) else {
                    return false;
                };
                other.def.kind == DefKind::Method
                    && !other.sig.has("static")
                    && owner != class
                    && self.is_subtype(owner, class, 0)
                    && holder.is_none_or(|h| h != owner)
            })
            .collect()
    }

    /// A call or read through a receiver nothing types: the methods of that name
    /// of the types the file can hold a value of.
    fn untyped(&self, f: usize, r: &Ref, call: bool) -> Resolution {
        if !call {
            return unresolved(UnresolvedReason::UntypedRead);
        }
        let pool: Vec<usize> = self
            .ix
            .members
            .get(&lower(&r.name))
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| self.ix.defs[*id].def.kind == DefKind::Method)
            .filter(|id| self.reach(f, *id).is_ok() && self.may_hold(f, *id))
            .collect();
        match pool.len() {
            0 => unresolved(UnresolvedReason::NoCandidates),
            n if n > MAX_CANDIDATES => unresolved(UnresolvedReason::TooManyCandidates),
            _ => self.candidates(&pool, CandidateReason::UntypedReceiver),
        }
    }

    /// Whether a file can hold a value of the member's type without spelling it:
    /// it is that file, shares the namespace, or names the type or one it
    /// inherits from.
    fn may_hold(&self, f: usize, member: usize) -> bool {
        let rec = &self.ix.defs[member];
        let file = &self.ix.files[f];
        if rec.file == f {
            return true;
        }
        let Some(owner) = self.holder_of(member) else {
            return true;
        };
        let owner_rec = &self.ix.defs[owner];
        if (owner_rec.namespace.is_empty() && file.regions.is_empty())
            || file.regions.iter().any(|(_, n)| *n == owner_rec.namespace)
        {
            return true;
        }
        let mut related: Vec<String> = vec![owner_rec.def.local.name.as_str().to_ascii_lowercase()];
        related.extend(self.supers_of(owner).iter().filter_map(|s| {
            match s {
                Super::Repo(id) => Some(
                    self.ix.defs[*id]
                        .def
                        .local
                        .name
                        .as_str()
                        .to_ascii_lowercase(),
                ),
                Super::Foreign => None,
            }
        }));
        related.iter().any(|n| file.names.contains(n))
            || file.imports.iter().any(|i| {
                i.path
                    .last()
                    .is_some_and(|l| related.contains(&l.to_ascii_lowercase()))
            })
    }

    // ---- bare names

    fn bare(&self, f: usize, scope: &[Name], r: &Ref, plain: Relation) -> (Relation, Resolution) {
        let at = r.site.bytes.start;
        match r.use_kind {
            UseKind::Instantiate | UseKind::Decorate => {
                let name = r.name.as_str().to_ascii_lowercase();
                if r.qualifier.is_empty() && matches!(name.as_str(), "self" | "static" | "parent") {
                    return (
                        plain,
                        match self.type_ent(f, at, scope, &name) {
                            Ent::Instance(class) => {
                                self.resolved(class, EvidenceKind::SelfReceiver)
                            }
                            _ => unresolved(UnresolvedReason::NoBinding),
                        },
                    );
                }
                let found = self.resolve_class(f, at, &r.qualifier, &r.name);
                (plain, self.class_found(found))
            }
            _ => match self.resolve_function(f, at, &r.qualifier, &r.name) {
                Found::Defs(ids, how) => (plain, self.resolved_or_candidates(&ids, how)),
                Found::Blocked(reason) => (plain, unresolved(reason)),
                Found::External => (plain, unresolved(UnresolvedReason::Builtin)),
                Found::Nothing => (plain, unresolved(UnresolvedReason::NoBinding)),
            },
        }
    }

    // ---- overrides

    fn override_of(&self, f: usize, r: &Ref) -> (Relation, Resolution) {
        let Scope::Def(key) = &r.enclosing else {
            return (Relation::Overrides, unresolved(UnresolvedReason::NoMember));
        };
        let mut path = key.container.clone();
        path.push(key.name.clone());
        let member = self.ix.owner(f, &path);
        let holder = member.and_then(|m| self.holder_of(m));
        let Some(holder) = holder else {
            return (Relation::Overrides, unresolved(UnresolvedReason::External));
        };
        let mut found: Vec<usize> = Vec::new();
        for s in self.supers_of(holder).iter() {
            if let Super::Repo(id) = s {
                if let Some((ids, _)) = self.members_of(*id, &r.name, true, 0) {
                    found.extend(
                        ids.into_iter()
                            .filter(|i| self.ix.defs[*i].def.kind == DefKind::Method),
                    );
                }
            }
        }
        let resolution = match found.as_slice() {
            [] => unresolved(UnresolvedReason::External),
            [one] => self.resolved(*one, EvidenceKind::Overridden),
            many => self.candidates(many, CandidateReason::Overloaded),
        };
        (Relation::Overrides, resolution)
    }

    // ---- imports

    fn import(&self, f: usize, import: &ImportRec, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let Some(local) = import.path.last() else {
            return;
        };
        let fqn = import.path.join("\\");
        let found = match import.kind {
            ImportKind::FunctionOnly => match self.ix.functions.get(&fqn.to_ascii_lowercase()) {
                Some(ids) => match self.reachable(f, ids.clone()) {
                    Ok(ids) if !ids.is_empty() => Found::Defs(ids, EvidenceKind::ImportedName),
                    Ok(_) => Found::Nothing,
                    Err(reason) => Found::Blocked(reason),
                },
                None => Found::External,
            },
            ImportKind::ConstOnly => match self.ix.consts.get(&fqn) {
                Some(ids) => match self.reachable(f, ids.clone()) {
                    Ok(ids) if !ids.is_empty() => Found::Defs(ids, EvidenceKind::ImportedName),
                    Ok(_) => Found::Nothing,
                    Err(reason) => Found::Blocked(reason),
                },
                None => Found::External,
            },
            _ => {
                // `use A\B;` may name a namespace, which binds no definition.
                if self.ix.namespaces.contains(&fqn.to_ascii_lowercase())
                    && !self.ix.types.contains_key(&fqn.to_ascii_lowercase())
                {
                    return;
                }
                self.find_type(f, &fqn, EvidenceKind::ImportedName)
            }
        };
        out.push(Binding {
            file: file.facts.path.clone(),
            site: import.site.clone(),
            from: import.scope.clone(),
            relation: Relation::Imports,
            name: Name::new(local.as_str()),
            resolution: self.class_found(found),
            entry_guard: false,
        });
    }

    fn require(&self, f: usize, require: &RequireRec, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let dir = dir_of(file.facts.path.as_str());
        let module = require.module.trim_start_matches('/');
        let mut candidates = vec![join(dir, module)];
        if !require.relative {
            candidates.push(module.to_string());
        }
        let normalised: Vec<String> = candidates.iter().map(|c| normalise(c)).collect();
        let found = normalised
            .iter()
            .find(|c| self.ix.by_file.contains_key(c.as_str()));
        let resolution = match found {
            Some(path) => match RepoPath::new(path.as_str()) {
                Ok(path) => Resolution::Module {
                    file: path,
                    evidence: Evidence::new(EvidenceKind::ModuleAttribute),
                },
                Err(_) => return,
            },
            None if require.relative => unresolved(UnresolvedReason::MissingModule),
            None => unresolved(UnresolvedReason::External),
        };
        out.push(Binding {
            file: file.facts.path.clone(),
            site: require.site.clone(),
            from: require.scope.clone(),
            relation: Relation::Imports,
            name: Name::new(module.rsplit('/').next().unwrap_or(module)),
            resolution,
            entry_guard: false,
        });
    }
}

/// A path without `.` and `..` segments.
fn normalise(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_header_gives_its_supertypes_and_traits() {
        let sig = parse_sig(
            DefKind::Class,
            "final class Users extends Base implements Countable, \\A\\B use Loggable, Cached {}",
        );
        assert_eq!(sig.keyword.as_deref(), Some("class"));
        assert_eq!(sig.supers, ["Base", "Countable", "\\A\\B"]);
        assert_eq!(sig.traits, ["Loggable", "Cached"]);
        assert!(sig.adapted && sig.has("final"));
    }

    #[test]
    fn a_method_gives_its_return_class_and_a_property_its_written_one() {
        let method = parse_sig(
            DefKind::Method,
            "public static function find(int $id = f(1)): ?User",
        );
        assert_eq!(method.declared.as_deref(), Some("User"));
        assert!(method.has("static"));
        let scalar = parse_sig(DefKind::Method, "public function n(): int");
        assert_eq!(scalar.declared, None);
        let union = parse_sig(DefKind::Method, "function u(): A|B");
        assert_eq!(union.declared, None);
        let property = parse_sig(DefKind::Property, "private ?UserRepo $repo");
        assert_eq!(property.declared.as_deref(), Some("UserRepo"));
        let untyped = parse_sig(DefKind::Property, "private $repo");
        assert_eq!(untyped.declared, None);
    }

    #[test]
    fn a_prefix_covers_the_names_below_it() {
        assert!(under("App\\Models\\User", "App"));
        assert!(under("app\\x", "App"));
        assert!(!under("Application\\X", "App"));
        assert!(under("Any", ""));
    }

    #[test]
    fn composer_maps_place_a_class_in_a_file() {
        let manifests = vec![(
            RepoPath::new("svc/composer.json").expect("path"),
            Some(Bytes::from(
                br#"{"autoload":{"psr-4":{"App\\":"src/","Lib\\":["lib/","more/"]},"files":["boot.php"]}}"#
                    .to_vec(),
            )),
        )];
        let mut losses = Vec::new();
        let composer = Composer::read(&manifests, &mut losses);
        assert!(losses.is_empty());
        assert_eq!(
            composer.paths_for("App\\Models\\User"),
            ["svc/src/Models/User.php"]
        );
        assert_eq!(
            composer.paths_for("Lib\\X"),
            ["svc/lib/X.php", "svc/more/X.php"]
        );
        assert!(composer.owns("App\\Anything") && !composer.owns("Vendor\\Pkg"));
        assert!(composer.autoloads("App\\A", "svc/src/A.php"));
        assert!(composer.autoloads("Nope\\A", "svc/boot.php"));
        assert!(!composer.autoloads("Nope\\A", "svc/other.php"));
    }

    #[test]
    fn paths_lose_their_dot_segments() {
        assert_eq!(normalise("a/./b/../c.php"), "a/c.php");
    }
}
