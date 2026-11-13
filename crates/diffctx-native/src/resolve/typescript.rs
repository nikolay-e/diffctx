//! TypeScript and JavaScript: every reference and import of the facts layer
//! bound to the definitions it lands on.
//!
//! The facts state names, never meanings: `a.b.f()` is the name `f` with the
//! chain `[a, b]` written before it, and `a` may be a namespace import, a class,
//! a local or a parameter. The resolver walks that chain through the file's own
//! scopes, its imports (relative specifiers with their extension swaps and
//! `index` files, `tsconfig` `paths` and `baseUrl`, workspace packages through
//! `package.json` names and `exports`), what a module offers (declarations,
//! `export { a as b }`, `export * from`, `export default`, `module.exports`), the
//! classes with their `extends` and `implements`, and the types the code states:
//! an annotation, `new C()`, a function's declared return type.
//!
//! The facts carry no initializer of a variable, so the places that read the
//! file's text are the narrow checks that `x = [await] [new] callee(...)` is the
//! whole statement (a binding adjacent to a call, with nothing but `=` between
//! them), that `const x = a.b.c [as T]` is one chain, or `const {k: x} = Class`
//! one class, that a name is followed by `: any`, and that a call's name sits
//! right after a JSX `<`.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use super::index::{DefRec, Path, dir_of, join, scope_path};
use super::twins;
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason, plain_relation,
};
use crate::facts::{
    Def, DefKey, DefKind, Export, Fact, FactsCache, FileFacts, Import, ImportKind, Language,
    LocalKind, Name, Namespace, Receiver, Ref, Scope, Span, UseKind, Visibility, expired, facts_of,
};
use crate::snapshot::workspace::{Alias, Ecosystem, Project, ProjectId, RootOrigin};
use crate::snapshot::{
    AnalysisContext, Bytes, Loss, NotAnalysedReason, RepoPath, Result, SideTag, SnapshotReader,
    Stage, WorkspaceModel, normalize_lexical,
};

/// A call through an untyped receiver is a candidate for at most this many
/// members; past it the list says nothing a reader could use.
const MAX_CANDIDATES: usize = 24;
/// How far a chain of inferred types may feed one another.
const MAX_DEPTH: usize = 6;
/// How many re-exporting modules a name may pass through.
const MAX_HOPS: usize = 8;

/// What a specifier without an extension is tried with, in the order the
/// TypeScript compiler tries them.
const EXTENSIONS: [&str; 9] = [
    ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts",
];

/// The bindings of one snapshot's TypeScript and JavaScript files.
pub struct TypeScriptResolver {
    solved: Solved,
}

/// The languages the resolver reads: the scripts of a page or an Astro
/// component are its own, read in place (`facts::lang::html`).
fn reads(language: Language) -> bool {
    matches!(
        language,
        Language::JavaScript | Language::TypeScript | Language::Html | Language::Astro
    )
}

impl TypeScriptResolver {
    /// Resolves every TypeScript and JavaScript file of `side`, found by
    /// extension or shebang.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            let language = Language::from_path(&entry.path);
            let script = language.is_some_and(reads)
                || (language.is_none() && Language::needs_shebang(&entry.path));
            if !script {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                if reads(file.language()) {
                    losses.extend(file.losses());
                    read.push(entry.path.clone());
                    files.push(file);
                }
            }
        }
        // A symlink holds the module it names, as Node and Vite load it: an
        // import of the link reaches the definitions there (#372).
        let mut links: HashMap<String, String> = HashMap::new();
        for (path, reason) in &listing.not_analysed {
            let script = Language::from_path(path).is_some_and(reads);
            if *reason == NotAnalysedReason::Symlink && script {
                if let Some(target) = reader.resolve_link(path)? {
                    links.insert(path.as_str().to_string(), target.as_str().to_string());
                }
            }
        }
        let engine = Engine::new(
            Index::build(files, links, &ctx.workspace),
            &ctx.workspace,
            reader,
            side,
        );
        let (definitions, bindings) = engine.run();
        losses.extend(engine.losses.take());
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
        })
    }
}

solved_by!(TypeScriptResolver);

/// A CommonJS export statement names what it offers where it is read: the walker
/// sees `x` of `exports.x = x` as a read of `x`, and that read is the export row
/// of the statement, not a second use of it.
fn reads_inside_exports_are_the_export(bindings: &mut Vec<Binding>) {
    let exports: Vec<(RepoPath, std::ops::Range<u32>, DefKey)> = bindings
        .iter()
        .filter(|b| b.relation == Relation::Reexports && matches!(b.from, Scope::Module))
        .filter_map(|b| match &b.resolution {
            Resolution::Resolved { target, .. } => {
                Some((b.file.clone(), b.site.bytes.clone(), target.clone()))
            }
            _ => None,
        })
        .collect();
    if exports.is_empty() {
        return;
    }
    bindings.retain(|b| {
        let Resolution::Resolved { target, .. } = &b.resolution else {
            return true;
        };
        !(b.relation == Relation::Reads
            && matches!(b.from, Scope::Module)
            && exports.iter().any(|(file, bytes, key)| {
                *file == b.file
                    && key == target
                    && bytes.start <= b.site.bytes.start
                    && b.site.bytes.end <= bytes.end
            }))
    });
}

// ---- the index

/// The scope and the callback scopes around it, innermost first, up to the
/// definition or module that holds them: the places a local can be bound.
fn ancestors(scope: &Scope) -> Vec<&Scope> {
    let mut found = vec![scope];
    let mut current = scope;
    while let Scope::Callback { parent, .. } = current {
        current = parent;
        found.push(current);
    }
    found
}

/// What an import binds.
enum Spec {
    /// The module as a whole: `import * as ns`, `import x = require()`,
    /// `const m = require()`, `const m = await import()`.
    Module { module: String },
    /// `import { name }`, `const { name } = require()`.
    Member { module: String, name: Name },
    /// `import x from`, `const x = require().default`.
    Default { module: String },
}

struct ImportRec {
    /// The scope the binding is visible from.
    scope: Scope,
    local: Name,
    spec: Spec,
    kind: ImportKind,
}

/// What a module offers under an exported name.
enum Offer {
    /// A name of the module itself: a declaration, or something it imported.
    Local(Name),
    /// `export { name as exported } from 'module'`.
    From { module: String, name: Name },
    /// `export * as exported from 'module'`.
    Namespace { module: String },
}

struct LocalRec {
    kind: LocalKind,
    scope: Scope,
    site: Span,
    /// Where the binding sits among the file's facts.
    fact: usize,
}

struct FileRec {
    facts: FileFacts,
    dir: String,
    project: Option<ProjectId>,
    /// Definitions by the path of the scope that holds them.
    children: HashMap<Path, Vec<usize>>,
    /// Definitions by their own path (overloads and merged declarations share one).
    by_path: HashMap<Path, Vec<usize>>,
    locals: HashMap<Name, Vec<LocalRec>>,
    imports: Vec<ImportRec>,
    exports: HashMap<Name, Offer>,
    /// The modules of `export * from`.
    stars: Vec<String>,
    /// The scripts `importScripts('...')` loads, as written.
    scripts: Vec<String>,
    /// A script with no module syntax: its top-level names are globals.
    classic: bool,
    /// The glob patterns of `import.meta.glob` and `require.context`, with the
    /// site and scope of each.
    globs: Vec<(String, Span, Scope)>,
}

#[derive(Default)]
struct Index {
    files: Vec<FileRec>,
    defs: Vec<DefRec>,
    by_file: HashMap<String, usize>,
    /// Methods and properties by name, of classes, interfaces and the object
    /// literals a function returns: what an untyped receiver may be.
    members: HashMap<Name, Vec<usize>>,
    /// Symlinks of the snapshot to the path they name.
    links: HashMap<String, String>,
    /// The top-level definitions a module offers, by name.
    public_named: HashMap<Name, Vec<usize>>,
    /// The definitions that are a module's default export.
    defaults: Vec<usize>,
    /// The top-level definitions of classic scripts, by name.
    globals: HashMap<Name, Vec<usize>>,
}

fn is_relative(specifier: &str) -> bool {
    specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier == "."
        || specifier == ".."
}

fn strip_source_extension(path: &str) -> &str {
    [
        ".d.ts", ".tsx", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".ts", ".js",
    ]
    .iter()
    .find_map(|extension| path.strip_suffix(extension))
    .unwrap_or(path)
}

/// `@scope/name/sub/path` is the package `@scope/name` and the rest `sub/path`.
fn split_package(specifier: &str) -> (&str, &str) {
    let mut ends = specifier.match_indices('/').map(|(at, _)| at);
    let end = if specifier.starts_with('@') {
        ends.nth(1)
    } else {
        ends.next()
    };
    match end {
        Some(at) => (&specifier[..at], &specifier[at + 1..]),
        None => (specifier, ""),
    }
}

impl Index {
    fn build(
        files: Vec<FileFacts>,
        links: HashMap<String, String>,
        workspace: &WorkspaceModel,
    ) -> Self {
        let mut index = Self {
            links,
            ..Self::default()
        };
        for (at, facts) in files.into_iter().enumerate() {
            let path = facts.path.as_str().to_string();
            index.by_file.insert(path.clone(), at);
            index.files.push(FileRec {
                dir: dir_of(&path).to_string(),
                project: workspace.project_of(&path),
                facts,
                children: HashMap::new(),
                by_path: HashMap::new(),
                locals: HashMap::new(),
                imports: Vec::new(),
                exports: HashMap::new(),
                stars: Vec::new(),
                scripts: Vec::new(),
                classic: false,
                globs: Vec::new(),
            });
            index.read(at);
        }
        index.collect_members();
        index.collect_offers();
        index
    }

    fn read(&mut self, at: usize) {
        let blob = Arc::clone(&self.files[at].facts.blob);
        let mut aliased: HashSet<Name> = HashSet::new();
        let mut module_syntax = false;
        for (fact_at, fact) in blob.facts.iter().enumerate() {
            match fact {
                Fact::Definition(def) => self.add_def(at, fact_at, def),
                Fact::LocalBinding(local) => {
                    self.files[at]
                        .locals
                        .entry(local.name.clone())
                        .or_default()
                        .push(LocalRec {
                            kind: local.kind,
                            scope: local.enclosing.clone(),
                            site: local.site.clone(),
                            fact: fact_at,
                        });
                }
                Fact::Import(import) => {
                    if import.kind == ImportKind::SideEffect {
                        self.files[at]
                            .scripts
                            .push(import.module.as_str().to_string());
                    } else {
                        module_syntax = true;
                    }
                    add_import(&mut self.files[at], import);
                }
                Fact::Export(export) => {
                    module_syntax = true;
                    add_export(&mut self.files[at], export, &mut aliased);
                }
                Fact::PathMention(mention) => {
                    self.files[at].globs.push((
                        mention.text.clone(),
                        mention.site.clone(),
                        mention.enclosing.clone(),
                    ));
                }
                _ => {}
            }
        }
        self.files[at].classic = !module_syntax;
        // What a module declares and marks public is offered under its own name,
        // unless it is offered only under another.
        #[allow(
            clippy::disallowed_methods,
            reason = "a file with no top-level children offers nothing"
        )]
        let offered: Vec<(Name, usize)> = self.files[at]
            .children
            .get(&Vec::new())
            .map(|ids| {
                ids.iter()
                    .filter(|id| self.defs[**id].def.visibility == Visibility::Public)
                    .map(|id| (self.defs[*id].def.local.name.clone(), *id))
                    .collect()
            })
            .unwrap_or_default();
        let file = &mut self.files[at];
        for (name, _) in offered {
            if !aliased.contains(&name) {
                file.exports
                    .entry(name.clone())
                    .or_insert(Offer::Local(name));
            }
        }
    }

    fn add_def(&mut self, at: usize, fact_at: usize, def: &Def) {
        let key = self.files[at].facts.key(&def.local);
        let mut path = def.local.container.clone();
        path.push(def.local.name.clone());
        let id = self.defs.len();
        self.defs.push(DefRec {
            file: at,
            def: def.clone(),
            path: path.clone(),
            key,
            fact: fact_at,
        });
        let file = &mut self.files[at];
        file.children
            .entry(def.local.container.clone())
            .or_default()
            .push(id);
        file.by_path.entry(path).or_default().push(id);
    }

    fn collect_members(&mut self) {
        for id in 0..self.defs.len() {
            let rec = &self.defs[id];
            if rec.path.len() >= 2 && matches!(rec.def.kind, DefKind::Method | DefKind::Property) {
                let name = rec.def.local.name.clone();
                self.members.entry(name).or_default().push(id);
            }
        }
    }

    /// What modules offer by name, the default exports, and the globals of
    /// classic scripts.
    fn collect_offers(&mut self) {
        for at in 0..self.files.len() {
            #[allow(
                clippy::disallowed_methods,
                reason = "a file with no top-level children has none to offer"
            )]
            let top = self.files[at]
                .children
                .get(&Vec::new())
                .cloned()
                .unwrap_or_default();
            if self.files[at].classic {
                for id in &top {
                    let name = self.defs[*id].def.local.name.clone();
                    self.globals.entry(name).or_default().push(*id);
                }
            }
            for id in &top {
                if self.defs[*id].def.visibility == Visibility::Public {
                    let name = self.defs[*id].def.local.name.clone();
                    self.public_named.entry(name).or_default().push(*id);
                }
            }
            let default = Name::new("default");
            if let Some(Offer::Local(local)) = self.files[at].exports.get(&default) {
                let named: Vec<usize> = top
                    .iter()
                    .copied()
                    .filter(|id| self.defs[*id].def.local.name == *local)
                    .collect();
                if let Some(id) = self.primary(&named, false) {
                    self.defaults.push(id);
                }
            }
        }
    }

    /// The definition a path names: the primary of its twins.
    fn owner(&self, file: usize, path: &[Name]) -> Option<usize> {
        let ids = self.files[file].by_path.get(path)?;
        self.primary(ids, false)
    }

    /// Of definitions sharing a name: the one the code means by it. A class
    /// stands before its merged namespace and interface; of overloads, the
    /// implementation, which is written last.
    fn primary(&self, ids: &[usize], want_type: bool) -> Option<usize> {
        let wanted = if want_type {
            Namespace::Type
        } else {
            Namespace::Value
        };
        let in_wanted: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|id| self.defs[*id].def.local.ns == wanted)
            .collect();
        let pool: &[usize] = if in_wanted.is_empty() {
            ids
        } else {
            &in_wanted
        };
        pool.iter()
            .copied()
            .find(|id| matches!(self.defs[*id].def.kind, DefKind::Class | DefKind::Enum))
            .or_else(|| pool.last().copied())
    }
}

fn add_import(file: &mut FileRec, import: &Import) {
    // `const Card = lazy(() => import('./Card'))` binds the constant to the
    // import's default, but the constant stays the local definition it is; the
    // import is a binding of its own.
    if import.kind == ImportKind::Dynamic && import.default.is_some() {
        return;
    }
    let module = import.module.as_str().to_string();
    let scope = import.enclosing.clone();
    let mut push = |local: &Name, spec: Spec| {
        file.imports.push(ImportRec {
            scope: scope.clone(),
            local: local.clone(),
            spec,
            kind: import.kind,
        });
    };
    if let Some(local) = import.star.alias() {
        push(
            local,
            Spec::Module {
                module: module.clone(),
            },
        );
    }
    if let Some(local) = &import.default {
        push(
            local,
            Spec::Default {
                module: module.clone(),
            },
        );
    }
    for (name, alias) in &import.names {
        push(
            alias.as_ref().unwrap_or(name),
            Spec::Member {
                module: module.clone(),
                name: name.clone(),
            },
        );
    }
}

fn add_export(file: &mut FileRec, export: &Export, aliased: &mut HashSet<Name>) {
    match export {
        Export::Named {
            name,
            alias,
            from,
            type_only: _,
            ..
        } => {
            let exported = alias.clone().unwrap_or_else(|| name.clone());
            let offer = match from {
                Some(module) => Offer::From {
                    module: module.as_str().to_string(),
                    name: name.clone(),
                },
                None => {
                    if alias.is_some() {
                        aliased.insert(name.clone());
                    }
                    Offer::Local(name.clone())
                }
            };
            file.exports.insert(exported, offer);
        }
        Export::Star { from, alias, .. } => match alias {
            Some(alias) => {
                file.exports.insert(
                    alias.clone(),
                    Offer::Namespace {
                        module: from.as_str().to_string(),
                    },
                );
            }
            None => file.stars.push(from.as_str().to_string()),
        },
        Export::Default { local, .. } => {
            let local = local.clone().unwrap_or_else(|| Name::new("default"));
            file.exports
                .insert(Name::new("default"), Offer::Local(local));
        }
    }
}

// ---- text helpers

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The index after the string that opens at `start`.
fn skip_string(text: &[u8], start: usize) -> Option<usize> {
    let quote = *text.get(start)?;
    let mut at = start + 1;
    while let Some(&b) = text.get(at) {
        if b == b'\\' {
            at += 2;
        } else if b == quote {
            return Some(at + 1);
        } else {
            at += 1;
        }
    }
    None
}

/// The index after the group that opens at `open` with `(`, `[` or `{`,
/// skipping strings and comments.
fn group_end(text: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = open;
    while let Some(&b) = text.get(at) {
        match b {
            b'"' | b'\'' | b'`' => {
                at = skip_string(text, at)?;
                continue;
            }
            b'/' if text.get(at + 1) == Some(&b'/') => {
                while text.get(at).is_some_and(|c| *c != b'\n') {
                    at += 1;
                }
                continue;
            }
            b'/' if text.get(at + 1) == Some(&b'*') => {
                at += 2;
                while at + 1 < text.len() && !(text[at] == b'*' && text[at + 1] == b'/') {
                    at += 1;
                }
                at += 2;
                continue;
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

fn skip_blanks(text: &[u8], mut at: usize) -> usize {
    while text
        .get(at)
        .is_some_and(|b| matches!(*b, b' ' | b'\t' | b'\n' | b'\r'))
    {
        at += 1;
    }
    at
}

/// The `(` of the call whose callee name ends at `at`, past type arguments.
fn call_open(text: &[u8], at: usize) -> Option<usize> {
    let mut at = skip_blanks(text, at);
    if text.get(at) == Some(&b'<') {
        let mut depth = 0usize;
        while let Some(&b) = text.get(at) {
            match b {
                b'<' => depth += 1,
                b'>' => {
                    depth -= 1;
                    if depth == 0 {
                        at += 1;
                        break;
                    }
                }
                b'(' | b';' | b'{' => return None,
                _ => {}
            }
            at += 1;
        }
        at = skip_blanks(text, at);
    }
    (text.get(at) == Some(&b'(')).then_some(at)
}

/// Whether the expression that ends at `at` ends its statement or its item:
/// nothing continues it with a member, an index or an operator.
fn expression_ends(text: &[u8], mut at: usize) -> bool {
    while text.get(at).is_some_and(|b| matches!(*b, b' ' | b'\t')) {
        at += 1;
    }
    text.get(at)
        .is_none_or(|b| matches!(*b, b'\n' | b'\r' | b';' | b',' | b'}' | b')'))
}

/// The type written after the name that ends at `at` (`: Foo`), up to the
/// `,`, `)`, `=`, `;` or end of line that closes it.
fn annotation_after(text: &[u8], at: usize) -> Option<String> {
    let mut i = at;
    while text.get(i).is_some_and(|b| matches!(*b, b' ' | b'\t')) {
        i += 1;
    }
    if matches!(text.get(i), Some(b'?' | b'!')) {
        i += 1;
    }
    while text.get(i).is_some_and(|b| matches!(*b, b' ' | b'\t')) {
        i += 1;
    }
    if text.get(i) != Some(&b':') {
        return None;
    }
    written_type(text, i + 1)
}

/// The type written from `start` to the `,`, `)`, `=`, `;` or end of line that
/// closes it.
fn written_type(text: &[u8], start: usize) -> Option<String> {
    let mut i = start;
    let mut depth = 0usize;
    while let Some(&b) = text.get(i) {
        match b {
            b'=' if text.get(i + 1) == Some(&b'>') => {
                i += 2;
                continue;
            }
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' | b'>' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b',' | b'=' | b';' | b'\n' if depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    #[allow(
        clippy::disallowed_methods,
        reason = "a type annotation that is not UTF-8 is not read as written text; the caller takes `None` as no written type"
    )]
    let written = std::str::from_utf8(&text[start..i]).ok()?.trim();
    (!written.is_empty()).then(|| written.to_string())
}

/// The text without its leading `word` when the word stands alone.
fn strip_word<'t>(text: &'t str, word: &str) -> Option<&'t str> {
    let rest = text.strip_prefix(word)?;
    rest.starts_with(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
        .then_some(rest)
}

/// The parts of a type written with `|` or `&` at its top level.
fn split_top(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0usize, 0usize);
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '=' if chars.peek().is_some_and(|(_, n)| *n == '>') => {
                chars.next();
            }
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            c if c == separator && depth == 0 => {
                parts.push(&text[start..at]);
                start = at + c.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn is_dotted(text: &str) -> bool {
    !text.is_empty()
        && text.split('.').all(|part| {
            let mut chars = part.chars();
            chars
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
                && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        })
}

// ---- what a signature states

/// The `extends` and `implements` names of a class or interface signature, each
/// with whether it is an `implements`. The signature reads `[modifiers]
/// extends B < T > implements I , J [< T extends X >]`: the heritage comes
/// before the type parameters.
fn heritage_names(signature: &str) -> Vec<(bool, String)> {
    let tokens: Vec<&str> = signature.split_whitespace().collect();
    let mut found = Vec::new();
    let mut mode: Option<bool> = None;
    let (mut depth, mut expecting) = (0usize, false);
    let mut previous = "";
    let mut at = 0;
    while at < tokens.len() {
        let token = tokens[at];
        match token {
            "extends" | "implements" if depth == 0 => {
                mode = Some(token == "implements");
                expecting = true;
            }
            _ if mode.is_none() => {}
            "," if depth == 0 => expecting = true,
            t if t.chars().all(|c| c == '<') => {
                if depth == 0 && previous.ends_with('>') {
                    break;
                }
                depth += t.len();
            }
            t if t.chars().all(|c| c == '>') => depth = depth.saturating_sub(t.len()),
            t if depth == 0 && expecting && is_dotted(t) => {
                let mut name = t.to_string();
                while tokens.get(at + 1) == Some(&".") {
                    if let Some(next) = tokens.get(at + 2) {
                        name.push('.');
                        name.push_str(next);
                    }
                    at += 2;
                }
                if let Some(implements) = mode {
                    found.push((implements, name));
                }
                expecting = false;
            }
            _ => {}
        }
        previous = token;
        at += 1;
    }
    found
}

/// The type a function's signature declares for what it returns.
fn return_type(signature: &str) -> Option<String> {
    let tokens: Vec<&str> = signature.split_whitespace().collect();
    let open = tokens.iter().position(|t| *t == "(")?;
    let mut depth = 0usize;
    let mut close = None;
    for (at, token) in tokens.iter().enumerate().skip(open) {
        match *token {
            "(" => depth += 1,
            ")" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    close = Some(at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    (tokens.get(close + 1) == Some(&":")).then(|| tokens[close + 2..].concat())
}

/// The type a field, variable or accessor declares for what it holds.
fn declared_type(signature: &str) -> Option<String> {
    let tokens: Vec<&str> = signature.split_whitespace().collect();
    let at = tokens.iter().position(|t| *t == ":")?;
    let written = tokens[at + 1..].concat();
    (!written.is_empty()).then_some(written)
}

/// The first name of the qualifier of a write that is a key of an object literal
/// handed to a call (`<arg0>` for the first argument), the callee's path after it.
const ARGUMENT: &str = "<arg";

/// The type written for the `index`th parameter of a signature (`( a : T , b : U )`
/// or a function type `: ( a : T ) => U`): a name, as written, without its type
/// arguments; none for a pattern, a literal type or a parameter with no type.
fn parameter_type(signature: &str, index: usize) -> Option<String> {
    let tokens: Vec<&str> = signature.split_whitespace().collect();
    let open = tokens.iter().position(|t| *t == "(")?;
    let mut depth = 0usize;
    let mut parameters: Vec<Vec<&str>> = vec![Vec::new()];
    for token in &tokens[open..] {
        match *token {
            "(" | "{" | "[" | "<" => {
                depth += 1;
                if depth > 1 {
                    parameters.last_mut()?.push(*token);
                }
            }
            ")" | "}" | "]" | ">" => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    break;
                }
                parameters.last_mut()?.push(*token);
            }
            "," if depth == 1 => parameters.push(Vec::new()),
            _ => parameters.last_mut()?.push(*token),
        }
    }
    let parameter = parameters.get(index)?;
    let colon = parameter.iter().position(|t| *t == ":")?;
    let mut written = String::new();
    for token in &parameter[colon + 1..] {
        let name = token
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'));
        if name || *token == "." {
            written.push_str(token);
        } else {
            break;
        }
    }
    (!written.is_empty()).then_some(written)
}

fn is_abstract(signature: Option<&str>) -> bool {
    signature.is_some_and(|s| s.split_whitespace().any(|t| t == "abstract"))
}

// ---- the engine

#[derive(Clone, PartialEq, Eq, Debug)]
enum Entity {
    Module(usize),
    /// A definition that holds members: a class, an interface, an enum, a
    /// namespace.
    Class(usize),
    /// A value of the class or interface: `this`, an annotated name, a built object.
    Instance(usize),
    /// `super` inside the class: its bases, not itself.
    Super(usize),
    /// A function, method, variable, constant or other definition.
    Def(usize),
    /// Annotated `any`: any class.
    Dynamic,
    /// `this.constructor`: the class of the instance, or a class that extends it.
    Meta(usize),
    /// What a module nothing places may offer under the name: every definition of
    /// it, or every default export.
    Maybe(Vec<usize>),
    External,
    Builtin,
    Outside,
    Unknown,
}

type Hit = twins::Hit<Entity>;

enum Named {
    Local(usize),
    Hit(Hit),
}

enum Lookup {
    Found {
        entity: Entity,
        through: Vec<usize>,
        inherited: bool,
    },
    /// The member is declared by an interface or as abstract: the definitions
    /// that may be the one run.
    Dispatch(Vec<usize>),
    Absent(UnresolvedReason),
    /// The receiver says nothing about what it is.
    Untyped,
    Any,
}

type Outcome = twins::EntityOutcome<Entity>;

#[derive(Clone, PartialEq, Eq, Debug)]
enum ModRes {
    File(usize),
    /// A package outside the repository, or a built-in module.
    External,
    /// A relative specifier, or a workspace package, that lands on no file.
    Missing,
    /// A file of a project the importer has no declared dependency on.
    Outside,
    /// An alias or subpath import nothing places, or a bare one in a project whose
    /// configuration could not be read whole: the name may be any module's.
    Unplaced,
}

type Seen = HashSet<(usize, Name)>;

struct Engine<'a> {
    ix: Index,
    workspace: &'a WorkspaceModel,
    reader: &'a SnapshotReader,
    side: SideTag,
    texts: RefCell<HashMap<usize, Option<Bytes>>>,
    modules: RefCell<HashMap<(usize, String), ModRes>>,
    instances: RefCell<HashMap<usize, Entity>>,
    bases: RefCell<HashMap<usize, Rc<Vec<(Entity, bool)>>>>,
    imported: RefCell<HashMap<usize, Rc<HashSet<usize>>>>,
    linked: RefCell<HashMap<usize, Rc<HashSet<usize>>>>,
    subtype_map: OnceCell<HashMap<usize, Vec<usize>>>,
    globs: RefCell<HashMap<usize, Rc<Vec<GlobSet>>>>,
    wrapped: RefCell<HashMap<usize, Option<Entity>>>,
    losses: RefCell<Vec<Loss>>,
}

/// What one `import.meta.glob(...)` or `require.context(...)` loads.
struct GlobSet {
    site: Span,
    scope: Scope,
    /// The first positive pattern, as written.
    label: String,
    files: Vec<usize>,
}

impl<'a> Engine<'a> {
    fn new(
        ix: Index,
        workspace: &'a WorkspaceModel,
        reader: &'a SnapshotReader,
        side: SideTag,
    ) -> Self {
        Self {
            ix,
            workspace,
            reader,
            side,
            texts: RefCell::default(),
            modules: RefCell::default(),
            instances: RefCell::default(),
            bases: RefCell::default(),
            imported: RefCell::default(),
            linked: RefCell::default(),
            subtype_map: OnceCell::new(),
            globs: RefCell::default(),
            wrapped: RefCell::default(),
            losses: RefCell::default(),
        }
    }

    fn run(&self) -> (Vec<Definition>, Vec<Binding>) {
        let mut bindings = Vec::new();
        for (f, file) in self.ix.files.iter().enumerate() {
            for fact in &file.facts.blob.facts {
                match fact {
                    Fact::Reference(r) => self.reference(f, r, &mut bindings),
                    Fact::Import(import) => self.import(f, import, &mut bindings),
                    Fact::Export(export) => self.export(f, export, &mut bindings),
                    _ => {}
                }
            }
        }
        self.globs_imported(&mut bindings);
        self.overrides(&mut bindings);
        reads_inside_exports_are_the_export(&mut bindings);
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
        (definitions, bindings)
    }

    fn path_of(&self, f: usize) -> &RepoPath {
        &self.ix.files[f].facts.path
    }

    fn paths(&self, files: &[usize]) -> Vec<RepoPath> {
        files.iter().map(|f| self.path_of(*f).clone()).collect()
    }

    /// Whether the projects allow a name used in file `f` to refer to something
    /// in file `g`: one project, a declared dependency, or no project at all.
    fn allowed(&self, f: usize, g: usize) -> bool {
        f == g
            || self
                .workspace
                .may_reference(self.path_of(f).as_str(), self.path_of(g).as_str())
    }

    /// Whether a name used in file `f` may refer to something in file `g`: the
    /// projects allow it, or a chain of the modules `f` imports leads there. A
    /// relative path names its file whatever project either belongs to; only a
    /// bare package name is held to the dependencies the manifests declare.
    fn reaches(&self, f: usize, g: usize) -> bool {
        self.allowed(f, g) || self.linked(f).contains(&g)
    }

    /// The files a chain of resolved imports from `f` lands on.
    fn linked(&self, f: usize) -> Rc<HashSet<usize>> {
        if let Some(known) = self.linked.borrow().get(&f) {
            return Rc::clone(known);
        }
        let mut seen: HashSet<usize> = HashSet::from([f]);
        let mut queue = vec![f];
        while let Some(at) = queue.pop() {
            let file = &self.ix.files[at];
            let mut modules: Vec<&str> = Vec::new();
            for fact in &file.facts.blob.facts {
                if let Fact::Import(import) = fact {
                    modules.push(import.module.as_str());
                }
            }
            for rec in &file.imports {
                match &rec.spec {
                    Spec::Module { module }
                    | Spec::Member { module, .. }
                    | Spec::Default { module } => modules.push(module),
                }
            }
            for offer in file.exports.values() {
                match offer {
                    Offer::From { module, .. } | Offer::Namespace { module } => {
                        modules.push(module)
                    }
                    Offer::Local(_) => {}
                }
            }
            modules.extend(file.stars.iter().map(String::as_str));
            for module in modules {
                if let ModRes::File(g) = self.module(at, module) {
                    if seen.insert(g) {
                        queue.push(g);
                    }
                }
            }
        }
        let seen = Rc::new(seen);
        self.linked.borrow_mut().insert(f, Rc::clone(&seen));
        seen
    }

    fn reachable(&self, f: usize, def: usize) -> bool {
        self.reaches(f, self.ix.defs[def].file)
    }

    file_text!();

    // ---- where a specifier lands

    fn module(&self, f: usize, specifier: &str) -> ModRes {
        let key = (f, specifier.to_string());
        if let Some(known) = self.modules.borrow().get(&key) {
            return known.clone();
        }
        let found = self.find_module(f, specifier);
        self.modules.borrow_mut().insert(key, found.clone());
        found
    }

    fn landed(&self, f: usize, g: usize) -> ModRes {
        if self.allowed(f, g) {
            ModRes::File(g)
        } else {
            ModRes::Outside
        }
    }

    fn find_module(&self, f: usize, written: &str) -> ModRes {
        let file = &self.ix.files[f];
        // A bundler query (`./sw.js?raw`) names the same file (#558); a leading
        // `#` is a subpath import, not a fragment.
        let specifier = if written.starts_with('#') {
            written
        } else {
            written.split('?').next().unwrap_or(written)
        };
        if is_relative(specifier) {
            let Some(base) = normalize_lexical(&join(&file.dir, specifier)) else {
                return ModRes::Missing;
            };
            return match self.probe(&base) {
                Some(g) => ModRes::File(g),
                None if is_asset(specifier) => ModRes::External,
                None => ModRes::Missing,
            };
        }
        if let Some(from_root) = specifier.strip_prefix('/') {
            return match normalize_lexical(from_root).and_then(|base| self.probe(&base)) {
                Some(g) => ModRes::File(g),
                None => ModRes::Unplaced,
            };
        }
        let (mut outside, mut unplaced) = (false, false);
        let around = self.projects_around(f);
        for project in &around {
            let mut targets: Vec<String> = Vec::new();
            if let Some(alias) = best_alias(specifier, &project.aliases) {
                unplaced = true;
                targets.extend(alias_targets(specifier, alias));
            }
            targets.extend(project.base_urls.iter().map(|base| join(base, specifier)));
            targets.extend(
                project
                    .import_roots
                    .iter()
                    .filter(|r| r.origin == RootOrigin::Configured)
                    .map(|r| join(&r.dir, specifier)),
            );
            for target in targets {
                let Some(base) = normalize_lexical(&target) else {
                    continue;
                };
                if let Some(g) = self.probe(&base) {
                    if self.allowed(f, g) {
                        return ModRes::File(g);
                    }
                    outside = true;
                }
            }
            if specifier.starts_with('#') {
                for base in package_entries(project, specifier) {
                    if let Some(g) = self.probe_entry(&base) {
                        return self.landed(f, g);
                    }
                }
            }
        }
        if let Some(found) = self.package_module(f, specifier) {
            return found;
        }
        let incomplete = around.iter().any(|project| project.paths_incomplete);
        if outside {
            ModRes::Outside
        } else if unplaced || incomplete || is_alias_like(specifier) {
            ModRes::Unplaced
        } else {
            ModRes::External
        }
    }

    /// The projects whose configuration a file's imports are read with: its own,
    /// then those whose directory holds it, nearest first.
    fn projects_around(&self, f: usize) -> Vec<&Project> {
        let file = &self.ix.files[f];
        #[allow(
            clippy::disallowed_methods,
            reason = "a file under no project has no project of its own to read imports with"
        )]
        let mut found: Vec<&Project> = file
            .project
            .map(|id| vec![self.workspace.project(id)])
            .unwrap_or_default();
        let mut holders: Vec<&Project> = self
            .workspace
            .projects()
            .iter()
            .filter(|p| {
                file.project.is_none_or(|own| p.id != own)
                    && (p.root.is_empty()
                        || file.dir == p.root
                        || file.dir.starts_with(&format!("{}/", p.root)))
            })
            .collect();
        holders.sort_by_key(|p| std::cmp::Reverse(p.root.len()));
        found.extend(holders);
        found
    }

    /// The file a path names: itself, with an extension, or as a directory's
    /// `index`. A `.js` written for a `.ts` source finds the source.
    fn probe(&self, base: &str) -> Option<usize> {
        let stem = strip_source_extension(base);
        let mut tries: Vec<String> = vec![base.to_string()];
        for root in [stem, base] {
            tries.extend(
                EXTENSIONS
                    .iter()
                    .map(|extension| format!("{root}{extension}")),
            );
        }
        tries.extend(
            EXTENSIONS
                .iter()
                .map(|extension| join(base, &format!("index{extension}"))),
        );
        tries.iter().find_map(|path| self.file_at(path))
    }

    /// The file at a path of the snapshot: itself, or what a symlink there names.
    fn file_at(&self, path: &str) -> Option<usize> {
        if let Some(found) = self.ix.by_file.get(path) {
            return Some(*found);
        }
        self.ix
            .links
            .get(path)
            .and_then(|target| self.ix.by_file.get(target))
            .copied()
    }

    /// An entry point a manifest names; a build output stands for its source.
    fn probe_entry(&self, base: &str) -> Option<usize> {
        self.probe(base).or_else(|| {
            let parts: Vec<&str> = base.split('/').collect();
            let at = parts
                .iter()
                .rposition(|p| matches!(*p, "dist" | "build" | "out" | "lib" | "esm" | "cjs"))?;
            let mut mapped = parts.clone();
            mapped[at] = "src";
            self.probe(&mapped.join("/"))
        })
    }

    /// A bare specifier that names a package of the workspace, through the name
    /// its `package.json` gives it. A package the importer does not depend on is
    /// outside.
    fn package_module(&self, f: usize, specifier: &str) -> Option<ModRes> {
        let (package, rest) = split_package(specifier);
        let user = self.ix.files[f].project;
        let mut result: Option<ModRes> = None;
        for project in self.workspace.projects() {
            if !project
                .names
                .iter()
                .any(|(e, n)| *e == Ecosystem::Node && n == package)
            {
                continue;
            }
            let allowed =
                user.is_none_or(|u| u == project.id || self.workspace.depends(u, project.id));
            if !allowed {
                result = result.or(Some(ModRes::Outside));
                continue;
            }
            let subpath = if rest.is_empty() {
                ".".to_string()
            } else {
                format!("./{rest}")
            };
            for base in package_entries(project, &subpath) {
                if let Some(g) = self.probe_entry(&base) {
                    return Some(ModRes::File(g));
                }
            }
            result = Some(ModRes::Missing);
        }
        result
    }

    // ---- names

    fn is_scope_container(&self, f: usize, path: &[Name]) -> bool {
        self.ix.owner(f, path).is_some_and(|id| {
            matches!(
                self.ix.defs[id].def.kind,
                DefKind::Function
                    | DefKind::Method
                    | DefKind::Constructor
                    | DefKind::Property
                    | DefKind::Module
                    | DefKind::Variable
                    | DefKind::Constant
                    | DefKind::Field
            )
        })
    }

    fn entity_of_def(&self, id: usize) -> Entity {
        match self.ix.defs[id].def.kind {
            DefKind::Class
            | DefKind::Interface
            | DefKind::Enum
            | DefKind::Module
            | DefKind::Object => Entity::Class(id),
            // `type Prefs = { a: string }` has members as an interface does.
            DefKind::TypeAlias => {
                let rec = &self.ix.defs[id];
                if self.ix.files[rec.file].children.contains_key(&rec.path) {
                    Entity::Class(id)
                } else {
                    Entity::Def(id)
                }
            }
            _ => Entity::Def(id),
        }
    }

    /// What a bare name means in a scope: the locals and the imports bound in
    /// the scopes around it, then the definitions of the scopes around it (a
    /// class's members are not among them), then the imports of the module.
    fn lookup_name(&self, f: usize, scope: &Scope, name: &Name, want_type: bool) -> Option<Named> {
        let file = &self.ix.files[f];
        for level in ancestors(scope) {
            if let Some(rec) = file
                .imports
                .iter()
                .rev()
                .find(|i| i.scope == *level && i.local == *name)
            {
                let hit = self.import_hit(f, rec);
                if !(matches!(level, Scope::Module) && self.yields_to_declaration(f, name, &hit)) {
                    return Some(Named::Hit(hit));
                }
            }
            let local = file
                .locals
                .get(name)
                .and_then(|list| list.iter().rposition(|l| l.scope == *level));
            if let Some(at) = local {
                return Some(Named::Local(at));
            }
        }
        let path = scope_path(scope);
        for depth in (0..=path.len()).rev() {
            let prefix = &path[..depth];
            if depth > 0 && !self.is_scope_container(f, prefix) {
                continue;
            }
            if depth == 0 {
                if let Some(rec) = file
                    .imports
                    .iter()
                    .rev()
                    .find(|i| matches!(i.scope, Scope::Module) && i.local == *name)
                {
                    let hit = self.import_hit(f, rec);
                    if !self.yields_to_declaration(f, name, &hit) {
                        return Some(Named::Hit(hit));
                    }
                }
            }
            if let Some(ids) = file.children.get(prefix) {
                let named: Vec<usize> = ids
                    .iter()
                    .copied()
                    .filter(|id| self.ix.defs[*id].def.local.name == *name)
                    .collect();
                if let Some(id) = self.ix.primary(&named, want_type) {
                    return Some(Named::Hit(Hit {
                        entity: self.entity_of_def(id),
                        kind: EvidenceKind::SameScope,
                        through: Vec::new(),
                    }));
                }
            }
        }
        None
    }

    /// `var path = require('path')` binds a variable of the file too: where the
    /// module is no file of the repository, the name is that variable.
    fn yields_to_declaration(&self, f: usize, name: &Name, hit: &Hit) -> bool {
        matches!(hit.entity, Entity::External | Entity::Unknown)
            && self.ix.files[f]
                .children
                .get(&Vec::new())
                .is_some_and(|ids| {
                    ids.iter()
                        .any(|id| self.ix.defs[*id].def.local.name == *name)
                })
    }

    fn import_hit(&self, f: usize, rec: &ImportRec) -> Hit {
        let mut through = Vec::new();
        let mut seen = Seen::new();
        let entity = self.import_entity(f, rec, 0, &mut through, &mut seen);
        Hit {
            entity,
            kind: if rec.kind == ImportKind::Dynamic {
                EvidenceKind::DynamicImport
            } else {
                EvidenceKind::ImportedName
            },
            through,
        }
    }

    fn import_entity(
        &self,
        f: usize,
        rec: &ImportRec,
        depth: usize,
        through: &mut Vec<usize>,
        seen: &mut Seen,
    ) -> Entity {
        let (module, name) = match &rec.spec {
            Spec::Module { module } => (module, None),
            Spec::Member { module, name } => (module, Some(name.clone())),
            Spec::Default { module } => (module, Some(Name::new("default"))),
        };
        match self.module(f, module) {
            ModRes::File(g) => match name {
                None => Entity::Module(g),
                Some(name) => self
                    .exported_member(g, &name, depth + 1, through, seen)
                    .or_else(|| {
                        // A CommonJS module with no default is its own default.
                        (name.as_str() == "default"
                            && !self.ix.files[g].exports.contains_key(&name))
                        .then_some(Entity::Module(g))
                    })
                    .unwrap_or(Entity::Unknown),
            },
            ModRes::External => Entity::External,
            ModRes::Outside => Entity::Outside,
            ModRes::Missing => Entity::Unknown,
            ModRes::Unplaced => match name {
                None => Entity::Unknown,
                Some(name) => self.maybe(f, &name),
            },
        }
    }

    /// The definitions a module nothing places may offer under `name`: those of
    /// that name any module offers, or the default exports.
    fn maybe(&self, f: usize, name: &Name) -> Entity {
        let pool: &[usize] = if name.as_str() == "default" {
            &self.ix.defaults
        } else {
            self.ix
                .public_named
                .get(name)
                .map_or(&[][..], Vec::as_slice)
        };
        let ids: Vec<usize> = pool
            .iter()
            .copied()
            .filter(|id| self.reachable(f, *id))
            .collect();
        if ids.is_empty() || ids.len() > MAX_CANDIDATES {
            Entity::Unknown
        } else {
            Entity::Maybe(ids)
        }
    }

    /// What module `g` offers under `name`: a declaration of its own, a name it
    /// takes from another module and offers on, a star re-export's, a namespace.
    /// `through` collects the modules the name passed through.
    fn exported_member(
        &self,
        g: usize,
        name: &Name,
        depth: usize,
        through: &mut Vec<usize>,
        seen: &mut Seen,
    ) -> Option<Entity> {
        if depth > MAX_HOPS || !seen.insert((g, name.clone())) {
            return None;
        }
        let file = &self.ix.files[g];
        if let Some(offer) = file.exports.get(name) {
            match offer {
                Offer::Local(local) => {
                    if let Some(entity) = self.module_name(g, local, depth + 1, through, seen) {
                        if let Entity::Def(id) = entity {
                            if let Some(handed) = self.wrapped_default(g, id) {
                                return Some(handed);
                            }
                        }
                        return Some(entity);
                    }
                }
                Offer::From {
                    module,
                    name: original,
                } => {
                    if let ModRes::File(h) = self.module(g, module) {
                        let mut hops = Vec::new();
                        if let Some(entity) =
                            self.exported_member(h, original, depth + 1, &mut hops, seen)
                        {
                            through.push(g);
                            through.extend(hops);
                            return Some(entity);
                        }
                    }
                }
                Offer::Namespace { module } => {
                    if let ModRes::File(h) = self.module(g, module) {
                        through.push(g);
                        return Some(Entity::Module(h));
                    }
                }
            }
        }
        if name.as_str() != "default" {
            for module in &file.stars {
                if let ModRes::File(h) = self.module(g, module) {
                    let mut hops = Vec::new();
                    if let Some(entity) = self.exported_member(h, name, depth + 1, &mut hops, seen)
                    {
                        through.push(g);
                        through.extend(hops);
                        return Some(entity);
                    }
                }
            }
        }
        None
    }

    /// A name of module `g` itself: what it imported, else what it declares.
    fn module_name(
        &self,
        g: usize,
        local: &Name,
        depth: usize,
        through: &mut Vec<usize>,
        seen: &mut Seen,
    ) -> Option<Entity> {
        let file = &self.ix.files[g];
        if let Some(rec) = file
            .imports
            .iter()
            .rev()
            .find(|i| matches!(i.scope, Scope::Module) && i.local == *local)
        {
            let mut hops = Vec::new();
            let entity = self.import_entity(g, rec, depth + 1, &mut hops, seen);
            if entity != Entity::Unknown {
                through.push(g);
                through.extend(hops);
                return Some(entity);
            }
        }
        let ids = file.children.get(&Vec::<Name>::new())?;
        let named: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|id| self.ix.defs[*id].def.local.name == *local)
            .collect();
        self.ix
            .primary(&named, false)
            .map(|id| self.entity_of_def(id))
    }

    // ---- receivers and chains

    /// The class a `this` in the scope is an instance of.
    fn class_of_scope(&self, f: usize, scope: &Scope) -> Option<usize> {
        let path = scope_path(scope);
        (1..path.len()).rev().find_map(|depth| {
            let id = self.ix.owner(f, &path[..depth])?;
            matches!(
                self.ix.defs[id].def.kind,
                DefKind::Class | DefKind::Interface | DefKind::Enum | DefKind::Object
            )
            .then_some(id)
        })
    }

    /// The scope a definition is written in, for resolving the names it states.
    fn container_scope(&self, rec: &DefRec) -> Scope {
        let container = &rec.def.local.container;
        if container.is_empty() {
            return Scope::Module;
        }
        match self.ix.owner(rec.file, container) {
            Some(id) => Scope::Def(self.ix.defs[id].def.local.clone()),
            None => Scope::Module,
        }
    }

    /// What the first name of a chain stands for.
    fn head_entity(
        &self,
        f: usize,
        scope: &Scope,
        head: &Name,
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        match head.as_str() {
            "this" => (
                self.class_of_scope(f, scope)
                    .map_or(Entity::Unknown, Entity::Instance),
                EvidenceKind::SelfReceiver,
            ),
            "super" => (
                self.class_of_scope(f, scope)
                    .map_or(Entity::Unknown, Entity::Super),
                EvidenceKind::SuperCall,
            ),
            _ => match self.lookup_name(f, scope, head, false) {
                Some(Named::Local(at)) => self.local_entity(f, scope, head, at, depth),
                Some(Named::Hit(hit)) => (hit.entity, hit.kind),
                None if is_global(head.as_str()) => (Entity::Builtin, EvidenceKind::SameScope),
                None => (Entity::Unknown, EvidenceKind::SameScope),
            },
        }
    }

    /// Where a reference's chain starts: the entity, how it is known, and the
    /// names written after it.
    fn chain_start<'r>(
        &self,
        f: usize,
        r: &'r Ref,
        depth: usize,
    ) -> (Entity, EvidenceKind, &'r [Name]) {
        let scope = &r.enclosing;
        let typed_shape = match r.qualifier.as_slice() {
            [only] => only.as_str() != "this",
            [first, _] => first.as_str() == "this",
            _ => false,
        };
        match &r.receiver {
            Receiver::New(t) => (
                self.type_name_entity(f, scope, t.as_str()),
                EvidenceKind::Constructed,
                &r.qualifier[..],
            ),
            Receiver::CallResult(chain) => {
                let (entity, kind) = self.call_result(f, scope, chain, depth);
                (entity, kind, &r.qualifier[..])
            }
            Receiver::SelfType if r.qualifier.is_empty() => (
                self.class_of_scope(f, scope)
                    .map_or(Entity::Unknown, Entity::Instance),
                EvidenceKind::SelfReceiver,
                &[],
            ),
            Receiver::Typed(t)
                if typed_shape
                    && self.type_name_entity(f, scope, t.as_str()) != Entity::Unknown =>
            {
                (
                    self.type_name_entity(f, scope, t.as_str()),
                    EvidenceKind::TypedReceiver,
                    &[],
                )
            }
            _ => match r.qualifier.split_first() {
                Some((head, rest)) => {
                    let (entity, kind) = self.head_entity(f, scope, head, depth);
                    (entity, kind, rest)
                }
                None => (Entity::Unknown, EvidenceKind::Constructed, &[]),
            },
        }
    }

    /// The entity before the last name of a reference.
    fn base_entity(&self, f: usize, r: &Ref, depth: usize) -> (Entity, EvidenceKind) {
        let (mut entity, kind, rest) = self.chain_start(f, r, depth);
        for name in rest {
            entity = self.step(f, &entity, name);
        }
        (entity, kind)
    }

    /// The members a chain passes through on its way to its last name are read:
    /// `o.softPrefs.length` reads `softPrefs`. Where nothing types the receiver the
    /// member is an untyped read like the chain's last name, and one an interface
    /// or an abstract class declares is a candidate: `error.request.url` reads some
    /// object's `request`, which may be the property a change removed. A member
    /// the receiver's class lacks is bound to nothing, as the last name is: the
    /// line a removal strands.
    fn chain_reads(&self, f: usize, r: &Ref) -> Vec<(Name, Resolution)> {
        let (mut entity, kind, rest) = self.chain_start(f, r, 0);
        let mut reads = Vec::new();
        let holds = |id: usize| {
            matches!(
                self.ix.defs[id].def.kind,
                DefKind::Field | DefKind::Property | DefKind::Variable | DefKind::Constant
            )
        };
        for name in rest {
            match self.member(f, &entity, name) {
                Lookup::Found {
                    entity: Entity::Def(id),
                    through,
                    inherited,
                } => {
                    if holds(id) && self.reachable(f, id) {
                        let kind = if inherited {
                            EvidenceKind::Inherited
                        } else {
                            kind
                        };
                        reads.push((
                            name.clone(),
                            Resolution::Resolved {
                                target: self.ix.defs[id].key.clone(),
                                evidence: Evidence::through(kind, self.paths(&through)),
                            },
                        ));
                    }
                }
                Lookup::Dispatch(ids) => {
                    let targets: Vec<DefKey> = ids
                        .iter()
                        .filter(|id| holds(**id) && self.reachable(f, **id))
                        .map(|id| self.ix.defs[*id].key.clone())
                        .collect();
                    if !targets.is_empty() {
                        reads.push((
                            name.clone(),
                            Resolution::Candidate {
                                targets,
                                reason: CandidateReason::UntypedReceiver,
                            },
                        ));
                    }
                }
                Lookup::Untyped | Lookup::Any => reads.push((
                    name.clone(),
                    Resolution::Unresolved {
                        reason: UnresolvedReason::UntypedRead,
                    },
                )),
                Lookup::Absent(reason) if reason.is_ours() => {
                    reads.push((name.clone(), Resolution::Unresolved { reason }))
                }
                Lookup::Found { .. } | Lookup::Absent(_) => {}
            }
            entity = self.step(f, &entity, name);
        }
        reads
    }

    /// The entity a member is, for the next name of a chain to start from.
    fn step(&self, f: usize, entity: &Entity, name: &Name) -> Entity {
        match self.member(f, entity, name) {
            Lookup::Found { entity, .. } => entity,
            Lookup::Absent(UnresolvedReason::External) => Entity::External,
            Lookup::Absent(UnresolvedReason::Builtin) => Entity::Builtin,
            Lookup::Absent(UnresolvedReason::OutsideProject) => Entity::Outside,
            Lookup::Any => Entity::Dynamic,
            Lookup::Absent(_) | Lookup::Untyped | Lookup::Dispatch(_) => Entity::Unknown,
        }
    }

    /// What `path` names, as a chain from the scope.
    fn path_entity(&self, f: usize, scope: &Scope, path: &[Name], depth: usize) -> Entity {
        let Some((head, rest)) = path.split_first() else {
            return Entity::Unknown;
        };
        let (mut entity, _) = self.head_entity(f, scope, head, depth);
        for name in rest {
            entity = self.step(f, &entity, name);
        }
        entity
    }

    /// The object a call makes: an instance of the class it constructs, or of the
    /// type a function declares it returns.
    fn call_result(
        &self,
        f: usize,
        scope: &Scope,
        chain: &[Name],
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        if depth >= MAX_DEPTH {
            return (Entity::Unknown, EvidenceKind::Constructed);
        }
        match self.path_entity(f, scope, chain, depth + 1) {
            Entity::Class(c) => (Entity::Instance(c), EvidenceKind::Constructed),
            Entity::Def(id) => (self.return_instance(id, false), EvidenceKind::FactoryReturn),
            _ => (Entity::Unknown, EvidenceKind::Constructed),
        }
    }

    /// A local name used as the head of a chain: its annotation, else the call it
    /// is assigned.
    fn local_entity(
        &self,
        f: usize,
        scope: &Scope,
        name: &Name,
        at: usize,
        depth: usize,
    ) -> (Entity, EvidenceKind) {
        let Some(rec) = self.ix.files[f].locals.get(name).and_then(|l| l.get(at)) else {
            return (Entity::Unknown, EvidenceKind::Constructed);
        };
        if rec.kind == LocalKind::Closure {
            return (Entity::Unknown, EvidenceKind::Constructed);
        }
        if let Some(text) = self.text(f) {
            if let Some(written) = annotation_after(&text, rec.site.bytes.end as usize) {
                let typed = self.type_expr(f, scope, &written);
                if typed != Entity::Unknown {
                    return (typed, EvidenceKind::TypedReceiver);
                }
            }
        }
        if rec.kind == LocalKind::Variable {
            if let Some(found) = self.assigned_call(f, rec.fact, rec.site.bytes.end, depth) {
                return found;
            }
            if let Some(found) = self.assigned_path(f, &rec.scope, &rec.site, depth) {
                return found;
            }
        }
        (Entity::Unknown, EvidenceKind::Constructed)
    }

    /// What `const x = a.b.c [as T]` binds, when `x` is a plain declarator of the
    /// statement: the type the assertion states, else what the chain holds
    /// (`const internals = this._internals`). A default inside a pattern, a
    /// chain that goes on into a call or an operator, and a chain nothing types
    /// bind nothing.
    fn assigned_path(
        &self,
        f: usize,
        scope: &Scope,
        site: &Span,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let text = self.text(f)?;
        let mut before = site.bytes.start as usize;
        while before > 0 && matches!(text.get(before - 1), Some(b' ' | b'\t')) {
            before -= 1;
        }
        let keyword_end = before;
        while before > 0 && text.get(before - 1).is_some_and(u8::is_ascii_alphabetic) {
            before -= 1;
        }
        let keyword = text.get(before..keyword_end)?;
        if !(keyword == b"const" || keyword == b"let" || keyword == b"var") {
            return None;
        }
        let mut at = skip_blanks(&text, site.bytes.end as usize);
        if text.get(at) != Some(&b'=') || matches!(text.get(at + 1), Some(b'=' | b'>')) {
            return None;
        }
        at = skip_blanks(&text, at + 1);
        let word_byte = |b: u8| is_ident_byte(b) || b == b'#';
        let mut names: Vec<Name> = Vec::new();
        loop {
            let from = at;
            while text.get(at).is_some_and(|b| word_byte(*b)) {
                at += 1;
            }
            if at == from {
                return None;
            }
            #[allow(
                clippy::disallowed_methods,
                reason = "a name that is not UTF-8 is not read as a chain; nothing is inferred from it"
            )]
            let word = std::str::from_utf8(text.get(from..at)?).ok()?;
            names.push(Name::new(word));
            let mut next = at;
            if text.get(next) == Some(&b'!') {
                next += 1;
            }
            if text.get(next) == Some(&b'?') && text.get(next + 1) == Some(&b'.') {
                at = next + 2;
            } else if text.get(next) == Some(&b'.') {
                at = next + 1;
            } else {
                at = next;
                break;
            }
        }
        let mut end = at;
        while matches!(text.get(end), Some(b' ' | b'\t')) {
            end += 1;
        }
        let asserted = text
            .get(end..)
            .is_some_and(|rest| rest.starts_with(b"as ") || rest.starts_with(b"as\t"));
        if asserted {
            let written = written_type(&text, end + 3)?;
            return match self.type_expr(f, scope, &written) {
                Entity::Unknown => None,
                typed => Some((typed, EvidenceKind::TypedReceiver)),
            };
        }
        if !expression_ends(&text, at) {
            return None;
        }
        match self.path_entity(f, scope, &names, depth + 1) {
            Entity::Unknown => None,
            held => Some((held, EvidenceKind::Aliased)),
        }
    }

    /// What `const {key: name} = Class` binds `name` to: the member `key` of the
    /// class, when the declaration is the whole statement and the source is a
    /// class of the repository (a static method kept under a short name).
    fn destructured_alias(&self, id: usize) -> Option<Entity> {
        let rec = &self.ix.defs[id];
        if !matches!(rec.def.kind, DefKind::Constant | DefKind::Variable) {
            return None;
        }
        let text = self.text(rec.file)?;
        let (start, name_at) = (
            rec.def.span.bytes.start as usize,
            rec.def.name_span.bytes.start as usize,
        );
        let open = start + text.get(start..name_at)?.iter().position(|b| *b == b'{')?;
        let close = group_end(&text, open)?;
        if close <= name_at {
            return None;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "a pattern that is not UTF-8 is not read; the name stays the variable it is"
        )]
        let pattern = std::str::from_utf8(text.get(open + 1..close - 1)?).ok()?;
        let local = rec.def.local.name.as_str();
        let key = split_top(pattern, ',').into_iter().find_map(|entry| {
            let bound = entry.split('=').next().unwrap_or(entry);
            match bound.split_once(':') {
                Some((key, value)) => (value.trim() == local).then(|| key.trim()),
                None => (bound.trim() == local).then(|| bound.trim()),
            }
        })?;
        let mut at = skip_blanks(&text, close);
        if text.get(at) != Some(&b'=') || matches!(text.get(at + 1), Some(b'=' | b'>')) {
            return None;
        }
        at = skip_blanks(&text, at + 1);
        let from = at;
        while text
            .get(at)
            .is_some_and(|b| is_ident_byte(*b) || *b == b'.')
        {
            at += 1;
        }
        if !expression_ends(&text, at) {
            return None;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "a source that is not UTF-8 is not read; the name stays the variable it is"
        )]
        let source = std::str::from_utf8(text.get(from..at)?).ok()?;
        if !is_dotted(source) {
            return None;
        }
        let Entity::Class(class) =
            self.resolve_dotted(rec.file, &self.container_scope(rec), source, false)
        else {
            return None;
        };
        match self.class_member(class, &Name::new(key), false, false) {
            Lookup::Found { entity, .. } => Some(entity),
            _ => None,
        }
    }

    /// The object made by the call that is the fact right after the binding at
    /// `fact`, when the text from `after` to it is `= [await] [new] chain.` and
    /// the call ends the statement.
    fn assigned_call(
        &self,
        f: usize,
        fact: usize,
        after: u32,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        if depth >= MAX_DEPTH {
            return None;
        }
        let facts = &self.ix.files[f].facts.blob.facts;
        let Fact::Reference(callee) = facts.get(fact + 1)? else {
            return None;
        };
        if !matches!(callee.use_kind, UseKind::Call | UseKind::Instantiate) {
            return None;
        }
        let text = self.text(f)?;
        #[allow(
            clippy::disallowed_methods,
            reason = "the slice is read for an assignment shape; bytes that are not UTF-8 show none, so nothing is inferred from them"
        )]
        let between =
            std::str::from_utf8(text.get(after as usize..callee.site.bytes.start as usize)?)
                .ok()?;
        let after_equals = between.trim_start().strip_prefix('=')?;
        if after_equals.starts_with('=') {
            return None;
        }
        let mut rest = after_equals.trim_start();
        let mut awaited = false;
        if let Some(tail) = strip_word(rest, "await") {
            rest = tail.trim_start();
            awaited = true;
        }
        if callee.use_kind == UseKind::Instantiate {
            rest = strip_word(rest, "new")?.trim_start();
        }
        let written: String = rest.split_whitespace().collect();
        let expected: String = callee.qualifier.iter().map(|q| format!("{q}.")).collect();
        if written.replace("?.", ".") != expected {
            return None;
        }
        let open = call_open(&text, callee.site.bytes.end as usize)?;
        if !expression_ends(&text, group_end(&text, open)?) {
            return None;
        }
        self.callee_result(f, callee, awaited, depth)
    }

    /// What calling or constructing `callee` makes: the language's own object, an
    /// object of a package outside the repository, an instance of a class of the
    /// repository, or what a function declares it returns.
    fn callee_result(
        &self,
        f: usize,
        callee: &Ref,
        awaited: bool,
        depth: usize,
    ) -> Option<(Entity, EvidenceKind)> {
        match self.outcome(f, callee, depth + 1) {
            Outcome::Unresolved(UnresolvedReason::Builtin)
            | Outcome::Found {
                entity: Entity::Builtin,
                ..
            } => Some((Entity::Builtin, EvidenceKind::Constructed)),
            Outcome::Unresolved(UnresolvedReason::External)
            | Outcome::Found {
                entity: Entity::External,
                ..
            } => Some((Entity::External, EvidenceKind::Constructed)),
            Outcome::Found {
                entity: Entity::Class(c),
                ..
            } => Some((Entity::Instance(c), EvidenceKind::Constructed)),
            Outcome::Found {
                entity: Entity::Def(id),
                ..
            } => match self.return_instance(id, awaited) {
                Entity::Unknown => None,
                entity => Some((entity, EvidenceKind::FactoryReturn)),
            },
            _ => None,
        }
    }

    /// The instance of the type a function's signature declares it returns.
    fn return_instance(&self, id: usize, awaited: bool) -> Entity {
        let rec = &self.ix.defs[id];
        let Some(mut written) = rec.def.signature.as_deref().and_then(return_type) else {
            return Entity::Unknown;
        };
        if awaited {
            if let Some(inner) = written
                .strip_prefix("Promise<")
                .and_then(|rest| rest.strip_suffix('>'))
            {
                written = inner.to_string();
            }
        }
        self.type_expr(rec.file, &self.container_scope(rec), &written)
    }

    var_instance_memo!();

    fn compute_var_instance(&self, id: usize) -> Entity {
        let rec = &self.ix.defs[id];
        let scope = self.container_scope(rec);
        if let Some(signature) = rec.def.signature.as_deref() {
            let declared = declared_type(signature).or_else(|| {
                (rec.def.kind == DefKind::Property)
                    .then(|| return_type(signature))
                    .flatten()
            });
            if let Some(written) = declared {
                let typed = self.type_expr(rec.file, &scope, &written);
                if typed != Entity::Unknown {
                    return typed;
                }
            }
        }
        match self.assigned_call(rec.file, rec.fact, rec.def.name_span.bytes.end, 0) {
            Some((entity, _)) => entity,
            None => self
                .assigned_path(rec.file, &scope, &rec.def.name_span, 0)
                .map_or(Entity::Unknown, |(entity, _)| entity),
        }
    }

    /// The instance a type written in code stands for.
    fn type_expr(&self, f: usize, scope: &Scope, written: &str) -> Entity {
        let parts: Vec<&str> = split_top(written, '|')
            .into_iter()
            .map(str::trim)
            .filter(|p| !matches!(*p, "" | "null" | "undefined"))
            .collect();
        let [only] = parts.as_slice() else {
            return Entity::Unknown;
        };
        let text = only.strip_prefix("readonly ").unwrap_or(only).trim();
        match text {
            "any" => return Entity::Dynamic,
            "string" | "number" | "boolean" | "bigint" | "symbol" | "void" | "never" => {
                return Entity::Builtin;
            }
            "unknown" | "object" => return Entity::Unknown,
            _ => {}
        }
        if text.ends_with("[]") {
            return Entity::Builtin;
        }
        let head = text.split('<').next().unwrap_or(text).trim();
        if !is_dotted(head) {
            return Entity::Unknown;
        }
        self.type_name_entity(f, scope, head)
    }

    /// The instance of the class or interface a type name stands for.
    fn type_name_entity(&self, f: usize, scope: &Scope, dotted: &str) -> Entity {
        match self.resolve_dotted(f, scope, dotted, true) {
            Entity::Class(c) => Entity::Instance(c),
            Entity::Builtin => Entity::Builtin,
            Entity::External => Entity::External,
            _ => Entity::Unknown,
        }
    }

    fn resolve_dotted(&self, f: usize, scope: &Scope, dotted: &str, want_type: bool) -> Entity {
        let mut parts = dotted.split('.');
        let Some(head) = parts.next() else {
            return Entity::Unknown;
        };
        let head = Name::new(head);
        let mut entity = match self.lookup_name(f, scope, &head, want_type) {
            Some(Named::Hit(hit)) => hit.entity,
            Some(Named::Local(_)) => Entity::Unknown,
            None if is_global(head.as_str()) => Entity::Builtin,
            None => Entity::Unknown,
        };
        for part in parts {
            entity = self.step(f, &entity, &Name::new(part));
        }
        entity
    }

    // ---- members

    fn member(&self, f: usize, entity: &Entity, name: &Name) -> Lookup {
        match entity {
            Entity::Module(g) => {
                let mut through = Vec::new();
                let mut seen = Seen::new();
                if let Some(found) = self.exported_member(*g, name, 0, &mut through, &mut seen) {
                    return Lookup::Found {
                        entity: found,
                        through,
                        inherited: false,
                    };
                }
                // `module.exports = Class` is also the module: its members.
                let mut hops = Vec::new();
                let default = Name::new("default");
                match self.exported_member(*g, &default, 0, &mut hops, &mut Seen::new()) {
                    Some(held @ (Entity::Class(_) | Entity::Def(_))) => self.member(f, &held, name),
                    _ => Lookup::Absent(UnresolvedReason::NoBinding),
                }
            }
            Entity::Class(c) => self.class_member(*c, name, false, false),
            Entity::Instance(c) => {
                if name.as_str() == "constructor" {
                    return Lookup::Found {
                        entity: Entity::Meta(*c),
                        through: Vec::new(),
                        inherited: false,
                    };
                }
                self.class_member(*c, name, false, true)
            }
            Entity::Meta(c) => {
                let own = self.class_member(*c, name, false, false);
                let Lookup::Found {
                    entity: Entity::Class(found) | Entity::Def(found),
                    ..
                } = own
                else {
                    return own;
                };
                let mut ids = vec![found];
                for sub in self.subtypes_of(*c) {
                    ids.extend(self.own_members(sub, name));
                }
                if ids.len() > 1 {
                    Lookup::Dispatch(ids)
                } else {
                    own
                }
            }
            Entity::Super(c) => self.class_member(*c, name, true, false),
            Entity::Def(id) => match self.ix.defs[*id].def.kind {
                DefKind::Variable | DefKind::Constant | DefKind::Field | DefKind::Property => {
                    // The methods of the object literal it is initialised with.
                    if let Some(own) = self.own_members(*id, name).first() {
                        return Lookup::Found {
                            entity: self.entity_of_def(*own),
                            through: Vec::new(),
                            inherited: false,
                        };
                    }
                    match self.var_instance(*id) {
                        Entity::Unknown => Lookup::Untyped,
                        held => self.member(f, &held, name),
                    }
                }
                DefKind::Function | DefKind::Method | DefKind::Constructor => {
                    Lookup::Absent(UnresolvedReason::Builtin)
                }
                _ => Lookup::Untyped,
            },
            Entity::Dynamic => Lookup::Any,
            Entity::Maybe(_) => Lookup::Untyped,
            Entity::External => Lookup::Absent(UnresolvedReason::External),
            Entity::Builtin => Lookup::Absent(UnresolvedReason::Builtin),
            Entity::Outside => Lookup::Absent(UnresolvedReason::OutsideProject),
            Entity::Unknown => Lookup::Untyped,
        }
    }

    fn class_member(&self, class: usize, name: &Name, bases_only: bool, instance: bool) -> Lookup {
        let (defs, opaque) = self.class_members(class, name, bases_only);
        match self.ix.primary(&defs, false) {
            Some(id) => {
                if instance && !bases_only {
                    if let Some(ids) = self.dispatch(class, name, id) {
                        return Lookup::Dispatch(ids);
                    }
                }
                let found = &self.ix.defs[id];
                let owner = &self.ix.defs[class];
                let inherited = !bases_only
                    && (found.file != owner.file
                        || found.path[..found.path.len() - 1] != owner.path[..]);
                Lookup::Found {
                    entity: self.entity_of_def(id),
                    through: Vec::new(),
                    inherited,
                }
            }
            None if opaque => Lookup::Absent(UnresolvedReason::ExternalBase),
            None => Lookup::Absent(UnresolvedReason::NoMember),
        }
    }

    /// The members called `name` of the first class, in breadth-first order of
    /// what it extends, that has any; and whether a base the repository does not
    /// hold stood in the way.
    fn class_members(&self, class: usize, name: &Name, bases_only: bool) -> (Vec<usize>, bool) {
        let mut queue = VecDeque::from([class]);
        let mut seen = HashSet::new();
        let mut opaque = false;
        let mut first = true;
        while let Some(current) = queue.pop_front() {
            if !seen.insert(current) {
                continue;
            }
            if !(first && bases_only) {
                let own = self.own_members(current, name);
                if !own.is_empty() {
                    return (own, opaque);
                }
            }
            first = false;
            for (base, implements) in self.bases(current).iter() {
                if *implements {
                    continue;
                }
                match base {
                    Entity::Class(b) => queue.push_back(*b),
                    Entity::Builtin => {}
                    _ => opaque = true,
                }
            }
        }
        (Vec::new(), opaque)
    }

    fn own_members(&self, class: usize, name: &Name) -> Vec<usize> {
        let rec = &self.ix.defs[class];
        let Some(ids) = self.ix.files[rec.file].children.get(&rec.path) else {
            return Vec::new();
        };
        let named: Vec<usize> = ids
            .iter()
            .copied()
            .filter(|id| self.ix.defs[*id].def.local.name == *name)
            .collect();
        self.ix.primary(&named, false).into_iter().collect()
    }

    /// What a class or interface extends and implements, as written in its
    /// header, each resolved from the scope the class is written in; one that is
    /// no class of the repository is `External`, `Builtin` or `Unknown`.
    fn bases(&self, class: usize) -> Rc<Vec<(Entity, bool)>> {
        if let Some(known) = self.bases.borrow().get(&class) {
            return Rc::clone(known);
        }
        self.bases.borrow_mut().insert(class, Rc::new(Vec::new()));
        let rec = &self.ix.defs[class];
        let scope = self.container_scope(rec);
        let interface = rec.def.kind == DefKind::Interface;
        #[allow(
            clippy::disallowed_methods,
            reason = "a definition without a signature lists no heritage"
        )]
        let entities: Vec<(Entity, bool)> = rec
            .def
            .signature
            .as_deref()
            .map(heritage_names)
            .unwrap_or_default()
            .iter()
            .map(|(implements, text)| {
                (
                    self.resolve_dotted(rec.file, &scope, text, interface || *implements),
                    *implements,
                )
            })
            .collect();
        let entities = Rc::new(entities);
        self.bases.borrow_mut().insert(class, Rc::clone(&entities));
        entities
    }

    /// The classes and interfaces that extend or implement `base`, at any depth.
    fn subtypes_of(&self, base: usize) -> Vec<usize> {
        let direct = self.subtype_map.get_or_init(|| {
            let mut map: HashMap<usize, Vec<usize>> = HashMap::new();
            for (id, rec) in self.ix.defs.iter().enumerate() {
                if !matches!(rec.def.kind, DefKind::Class | DefKind::Interface) {
                    continue;
                }
                for (parent, _) in self.bases(id).iter() {
                    if let Entity::Class(b) = parent {
                        map.entry(*b).or_default().push(id);
                    }
                }
            }
            map
        });
        let mut found = Vec::new();
        let mut seen = HashSet::from([base]);
        let mut queue = VecDeque::from([base]);
        while let Some(current) = queue.pop_front() {
            for sub in direct.get(&current).into_iter().flatten() {
                if seen.insert(*sub) {
                    found.push(*sub);
                    queue.push_back(*sub);
                }
            }
        }
        found
    }

    /// A member an interface declares, or a class leaves abstract, is run by one
    /// of the types that implement it: the declaration and every such member of
    /// the subtypes of the receiver's type.
    fn dispatch(&self, class: usize, name: &Name, found: usize) -> Option<Vec<usize>> {
        let rec = &self.ix.defs[found];
        // Only what runs is dispatched: a method, or a property holding a
        // function. A data property of an interface is read as declared.
        let runs = match rec.def.kind {
            DefKind::Method => true,
            DefKind::Property => rec
                .def
                .signature
                .as_deref()
                .and_then(declared_type)
                .is_some_and(|written| written.starts_with('(')),
            _ => false,
        };
        if !runs {
            return None;
        }
        let holder = self
            .ix
            .owner(rec.file, rec.path.get(..rec.path.len().checked_sub(1)?)?)?;
        if self.ix.defs[holder].def.kind != DefKind::Interface
            && !is_abstract(rec.def.signature.as_deref())
        {
            return None;
        }
        let mut ids = vec![found];
        for base in [class, holder] {
            for sub in self.subtypes_of(base) {
                ids.extend(self.own_members(sub, name));
            }
        }
        let mut seen = HashSet::new();
        ids.retain(|id| seen.insert(*id));
        // A property holding a function, which nothing replaces, is announced by
        // its type and run by the object that holds it: a call of it is a call of
        // the declaration. A method of an interface is announced for classes to
        // implement, and stays a candidate where it stands alone.
        (ids.len() > 1 || rec.def.kind == DefKind::Method).then_some(ids)
    }

    /// The files the file imports names or modules from, and the files those
    /// names passed through.
    fn imported_files(&self, f: usize) -> Rc<HashSet<usize>> {
        if let Some(known) = self.imported.borrow().get(&f) {
            return Rc::clone(known);
        }
        let file = &self.ix.files[f];
        let mut files = HashSet::new();
        for fact in &file.facts.blob.facts {
            if let Fact::Import(import) = fact {
                if let ModRes::File(g) = self.module(f, import.module.as_str()) {
                    files.insert(g);
                }
            }
        }
        for rec in &file.imports {
            let (module, name) = match &rec.spec {
                Spec::Member { module, name } => (module, name.clone()),
                Spec::Default { module } => (module, Name::new("default")),
                Spec::Module { .. } => continue,
            };
            if let ModRes::File(g) = self.module(f, module) {
                let mut through = Vec::new();
                let mut seen = Seen::new();
                let found = self.exported_member(g, &name, 0, &mut through, &mut seen);
                files.extend(through);
                if let Some(Entity::Class(id) | Entity::Def(id)) = found {
                    files.insert(self.ix.defs[id].file);
                }
            }
        }
        let files = Rc::new(files);
        self.imported.borrow_mut().insert(f, Rc::clone(&files));
        files
    }

    // ---- globals, scripts and globs

    /// The file an `importScripts` names: from the repository root when it starts
    /// with `/`, else from the script's own directory.
    fn script_file(&self, f: usize, written: &str) -> Option<usize> {
        let written = written.split('?').next().unwrap_or(written);
        let base = match written.strip_prefix('/') {
            Some(root) => normalize_lexical(root)?,
            None => normalize_lexical(&join(&self.ix.files[f].dir, written))?,
        };
        self.probe(&base)
    }

    /// What a classic script or a worker names without defining it: a worker
    /// reads the scripts it loads and no others; a classic script that loads
    /// none may mean any script's.
    fn global_outcome(&self, f: usize, name: &Name) -> Option<Outcome> {
        let file = &self.ix.files[f];
        if !file.scripts.is_empty() {
            let mut loaded: Vec<usize> = Vec::new();
            for written in &file.scripts {
                if let Some(g) = self.script_file(f, written) {
                    for id in self.ix.files[g]
                        .children
                        .get(&Vec::new())
                        .into_iter()
                        .flatten()
                    {
                        if self.ix.defs[*id].def.local.name == *name {
                            loaded.push(*id);
                        }
                    }
                }
            }
            return match loaded.as_slice() {
                [] => None,
                [one] => Some(Outcome::Found {
                    entity: self.entity_of_def(*one),
                    evidence: Evidence::through(
                        EvidenceKind::ImportedName,
                        vec![self.path_of(self.ix.defs[*one].file).clone()],
                    ),
                }),
                _ => Some(Outcome::Candidates {
                    ids: loaded,
                    reason: CandidateReason::Global,
                }),
            };
        }
        if !file.classic {
            return None;
        }
        let ids: Vec<usize> = self
            .ix
            .globals
            .get(name)?
            .iter()
            .copied()
            .filter(|id| self.ix.defs[*id].file != f && self.reachable(f, *id))
            .collect();
        match ids.len() {
            0 => None,
            n if n > MAX_CANDIDATES => {
                Some(Outcome::Unresolved(UnresolvedReason::TooManyCandidates))
            }
            _ => Some(Outcome::Candidates {
                ids,
                reason: CandidateReason::Global,
            }),
        }
    }

    /// The top-level definitions of file `g` that go by `name`.
    fn offered_named(&self, g: usize, name: &Name) -> Vec<usize> {
        self.ix.files[g]
            .children
            .get(&Vec::new())
            .into_iter()
            .flatten()
            .copied()
            .filter(|id| {
                let def = &self.ix.defs[*id].def;
                def.local.name == *name && def.visibility == Visibility::Public
            })
            .collect()
    }

    /// The glob of each `import.meta.glob(...)` and `require.context(...)` of the
    /// file, with the files its patterns match.
    fn glob_sets(&self, f: usize) -> Rc<Vec<GlobSet>> {
        if let Some(known) = self.globs.borrow().get(&f) {
            return Rc::clone(known);
        }
        let file = &self.ix.files[f];
        let mut sites: Vec<(Span, Scope, Vec<&str>)> = Vec::new();
        for (pattern, site, scope) in &file.globs {
            match sites.iter_mut().find(|(s, _, _)| s.bytes == site.bytes) {
                Some((_, _, list)) => list.push(pattern),
                None => sites.push((site.clone(), scope.clone(), vec![pattern])),
            }
        }
        let anchored = |pattern: &str| match pattern.strip_prefix('/') {
            Some(root) => normalize_lexical(root),
            None => normalize_lexical(&join(&file.dir, pattern)),
        };
        let mut sets = Vec::new();
        for (site, scope, patterns) in sites {
            let positive: Vec<String> = patterns
                .iter()
                .filter(|p| !p.starts_with('!'))
                .filter_map(|p| anchored(p))
                .collect();
            let negative: Vec<String> = patterns
                .iter()
                .filter_map(|p| p.strip_prefix('!'))
                .filter_map(anchored)
                .collect();
            let mut files: Vec<usize> = self
                .ix
                .by_file
                .iter()
                .filter(|(path, g)| {
                    **g != f
                        && positive.iter().any(|p| glob_match(p, path))
                        && !negative.iter().any(|n| glob_match(n, path))
                        && self.reaches(f, **g)
                })
                .map(|(_, g)| *g)
                .collect();
            files.sort_unstable();
            if let Some(label) = patterns.iter().find(|p| !p.starts_with('!')) {
                sets.push(GlobSet {
                    site,
                    scope,
                    label: (*label).to_string(),
                    files,
                });
            }
        }
        let sets = Rc::new(sets);
        self.globs.borrow_mut().insert(f, Rc::clone(&sets));
        sets
    }

    /// Every file the file's globs load.
    fn globbed(&self, f: usize) -> Vec<usize> {
        let mut all: Vec<usize> = self
            .glob_sets(f)
            .iter()
            .flat_map(|set| set.files.iter().copied())
            .collect();
        all.sort_unstable();
        all.dedup();
        all
    }

    /// The module a default-exported wrapper hands on: `export default memo(Badge)`
    /// is `Badge`, the one name the call is given.
    fn wrapped_default(&self, g: usize, id: usize) -> Option<Entity> {
        if let Some(known) = self.wrapped.borrow().get(&id) {
            return known.clone();
        }
        let rec = &self.ix.defs[id];
        let found = (rec.def.local.name.as_str() == "default"
            && matches!(rec.def.kind, DefKind::Variable | DefKind::Constant))
        .then(|| {
            let scope = Scope::Def(rec.def.local.clone());
            let names: Vec<&Name> = self.ix.files[g]
                .facts
                .blob
                .facts
                .iter()
                .filter_map(|fact| match fact {
                    Fact::Reference(r)
                        if r.enclosing == scope
                            && r.use_kind == UseKind::PassAsValue
                            && r.qualifier.is_empty() =>
                    {
                        Some(&r.name)
                    }
                    _ => None,
                })
                .collect();
            match names.as_slice() {
                [only] => self
                    .module_name(g, only, 0, &mut Vec::new(), &mut Seen::new())
                    .filter(|e| match e {
                        Entity::Class(_) => true,
                        Entity::Def(w) => matches!(
                            self.ix.defs[*w].def.kind,
                            DefKind::Function | DefKind::Constant | DefKind::Variable
                        ),
                        _ => false,
                    }),
                _ => None,
            }
        })
        .flatten();
        self.wrapped.borrow_mut().insert(id, found.clone());
        found
    }

    // ---- references

    fn reference(&self, f: usize, r: &Ref, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let outcome = self.outcome(f, r, 0);
        let jsx = self.is_jsx_tag(f, r);
        let (relation, resolution) = self.finish(f, r.use_kind, jsx, outcome);
        let binding = |name: &Name, relation: Relation, resolution: Resolution| Binding {
            file: file.facts.path.clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation,
            name: name.clone(),
            resolution,
            entry_guard: false,
        };
        out.push(binding(&r.name, relation, resolution));
        // The facts keep the head of a chain out of the references: `limits.max`
        // is the name `max` with `limits` written before it, and reading `limits`
        // is part of it. The head's own binding is made here, at the chain's line.
        if let Some((head, resolution)) = self.head_read(f, r) {
            out.push(binding(&head, Relation::Reads, resolution));
        }
        if r.qualifier.len() > 1 || matches!(r.receiver, Receiver::CallResult(_) | Receiver::New(_))
        {
            for (name, resolution) in self.chain_reads(f, r) {
                out.push(binding(&name, Relation::Reads, resolution));
            }
        }
    }

    /// The variable, constant, field, class or enum a chain starts from, when it
    /// is one the repository defines and not a local or a module.
    fn head_read(&self, f: usize, r: &Ref) -> Option<(Name, Resolution)> {
        let head = r.qualifier.first()?;
        if matches!(head.as_str(), "this" | "super") {
            return None;
        }
        let Some(Named::Hit(hit)) = self.lookup_name(f, &r.enclosing, head, false) else {
            return None;
        };
        let (Entity::Class(id) | Entity::Def(id)) = hit.entity else {
            return None;
        };
        let reads = matches!(
            self.ix.defs[id].def.kind,
            DefKind::Class
                | DefKind::Enum
                | DefKind::Function
                | DefKind::Variable
                | DefKind::Constant
                | DefKind::Field
        );
        if !reads || !self.reachable(f, id) {
            return None;
        }
        Some((
            head.clone(),
            Resolution::Resolved {
                target: self.ix.defs[id].key.clone(),
                evidence: Evidence::through(hit.kind, self.paths(&hit.through)),
            },
        ))
    }

    /// Whether the call is the name of a JSX element: the name, or the chain it
    /// ends, stands right after a `<`.
    fn is_jsx_tag(&self, f: usize, r: &Ref) -> bool {
        if r.use_kind != UseKind::Call {
            return false;
        }
        let Some(text) = self.text(f) else {
            return false;
        };
        let start = r.site.bytes.start as usize;
        let mut at = start;
        while at > 0 && (is_ident_byte(text[at - 1]) || text[at - 1] == b'.') {
            at -= 1;
        }
        let member = text.get(at..start).is_some_and(|tag| tag.contains(&b'.'));
        at > 0
            && text[at - 1] == b'<'
            && (member || text.get(at).is_some_and(u8::is_ascii_uppercase))
    }

    fn outcome(&self, f: usize, r: &Ref, depth: usize) -> Outcome {
        let want_type = r.use_kind == UseKind::TypeUse;
        if r.qualifier.is_empty() && matches!(r.receiver, Receiver::None) {
            if r.name.as_str() == "super" {
                return self.super_call(f, &r.enclosing);
            }
            return match self.lookup_name(f, &r.enclosing, &r.name, want_type) {
                Some(Named::Local(_)) => Outcome::Unresolved(UnresolvedReason::Local),
                Some(Named::Hit(hit)) => {
                    let alias = match hit.entity {
                        Entity::Def(id) => self.destructured_alias(id),
                        _ => None,
                    };
                    match alias {
                        Some(member) => Outcome::Found {
                            entity: member,
                            evidence: Evidence::through(
                                EvidenceKind::Aliased,
                                self.paths(&hit.through),
                            ),
                        },
                        None => Outcome::Found {
                            entity: hit.entity,
                            evidence: Evidence::through(hit.kind, self.paths(&hit.through)),
                        },
                    }
                }
                None => match self.global_outcome(f, &r.name) {
                    Some(found) => found,
                    None if is_global(r.name.as_str())
                        || (want_type && looks_like_type_parameter(r.name.as_str())) =>
                    {
                        Outcome::Unresolved(UnresolvedReason::Builtin)
                    }
                    None => Outcome::Unresolved(UnresolvedReason::NoBinding),
                },
            };
        }
        // `self.serveShell`, `window.x`, `globalThis.x`: the global object's
        // property, set by a classic script.
        if let [head] = r.qualifier.as_slice() {
            if is_global_object(head.as_str())
                && self.lookup_name(f, &r.enclosing, head, false).is_none()
            {
                if let Some(found) = self.global_outcome(f, &r.name) {
                    return found;
                }
            }
        }
        if r.qualifier.is_empty() && matches!(r.receiver, Receiver::Unknown) {
            return Outcome::Unresolved(UnresolvedReason::Opaque);
        }
        if let (true, Receiver::Typed(written)) = (r.qualifier.is_empty(), &r.receiver) {
            return self.literal_key(f, r, written.as_str());
        }
        if r.qualifier
            .first()
            .is_some_and(|marker| marker.as_str().starts_with(ARGUMENT))
        {
            return self.argument_key(f, r);
        }
        let (base, kind) = self.base_entity(f, r, depth);
        let via_module = matches!(base, Entity::Module(_));
        match self.member(f, &base, &r.name) {
            Lookup::Found {
                entity,
                through,
                inherited,
            } => {
                let kind = if inherited {
                    EvidenceKind::Inherited
                } else if via_module {
                    EvidenceKind::ModuleAttribute
                } else {
                    kind
                };
                Outcome::Found {
                    entity,
                    evidence: Evidence::through(kind, self.paths(&through)),
                }
            }
            Lookup::Dispatch(ids) => Outcome::Candidates {
                ids,
                reason: CandidateReason::UntypedReceiver,
            },
            Lookup::Absent(reason) => Outcome::Unresolved(reason),
            Lookup::Untyped => self.untyped(f, r, false),
            Lookup::Any => self.untyped(f, r, true),
        }
    }

    /// The key of an object literal the code types, a member of that type: the
    /// facts state it as a name with no qualifier and the type the literal is
    /// declared as. A type the repository does not hold, or one no member of
    /// which has the name, claims nothing and is no unknown.
    fn literal_key(&self, f: usize, r: &Ref, written: &str) -> Outcome {
        self.member_of_type(self.type_name_entity(f, &r.enclosing, written), &r.name)
    }

    /// The member `name` of the type an entity stands for: an instance of a class
    /// or interface of the repository has it or has not; a type the repository
    /// does not hold claims nothing.
    fn member_of_type(&self, typed: Entity, name: &Name) -> Outcome {
        match typed {
            Entity::Instance(class) => match self.class_member(class, name, false, false) {
                Lookup::Found {
                    entity,
                    through,
                    inherited,
                } => Outcome::Found {
                    entity,
                    evidence: Evidence::through(
                        if inherited {
                            EvidenceKind::Inherited
                        } else {
                            EvidenceKind::TypedReceiver
                        },
                        self.paths(&through),
                    ),
                },
                Lookup::Absent(reason) => Outcome::Unresolved(reason),
                _ => Outcome::Unresolved(UnresolvedReason::NoMember),
            },
            Entity::External => Outcome::Unresolved(UnresolvedReason::External),
            _ => Outcome::Unresolved(UnresolvedReason::Builtin),
        }
    }

    /// The key of an object literal handed to a call as one argument: a member of
    /// the type the callee declares for that parameter. A callee the code does not
    /// type, or one whose parameter is written no way a name can be read from,
    /// leaves the write unresolved and unknown: it may be a member of any type,
    /// and the answer says so.
    fn argument_key(&self, f: usize, r: &Ref) -> Outcome {
        let untyped = Outcome::Unresolved(UnresolvedReason::UntypedRead);
        let Some((marker, callee)) = r.qualifier.split_first() else {
            return untyped;
        };
        let digits = marker
            .as_str()
            .strip_prefix(ARGUMENT)
            .and_then(|rest| rest.strip_suffix('>'))
            .unwrap_or("");
        let Ok(index) = digits.parse::<usize>() else {
            return untyped;
        };
        match self.path_entity(f, &r.enclosing, callee, 0) {
            Entity::External => Outcome::Unresolved(UnresolvedReason::External),
            Entity::Builtin => Outcome::Unresolved(UnresolvedReason::Builtin),
            Entity::Def(id) => {
                let rec = &self.ix.defs[id];
                let Some(written) = rec
                    .def
                    .signature
                    .as_deref()
                    .and_then(|signature| parameter_type(signature, index))
                else {
                    return untyped;
                };
                let scope = self.container_scope(rec);
                self.member_of_type(self.type_name_entity(rec.file, &scope, &written), &r.name)
            }
            _ => untyped,
        }
    }

    /// `super(...)` calls the constructor of the base class.
    fn super_call(&self, f: usize, scope: &Scope) -> Outcome {
        let Some(class) = self.class_of_scope(f, scope) else {
            return Outcome::Unresolved(UnresolvedReason::Opaque);
        };
        let constructor = Name::new("constructor");
        match self.class_member(class, &constructor, true, false) {
            Lookup::Found { entity, .. } => Outcome::Found {
                entity,
                evidence: Evidence::new(EvidenceKind::SuperCall),
            },
            _ => Outcome::Unresolved(UnresolvedReason::Builtin),
        }
    }

    /// A member of a receiver the code does not type. A call may be any method
    /// of that name that a file of the caller's reach holds; a read, a write or a
    /// pass may be any object's property, so it names nothing.
    fn untyped(&self, f: usize, r: &Ref, any: bool) -> Outcome {
        if r.use_kind != UseKind::Call {
            return Outcome::Unresolved(UnresolvedReason::UntypedRead);
        }
        // What a glob loads is asked for by name: `mod.mountA(el)`.
        let loaded: Vec<usize> = self
            .globbed(f)
            .into_iter()
            .flat_map(|g| self.offered_named(g, &r.name))
            .filter(|id| self.reachable(f, *id))
            .collect();
        if !loaded.is_empty() {
            return if loaded.len() > MAX_CANDIDATES {
                Outcome::Unresolved(UnresolvedReason::TooManyCandidates)
            } else {
                Outcome::Candidates {
                    ids: loaded,
                    reason: CandidateReason::ImportGlob,
                }
            };
        }
        let Some(pool) = self.ix.members.get(&r.name) else {
            return Outcome::Unresolved(UnresolvedReason::NoCandidates);
        };
        let linked = self.imported_files(f);
        let ids: Vec<usize> = pool
            .iter()
            .copied()
            .filter(|id| {
                let g = self.ix.defs[*id].file;
                (any || g == f || linked.contains(&g)) && self.reachable(f, *id)
            })
            .collect();
        // An implementation that is known stands for the interface declaration
        // that announces it: one callee, not two.
        let declares = |id: usize| {
            let rec = &self.ix.defs[id];
            rec.path
                .len()
                .checked_sub(1)
                .and_then(|n| self.ix.owner(rec.file, &rec.path[..n]))
                .is_some_and(|holder| self.ix.defs[holder].def.kind == DefKind::Interface)
        };
        let ids = if ids.iter().any(|id| !declares(*id)) {
            ids.into_iter().filter(|id| !declares(*id)).collect()
        } else {
            ids
        };
        if ids.is_empty() {
            Outcome::Unresolved(UnresolvedReason::NoCandidates)
        } else if ids.len() > MAX_CANDIDATES {
            Outcome::Unresolved(UnresolvedReason::TooManyCandidates)
        } else {
            Outcome::Candidates {
                ids,
                reason: if any {
                    CandidateReason::AnyReceiver
                } else {
                    CandidateReason::UntypedReceiver
                },
            }
        }
    }

    fn finish(
        &self,
        f: usize,
        use_kind: UseKind,
        jsx: bool,
        outcome: Outcome,
    ) -> (Relation, Resolution) {
        let plain = plain_relation(use_kind);
        match outcome {
            Outcome::Found { entity, evidence } => match entity {
                Entity::Class(id) | Entity::Def(id) => {
                    if !self.reachable(f, id) {
                        return unresolved(plain, UnresolvedReason::OutsideProject);
                    }
                    (
                        relation_for(use_kind, self.ix.defs[id].def.kind, jsx),
                        Resolution::Resolved {
                            target: self.ix.defs[id].key.clone(),
                            evidence,
                        },
                    )
                }
                Entity::Module(g) => (
                    plain,
                    Resolution::Module {
                        file: self.path_of(g).clone(),
                        evidence,
                    },
                ),
                Entity::External => unresolved(plain, UnresolvedReason::External),
                Entity::Builtin => unresolved(plain, UnresolvedReason::Builtin),
                Entity::Outside => unresolved(plain, UnresolvedReason::OutsideProject),
                Entity::Unknown => unresolved(plain, UnresolvedReason::NoBinding),
                Entity::Maybe(ids) => self.finish(
                    f,
                    use_kind,
                    jsx,
                    Outcome::Candidates {
                        ids,
                        reason: CandidateReason::ImportUnresolved,
                    },
                ),
                Entity::Instance(_) | Entity::Super(_) | Entity::Dynamic | Entity::Meta(_) => {
                    unresolved(plain, UnresolvedReason::Opaque)
                }
            },
            Outcome::Candidates { ids, reason } => {
                let keys: Vec<DefKey> = ids
                    .iter()
                    .filter(|id| self.reachable(f, **id))
                    .map(|id| self.ix.defs[*id].key.clone())
                    .collect();
                if keys.is_empty() {
                    unresolved(plain, UnresolvedReason::NoCandidates)
                } else {
                    let relation = ids.first().map_or(plain, |id| {
                        relation_for(use_kind, self.ix.defs[*id].def.kind, jsx)
                    });
                    (
                        relation,
                        Resolution::Candidate {
                            targets: keys,
                            reason,
                        },
                    )
                }
            }
            Outcome::Unresolved(reason) => unresolved(plain, reason),
        }
    }

    // ---- imports and exports

    fn import(&self, f: usize, import: &Import, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let kind = if import.kind == ImportKind::Dynamic {
            EvidenceKind::DynamicImport
        } else {
            EvidenceKind::ImportedName
        };
        let found = self.module(f, import.module.as_str());
        let make = |name: Name, resolution: Resolution| Binding {
            file: file.facts.path.clone(),
            site: import.site.clone(),
            from: import.enclosing.clone(),
            relation: Relation::Imports,
            name,
            resolution,
            entry_guard: false,
        };
        let whole = || match &found {
            ModRes::File(g) => Resolution::Module {
                file: self.path_of(*g).clone(),
                evidence: Evidence::new(kind),
            },
            other => module_failure(other),
        };
        let mut bound = false;
        if let Some(local) = import.star.alias() {
            out.push(make(local.clone(), whole()));
            bound = true;
        }
        if let Some(local) = &import.default {
            let resolution = self.member_resolution(f, &found, &Name::new("default"), kind);
            out.push(make(local.clone(), resolution));
            bound = true;
        }
        for (name, _) in &import.names {
            out.push(make(
                name.clone(),
                self.member_resolution(f, &found, name, kind),
            ));
            bound = true;
        }
        if !bound {
            out.push(make(Name::new(import.module.as_str()), whole()));
            for name in self.then_members(f, import) {
                out.push(make(
                    name.clone(),
                    self.member_resolution(f, &found, &name, kind),
                ));
            }
        }
    }

    /// The members of a module that `import('m').then((m) => ... m.name ...)`
    /// uses: the callback's first parameter is the module.
    fn then_members(&self, f: usize, import: &Import) -> Vec<Name> {
        let Some(text) = self.text(f) else {
            return Vec::new();
        };
        let mut at = skip_blanks(&text, import.site.bytes.end as usize);
        if !text[at..].starts_with(b".then") {
            return Vec::new();
        }
        at = skip_blanks(&text, at + ".then".len());
        if text.get(at) != Some(&b'(') {
            return Vec::new();
        }
        let callback = skip_blanks(&text, at + 1) as u32;
        let facts = &self.ix.files[f].facts.blob.facts;
        let is_callback = |scope: &Scope| matches!(scope, Scope::Callback { site, .. } if site.bytes.start == callback);
        let Some(module) = facts.iter().find_map(|fact| match fact {
            Fact::LocalBinding(local)
                if local.kind == LocalKind::Parameter && is_callback(&local.enclosing) =>
            {
                Some(local.name.clone())
            }
            _ => None,
        }) else {
            return Vec::new();
        };
        let mut names: Vec<Name> = Vec::new();
        for fact in facts {
            if let Fact::Reference(r) = fact {
                if r.qualifier.as_slice() == [module.clone()]
                    && ancestors(&r.enclosing).into_iter().any(is_callback)
                    && !names.contains(&r.name)
                {
                    names.push(r.name.clone());
                }
            }
        }
        names
    }

    /// What `name` of the module the import landed on is.
    fn member_resolution(
        &self,
        f: usize,
        found: &ModRes,
        name: &Name,
        kind: EvidenceKind,
    ) -> Resolution {
        if matches!(found, ModRes::Unplaced) {
            return match self.maybe(f, name) {
                Entity::Maybe(ids) => Resolution::Candidate {
                    targets: ids.iter().map(|id| self.ix.defs[*id].key.clone()).collect(),
                    reason: CandidateReason::ImportUnresolved,
                },
                _ => module_failure(found),
            };
        }
        let ModRes::File(g) = found else {
            return module_failure(found);
        };
        let mut through = Vec::new();
        let mut seen = Seen::new();
        match self.exported_member(*g, name, 0, &mut through, &mut seen) {
            Some(entity) => {
                self.imported_resolution(f, entity, Evidence::through(kind, self.paths(&through)))
            }
            None if name.as_str() == "default" && !self.ix.files[*g].exports.contains_key(name) => {
                Resolution::Module {
                    file: self.path_of(*g).clone(),
                    evidence: Evidence::new(kind),
                }
            }
            None => Resolution::Unresolved {
                reason: UnresolvedReason::NoBinding,
            },
        }
    }

    fn imported_resolution(&self, f: usize, entity: Entity, evidence: Evidence) -> Resolution {
        match entity {
            Entity::Class(id) | Entity::Def(id) => {
                if self.reachable(f, id) {
                    Resolution::Resolved {
                        target: self.ix.defs[id].key.clone(),
                        evidence,
                    }
                } else {
                    Resolution::Unresolved {
                        reason: UnresolvedReason::OutsideProject,
                    }
                }
            }
            Entity::Module(g) => Resolution::Module {
                file: self.path_of(g).clone(),
                evidence,
            },
            Entity::Maybe(ids) => Resolution::Candidate {
                targets: ids.iter().map(|id| self.ix.defs[*id].key.clone()).collect(),
                reason: CandidateReason::ImportUnresolved,
            },
            Entity::External => Resolution::Unresolved {
                reason: UnresolvedReason::External,
            },
            _ => Resolution::Unresolved {
                reason: UnresolvedReason::Opaque,
            },
        }
    }

    /// `export { a } from 'm'` and `export * from 'm'` take names from another
    /// module and offer them on.
    fn export(&self, f: usize, export: &Export, out: &mut Vec<Binding>) {
        let file = &self.ix.files[f];
        let make = |site: &Span, name: Name, resolution: Resolution| Binding {
            file: file.facts.path.clone(),
            site: site.clone(),
            from: Scope::Module,
            relation: Relation::Reexports,
            name,
            resolution,
            entry_guard: false,
        };
        match export {
            Export::Named {
                site,
                name,
                alias,
                from: Some(module),
                ..
            } => {
                let found = self.module(f, module.as_str());
                let resolution =
                    self.member_resolution(f, &found, name, EvidenceKind::ImportedName);
                out.push(make(
                    site,
                    alias.clone().unwrap_or_else(|| name.clone()),
                    resolution,
                ));
            }
            Export::Named {
                site,
                name,
                from: None,
                ..
            } => self.offered(f, site, name, out),
            Export::Default {
                site,
                local: Some(name),
            } => self.offered(f, site, name, out),
            Export::Star { site, from, alias } => {
                let resolution = match self.module(f, from.as_str()) {
                    ModRes::File(g) => Resolution::Module {
                        file: self.path_of(g).clone(),
                        evidence: Evidence::new(EvidenceKind::StarImport),
                    },
                    other => module_failure(&other),
                };
                out.push(make(
                    site,
                    alias.clone().unwrap_or_else(|| Name::new(from.as_str())),
                    resolution,
                ));
            }
            _ => {}
        }
    }

    /// `export { a }`, `export default a`, `exports.a = a` and `module.exports =
    /// a` offer a definition of the module as it is: the module's export of it,
    /// which a change to its body leaves as it was and a removal breaks at load.
    /// A name nothing in the module binds is a dangling export, kept as a row
    /// that landed on nothing.
    fn offered(&self, f: usize, site: &Span, name: &Name, out: &mut Vec<Binding>) {
        let mut through = Vec::new();
        let entity = self.module_name(f, name, 0, &mut through, &mut Seen::new());
        let resolution = match entity {
            // `export default function f() {}` and `exports.f = function f() {}`
            // declare what they export: the statement is the definition.
            Some(Entity::Class(id) | Entity::Def(id))
                if self.ix.defs[id].file == f
                    && self.ix.defs[id].def.span.bytes.start <= site.bytes.start
                    && site.bytes.end <= self.ix.defs[id].def.span.bytes.end =>
            {
                return;
            }
            Some(Entity::Class(id) | Entity::Def(id))
                if self.ix.defs[id].file == f || self.reachable(f, id) =>
            {
                Resolution::Resolved {
                    target: self.ix.defs[id].key.clone(),
                    evidence: Evidence::through(EvidenceKind::SameScope, self.paths(&through)),
                }
            }
            None => Resolution::Unresolved {
                reason: UnresolvedReason::NoBinding,
            },
            Some(_) => return,
        };
        out.push(Binding {
            file: self.path_of(f).clone(),
            site: site.clone(),
            from: Scope::Module,
            relation: Relation::Reexports,
            name: name.clone(),
            resolution,
            entry_guard: false,
        });
    }

    /// A glob loads every module its patterns match: an import of each, whose
    /// members the file may use.
    fn globs_imported(&self, out: &mut Vec<Binding>) {
        for (f, file) in self.ix.files.iter().enumerate() {
            if file.globs.is_empty() {
                continue;
            }
            for set in self.glob_sets(f).iter() {
                for g in &set.files {
                    let offered: Vec<usize> = self.ix.files[*g]
                        .children
                        .get(&Vec::new())
                        .into_iter()
                        .flatten()
                        .copied()
                        .filter(|id| self.ix.defs[*id].def.visibility == Visibility::Public)
                        .collect();
                    let resolution = if offered.is_empty() || offered.len() > MAX_CANDIDATES {
                        Resolution::Module {
                            file: self.path_of(*g).clone(),
                            evidence: Evidence::new(EvidenceKind::DynamicImport),
                        }
                    } else {
                        Resolution::Candidate {
                            targets: offered
                                .iter()
                                .map(|id| self.ix.defs[*id].key.clone())
                                .collect(),
                            reason: CandidateReason::ImportGlob,
                        }
                    };
                    out.push(Binding {
                        file: file.facts.path.clone(),
                        site: set.site.clone(),
                        from: set.scope.clone(),
                        relation: Relation::Imports,
                        name: Name::new(set.label.as_str()),
                        resolution,
                        entry_guard: false,
                    });
                }
            }
        }
    }

    // ---- overrides

    /// A method that replaces one of a class the repository holds and its class
    /// extends.
    fn overrides(&self, out: &mut Vec<Binding>) {
        for (id, rec) in self.ix.defs.iter().enumerate() {
            if !matches!(rec.def.kind, DefKind::Method | DefKind::Property) || rec.path.len() < 2 {
                continue;
            }
            if self.ix.owner(rec.file, &rec.path) != Some(id) {
                continue;
            }
            let Some(class) = self.ix.owner(rec.file, &rec.path[..rec.path.len() - 1]) else {
                continue;
            };
            if self.ix.defs[class].def.kind != DefKind::Class {
                continue;
            }
            let (found, _) = self.class_members(class, &rec.def.local.name, true);
            let Some(base) = self.ix.primary(&found, false) else {
                continue;
            };
            if !self.reachable(rec.file, base) {
                continue;
            }
            out.push(Binding {
                file: self.path_of(rec.file).clone(),
                site: rec.def.name_span.clone(),
                from: Scope::Def(rec.def.local.clone()),
                relation: Relation::Overrides,
                name: rec.def.local.name.clone(),
                resolution: Resolution::Resolved {
                    target: self.ix.defs[base].key.clone(),
                    evidence: Evidence::new(EvidenceKind::Overridden),
                },
                entry_guard: false,
            });
        }
    }
}

fn unresolved(relation: Relation, reason: UnresolvedReason) -> (Relation, Resolution) {
    (relation, Resolution::Unresolved { reason })
}

fn module_failure(found: &ModRes) -> Resolution {
    let reason = match found {
        ModRes::External => UnresolvedReason::External,
        ModRes::Outside => UnresolvedReason::OutsideProject,
        ModRes::Missing | ModRes::Unplaced => UnresolvedReason::MissingModule,
        ModRes::File(_) => UnresolvedReason::Opaque,
    };
    Resolution::Unresolved { reason }
}

/// A call of a class makes an object, and so does a JSX element of a component;
/// a variable passed as an argument is read.
fn relation_for(use_kind: UseKind, target: DefKind, jsx: bool) -> Relation {
    match (use_kind, target) {
        (UseKind::Call, DefKind::Class) => Relation::Instantiates,
        (UseKind::Call, _) if jsx => Relation::Instantiates,
        (
            UseKind::PassAsValue,
            DefKind::Variable | DefKind::Constant | DefKind::Field | DefKind::TypeAlias,
        ) => Relation::Reads,
        _ => plain_relation(use_kind),
    }
}

/// The `paths` entry tsc picks for a specifier: an exact one, else the one with
/// the longest prefix before its `*`.
fn best_alias<'p>(specifier: &str, aliases: &'p [Alias]) -> Option<&'p Alias> {
    aliases
        .iter()
        .filter(|alias| !alias_targets(specifier, alias).is_empty())
        .max_by_key(|alias| alias.pattern.find('*').map_or(usize::MAX, |star| star))
}

/// A specifier that reads as a bundler or `tsconfig` alias, not a package.
fn is_alias_like(specifier: &str) -> bool {
    specifier.starts_with("@/") || specifier.starts_with("~/") || specifier.starts_with('#')
}

/// The repository paths a `tsconfig` `paths` entry sends the specifier to.
fn alias_targets(specifier: &str, alias: &Alias) -> Vec<String> {
    match alias.pattern.split_once('*') {
        Some((prefix, suffix)) => {
            if specifier.len() >= prefix.len() + suffix.len()
                && specifier.starts_with(prefix)
                && specifier.ends_with(suffix)
            {
                let middle = &specifier[prefix.len()..specifier.len() - suffix.len()];
                alias
                    .targets
                    .iter()
                    .map(|target| target.replacen('*', middle, 1))
                    .collect()
            } else {
                Vec::new()
            }
        }
        None if specifier == alias.pattern => alias.targets.clone(),
        None => Vec::new(),
    }
}

/// The paths a project offers under the subpath: what its `exports` name, else
/// the places a package keeps its entry point and its files.
fn package_entries(project: &Project, subpath: &str) -> Vec<String> {
    let mut found = Vec::new();
    for export in &project.exports {
        match export.subpath.split_once('*') {
            Some((prefix, suffix)) => {
                if subpath.len() >= prefix.len() + suffix.len()
                    && subpath.starts_with(prefix)
                    && subpath.ends_with(suffix)
                {
                    let middle = &subpath[prefix.len()..subpath.len() - suffix.len()];
                    found.push(export.target.replacen('*', middle, 1));
                }
            }
            None if export.subpath == subpath => found.push(export.target.clone()),
            None => {}
        }
    }
    match subpath.strip_prefix("./") {
        None => {
            found.push(join(&project.root, "src/index"));
            found.push(join(&project.root, "index"));
        }
        Some(rest) if !rest.starts_with('*') => {
            found.push(join(&project.root, rest));
            found.push(join(&project.root, &format!("src/{rest}")));
        }
        Some(_) => {}
    }
    found
}

/// Whether a `/`-separated path matches a glob: `*` and `?` within a segment, `**`
/// across segments, `{a,b}` alternatives.
fn glob_match(pattern: &str, path: &str) -> bool {
    let segments: Vec<&str> = path.split('/').collect();
    expand_braces(pattern).iter().any(|alternative| {
        let parts: Vec<&str> = alternative.split('/').collect();
        match_segments(&parts, &segments)
    })
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_string()];
    };
    let Some(close) = pattern[open..].find('}').map(|at| at + open) else {
        return vec![pattern.to_string()];
    };
    let (before, after) = (&pattern[..open], &pattern[close + 1..]);
    pattern[open + 1..close]
        .split(',')
        .flat_map(|alternative| expand_braces(&format!("{before}{alternative}{after}")))
        .collect()
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| match_segments(rest, &path[skip..])),
        Some((segment, rest)) => {
            path.first()
                .is_some_and(|name| match_segment(segment.as_bytes(), name.as_bytes()))
                && match_segments(rest, &path[1..])
        }
    }
}

fn match_segment(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|skip| match_segment(rest, &name[skip..])),
        Some((b'?', rest)) => !name.is_empty() && match_segment(rest, &name[1..]),
        Some((c, rest)) => name.first() == Some(c) && match_segment(rest, &name[1..]),
    }
}

/// The names the global object of a page, a worker or Node goes by.
fn is_global_object(name: &str) -> bool {
    matches!(name, "self" | "window" | "globalThis" | "global")
}

/// A file the specifier names that is not code: styles, images, data.
fn is_asset(specifier: &str) -> bool {
    const ASSETS: &[&str] = &[
        "css", "scss", "sass", "less", "svg", "png", "jpg", "jpeg", "gif", "webp", "ico", "json",
        "md", "mdx", "html", "txt", "wasm", "yaml", "yml", "woff", "woff2", "ttf", "mp3", "mp4",
        "vue", "svelte", "astro", "graphql", "gql", "csv",
    ];
    let last = specifier.rsplit('/').next().unwrap_or(specifier);
    last.rsplit_once('.')
        .is_some_and(|(stem, extension)| !stem.is_empty() && ASSETS.contains(&extension))
}

/// A type parameter is a one- or two-letter capital: `T`, `K`, `TK`.
fn looks_like_type_parameter(name: &str) -> bool {
    (1..=2).contains(&name.len()) && name.chars().all(|c| c.is_ascii_uppercase())
}

/// Names the language and its usual hosts provide without an import.
fn is_global(name: &str) -> bool {
    const GLOBALS: &[&str] = &[
        "Array",
        "ArrayBuffer",
        "AbortController",
        "AbortSignal",
        "Atomics",
        "BigInt",
        "Blob",
        "Boolean",
        "Buffer",
        "Date",
        "DataView",
        "Error",
        "EvalError",
        "Event",
        "EventTarget",
        "File",
        "FileReader",
        "Float32Array",
        "Float64Array",
        "FormData",
        "Function",
        "Headers",
        "Infinity",
        "Int16Array",
        "Int32Array",
        "Int8Array",
        "Intl",
        "JSON",
        "JSX",
        "Map",
        "Math",
        "NaN",
        "Number",
        "Object",
        "Omit",
        "Partial",
        "Pick",
        "Promise",
        "Proxy",
        "RangeError",
        "ReadableStream",
        "Readonly",
        "Record",
        "ReferenceError",
        "Reflect",
        "RegExp",
        "Request",
        "Required",
        "Response",
        "ReturnType",
        "Set",
        "String",
        "Symbol",
        "SyntaxError",
        "TextDecoder",
        "TextEncoder",
        "TypeError",
        "URIError",
        "URL",
        "URLSearchParams",
        "Uint16Array",
        "Uint32Array",
        "Uint8Array",
        "Uint8ClampedArray",
        "WeakMap",
        "WeakSet",
        "WebSocket",
        "Worker",
        "XMLHttpRequest",
        "__dirname",
        "__filename",
        "alert",
        "arguments",
        "atob",
        "btoa",
        "clearInterval",
        "clearTimeout",
        "console",
        "crypto",
        "decodeURI",
        "decodeURIComponent",
        "document",
        "encodeURI",
        "encodeURIComponent",
        "escape",
        "eval",
        "exports",
        "fetch",
        "global",
        "globalThis",
        "history",
        "isFinite",
        "isNaN",
        "localStorage",
        "location",
        "module",
        "navigator",
        "parseFloat",
        "parseInt",
        "performance",
        "process",
        "queueMicrotask",
        "importScripts",
        "self",
        "require",
        "requestAnimationFrame",
        "cancelAnimationFrame",
        "sessionStorage",
        "setInterval",
        "setTimeout",
        "structuredClone",
        "undefined",
        "window",
        "afterAll",
        "afterEach",
        "beforeAll",
        "beforeEach",
        "describe",
        "expect",
        "it",
        "jest",
        "test",
        "vi",
    ];
    GLOBALS.contains(&name)
        || name.starts_with("HTML")
        || name.starts_with("SVG")
        || (name.len() > 5 && (name.ends_with("Event") || name.ends_with("Element")))
}
