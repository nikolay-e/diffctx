//! Import bindings and the references they resolve, for the languages with a
//! declared binding model: Python and JavaScript/TypeScript. A caller is a
//! definite caller only when a binding links its reference to the changed
//! definition; a spelling match whose binding cannot be resolved is a
//! candidate, and one whose binding points elsewhere is rejected. Other
//! languages keep the lexical model and say so (#343, #344, #349, #353).

use std::cell::RefCell;
use std::path::{Component, Path};
use std::rc::Rc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::source::Source;

mod js;
mod python;

use js::TsConfigs;
pub use js::js_bindings;
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
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" => Some(Lang::Js),
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

/// Where an import specifier lands in the analysed snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resolution {
    File(String),
    /// Several files fit and nothing ties the import to one of them.
    Ambiguous(Vec<String>),
    /// Not a file of this repository: the standard library, a package.
    External,
    /// A form the resolver does not model (a bundler alias, a missing
    /// relative target): the binding is unknown, not absent.
    Unsupported,
}

/// A repo-relative path with `.` and `..` folded, `/`-separated.
fn normalize(path: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

fn parent_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Duplicate roots (`src/shop/pricing.py`, `legacy/shop/pricing.py`): the
/// importer's own root decides, and nothing else does.
fn narrow_to_root(importer: &str, matches: &[&String], suffixes: &[&String]) -> Resolution {
    let root_of = |f: &str| -> String {
        suffixes
            .iter()
            .find_map(|s| f.strip_suffix(s.as_str()))
            .unwrap_or("")
            .to_string()
    };
    let rooted: Vec<&String> = matches
        .iter()
        .copied()
        .filter(|f| importer.starts_with(&root_of(f)))
        .collect();
    let deepest = rooted.iter().map(|f| f.len()).max();
    let best: Vec<&String> = rooted
        .into_iter()
        .filter(|f| Some(f.len()) == deepest)
        .collect();
    if let [one] = best.as_slice() {
        return Resolution::File((*one).clone());
    }
    let mut all: Vec<String> = matches.iter().map(|f| (*f).clone()).collect();
    all.sort();
    Resolution::Ambiguous(all)
}

/// Resolves imports against the analysed snapshot's file list, and reads a
/// file's bindings once per run.
pub struct Resolver<'a> {
    root: &'a Path,
    source: &'a Source,
    files: FxHashSet<String>,
    ts_configs: TsConfigs,
    cache: RefCell<FxHashMap<String, Rc<FileBindings>>>,
    /// Python resolution scans every file of the snapshot; the same
    /// importer asks about the same specifier once per caller it checks.
    resolved: RefCell<FxHashMap<(String, String), Resolution>>,
    /// The repository's own npm packages, by name: (directory, entries).
    workspace: std::cell::OnceCell<FxHashMap<String, (String, Vec<String>)>>,
    /// Whole files, for the module-level bindings a function reads.
    texts: RefCell<FxHashMap<String, Option<Rc<str>>>>,
    /// The directories a Python file puts on `sys.path`, by importer.
    python_roots: RefCell<FxHashMap<String, Rc<[String]>>>,
    js_facts: RefCell<FxHashMap<String, Option<Rc<js::JsFacts>>>>,
}

impl<'a> Resolver<'a> {
    pub fn new(root: &'a Path, source: &'a Source, files: FxHashSet<String>) -> Self {
        let ts_configs = TsConfigs::read(source, root, &files);
        Self {
            root,
            source,
            files,
            ts_configs,
            cache: RefCell::default(),
            resolved: RefCell::default(),
            workspace: std::cell::OnceCell::new(),
            texts: RefCell::default(),
            python_roots: RefCell::default(),
            js_facts: RefCell::default(),
        }
    }

    pub fn bindings(&self, path: &str) -> Rc<FileBindings> {
        if let Some(b) = self.cache.borrow().get(path) {
            return b.clone();
        }
        let text = self
            .source
            .read_to_string(&self.root.join(path))
            .unwrap_or_default();
        let b = Rc::new(match lang_of(path) {
            Some(Lang::Python) => python_bindings(&text),
            Some(Lang::Js) => js_bindings(&text),
            None => FileBindings::default(),
        });
        self.cache.borrow_mut().insert(path.to_string(), b.clone());
        b
    }

    pub fn resolve(&self, importer: &str, spec: &str) -> Resolution {
        let key = (importer.to_string(), spec.to_string());
        if let Some(known) = self.resolved.borrow().get(&key) {
            return known.clone();
        }
        let answer = match lang_of(importer) {
            Some(Lang::Python) => self.resolve_python(importer, spec),
            Some(Lang::Js) => self.resolve_js(importer, spec),
            None => Resolution::Unsupported,
        };
        self.resolved.borrow_mut().insert(key, answer.clone());
        answer
    }
}

/// The owner of a method the syntax does not name: an object built inside a
/// call. A call through any receiver may reach it.
pub(crate) const UNKNOWN_OWNER: &str = "<object>";

/// A read of a member through a receiver nothing types: not listed as a
/// caller, and not nothing either (#399).
pub(crate) const UNRESOLVED_READ: &str = "unresolved_read";

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

    fn best(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
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

/// The changed definition: `name` in `path`, a method of `class` when it
/// has one.
pub struct Definition<'d> {
    pub path: &'d str,
    pub name: &'d str,
    pub class: Option<&'d str>,
}

/// The fragment that may reference the definition.
pub struct Site<'c> {
    pub path: &'c str,
    pub content: &'c str,
    pub start_line: u32,
    /// The class the fragment sits in: its name and its header line.
    pub class: Option<(&'c str, &'c str)>,
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub evidence: Evidence,
    pub relation: Relation,
    pub lines: Vec<u32>,
}

const MAX_REEXPORT_DEPTH: usize = 3;

/// The head of a declaration whose parentheses hold parameters: `def f(`,
/// `function f(`, a method `name(` opening a body, an arrow `= (…) =>`.
static DECLARED_PARAMS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\s*(?:(?:export|default|public|private|protected|static|async|get|set|override)\s+)*(?:(?:def|function)\s*\*?\s*[\w$]*\s*\(|[\w$]+\s*\((?:[^()]*)\)\s*(?::[^{=]*)?\{|(?:const|let|var)\s+[\w$]+\s*=\s*(?:async\s*)?\()",
    )
    .unwrap()
});

/// The parameter names of a fragment's own declaration.
fn params_of(content: &str) -> FxHashSet<String> {
    let head: String = content.lines().take(8).collect::<Vec<_>>().join(" ");
    let Some(found) = DECLARED_PARAMS.find(&head) else {
        return FxHashSet::default();
    };
    let Some(start) = head[found.start()..]
        .find('(')
        .map(|i| found.start() + i + 1)
    else {
        return FxHashSet::default();
    };
    let mut depth = 1;
    let mut end = head.len();
    for (i, c) in head[start..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    // `const x = (f(y) || []).map(…)` opens a parenthesised expression, not
    // an arrow's parameters: those are followed by `=>` or a return type
    // (#520).
    let arrow_form = found.as_str().trim_end().ends_with('(') && found.as_str().contains('=');
    let rest = head.get(end + 1..).unwrap_or("").trim_start();
    if arrow_form && !(rest.starts_with("=>") || rest.starts_with(':')) {
        return FxHashSet::default();
    }
    head[start..end]
        .split(',')
        .filter_map(|p| {
            let p = p.trim().trim_start_matches(['*', '.']);
            let name: String = p
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

fn assigns(content: &str, name: &str) -> bool {
    content.lines().skip(1).any(|line| {
        let t = line.trim_start();
        let t = t
            .strip_prefix("let ")
            .or_else(|| t.strip_prefix("const "))
            .or_else(|| t.strip_prefix("var "))
            .unwrap_or(t);
        t.strip_prefix(name).is_some_and(|rest| {
            let rest = rest.trim_start();
            (rest.starts_with('=') && !rest.starts_with("=="))
                || rest.starts_with(':') && rest.contains('=')
        }) || t.starts_with(&format!("for {name} in "))
            || t.contains(&format!(" as {name}:"))
    })
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// The dotted receiver before `at` (`a.b` in `a.b.name`), if any.
fn receiver_chain(code: &str, at: usize) -> Option<&str> {
    let before = code[..at].trim_end();
    // `...name(...)` spreads what the call returns; its dots are no member
    // access (every `...workboxPolicy({..})` in a Vite config was rejected).
    if before.ends_with("...") {
        return None;
    }
    // `a?.name`: the optional chain's `?` sits before the dot.
    let head = before.strip_suffix('.')?;
    let head = head.strip_suffix('?').unwrap_or(head);
    // `Bot().message`: the receiver is what `Bot` constructs.
    if let Some(call) = head.strip_suffix("()") {
        let bytes = call.as_bytes();
        let mut start = call.len();
        while start > 0 && (is_word(bytes[start - 1]) || bytes[start - 1] == b'.') {
            start -= 1;
        }
        let callee = &head[start..];
        return Some(if start == call.len() { "" } else { callee });
    }
    let bytes = head.as_bytes();
    let mut start = head.len();
    while start > 0 && (is_word(bytes[start - 1]) || bytes[start - 1] == b'.') {
        start -= 1;
    }
    let chain = head[start..].trim_matches('.');
    if chain.is_empty() {
        // `foo().name`, `x[0].name`: an expression, not a name.
        Some("")
    } else {
        Some(chain)
    }
}

impl Resolver<'_> {
    fn module_is(&self, importer: &str, spec: &str, path: &str) -> Evidence {
        match self.resolve(importer, spec) {
            Resolution::File(f) if f == path => Evidence::Resolved,
            Resolution::Ambiguous(all) if all.iter().any(|f| f == path) => {
                Evidence::Candidate("ambiguous_module")
            }
            Resolution::Unsupported => Evidence::Candidate("import_unresolved"),
            _ => Evidence::Rejected,
        }
    }

    fn default_export_is(&self, path: &str, name: &str) -> bool {
        if let Some(facts) = self.js_facts(path) {
            return facts.default_names.iter().any(|n| n == name);
        }
        let text = self
            .source
            .read_to_string(&self.root.join(path))
            .unwrap_or_default();
        text.lines().any(|l| {
            let l = l.trim_start();
            l.strip_prefix("export default ").is_some_and(|rest| {
                let rest = rest
                    .trim_start_matches("async ")
                    .trim_start_matches("function")
                    .trim_start_matches('*')
                    .trim_start_matches("class")
                    .trim_start();
                rest.starts_with(name)
                    && !rest[name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
            })
        })
    }

    /// Whether `target`, bound in `importer`, names `name` of `path` —
    /// through re-exports up to `MAX_REEXPORT_DEPTH` hops.
    fn reaches(
        &self,
        importer: &str,
        target: &Target,
        path: &str,
        name: &str,
        depth: usize,
    ) -> Evidence {
        let Target::Member {
            module,
            name: imported,
        } = target
        else {
            return Evidence::Rejected;
        };
        if lang_of(importer) == Some(Lang::Python) {
            let sub = if module.ends_with('.') {
                format!("{module}{imported}")
            } else {
                format!("{module}.{imported}")
            };
            if let Resolution::File(_) = self.resolve(importer, &sub) {
                return Evidence::Rejected;
            }
        }
        match self.resolve(importer, module) {
            Resolution::File(f) if f == path => {
                if imported == name || (imported == "default" && self.default_export_is(path, name))
                {
                    Evidence::Resolved
                } else {
                    Evidence::Rejected
                }
            }
            Resolution::File(f) if depth < MAX_REEXPORT_DEPTH => {
                let through = self.bindings(&f);
                if let Some(next) = through.names.get(imported) {
                    return self.reaches(&f, next, path, name, depth + 1);
                }
                through
                    .star_from
                    .iter()
                    .map(|spec| {
                        let star = Target::Member {
                            module: spec.clone(),
                            name: imported.clone(),
                        };
                        self.reaches(&f, &star, path, name, depth + 1)
                    })
                    .fold(Evidence::Rejected, Evidence::best)
            }
            Resolution::Ambiguous(all) if all.iter().any(|f| f == path) => {
                Evidence::Candidate("ambiguous_module")
            }
            Resolution::Unsupported => Evidence::Candidate("import_unresolved"),
            _ => Evidence::Rejected,
        }
    }

    /// Whether a name used in `site` designates `path::name` (a class, when
    /// asked about a receiver; a function, when asked about a bare call).
    fn names(&self, site: &Site, word: &str, path: &str, name: &str) -> Evidence {
        if site.path == path && word == name {
            return Evidence::Resolved;
        }
        let bindings = self.bindings(site.path);
        if let Some(target) = bindings.names.get(word) {
            return self.reaches(site.path, target, path, name, 0);
        }
        if word != name || site.path == path {
            return Evidence::Rejected;
        }
        bindings
            .star_from
            .iter()
            .map(|spec| {
                let star = Target::Member {
                    module: spec.clone(),
                    name: name.to_string(),
                };
                self.reaches(site.path, &star, path, name, 0)
            })
            .fold(Evidence::Rejected, Evidence::best)
    }

    /// `chain.member` where `chain` names a module: does it name `path`?
    fn chain_is_module(&self, site: &Site, chain: &str, path: &str) -> Evidence {
        let mut parts = chain.split('.');
        let head = parts.next().unwrap_or("");
        let rest: Vec<&str> = parts.collect();
        let bindings = self.bindings(site.path);
        let Some(target) = bindings.names.get(head) else {
            return Evidence::Rejected;
        };
        let python = lang_of(site.path) == Some(Lang::Python);
        let spec = match target {
            Target::Module(m) if python && !rest.is_empty() => format!("{m}.{}", rest.join(".")),
            Target::Module(m) => m.clone(),
            Target::Member { module, name } if python => {
                let sep = if module.ends_with('.') { "" } else { "." };
                let mut s = format!("{module}{sep}{name}");
                for r in &rest {
                    s.push('.');
                    s.push_str(r);
                }
                s
            }
            Target::Member { module, name } => {
                return self.js_namespace_is(site.path, module, name, path, 0);
            }
        };
        self.module_is(site.path, &spec, path)
    }

    /// What `receiver.method` says about the receiver being `class`.
    fn receiver_is(&self, site: &Site, chain: &str, def: &Definition, class: &str) -> Evidence {
        if chain.is_empty() || class == UNKNOWN_OWNER {
            return Evidence::Candidate("receiver_unresolved");
        }
        if matches!(chain, "self" | "cls" | "this") {
            let Some((own, header)) = site.class else {
                return Evidence::Candidate("receiver_unresolved");
            };
            if own == class && site.path == def.path {
                return Evidence::Resolved;
            }
            return bases_of(header)
                .iter()
                .map(|b| self.names(site, b, def.path, class))
                .fold(Evidence::Rejected, Evidence::best);
        }
        if let Some(ctor) = chain.strip_suffix("()") {
            return self.constructor_is(site, ctor, def, class);
        }
        let head = chain.split('.').next().unwrap_or(chain);
        if !chain.contains('.') {
            let direct = self.names(site, chain, def.path, class);
            if direct != Evidence::Rejected
                || self.bindings(site.path).names.contains_key(chain)
                || (site.path == def.path && chain == class)
            {
                return direct;
            }
        }
        if let Some(ctor) = constructed_as(site.path, site.content, head)
            .or_else(|| annotated_as(site.content, head))
        {
            return self.constructor_is(site, &ctor, def, class);
        }
        if assigned_literal(site.path, site.content, head) {
            return Evidence::Rejected;
        }
        // A module-level binding the function reads: `const m = new Map()`.
        if let Some(text) = self.text(site.path) {
            let top: String = text
                .lines()
                .filter(|l| !l.starts_with([' ', '\t']))
                .collect::<Vec<_>>()
                .join("\n");
            if let Some(ctor) = constructed_as(site.path, &top, head) {
                return self.constructor_is(site, &ctor, def, class);
            }
            if assigned_literal(site.path, &top, head) {
                return Evidence::Rejected;
            }
        }
        Evidence::Candidate("receiver_unresolved")
    }

    fn text(&self, path: &str) -> Option<Rc<str>> {
        if let Some(known) = self.texts.borrow().get(path) {
            return known.clone();
        }
        let text = self
            .source
            .read_to_string(&self.root.join(path))
            .map(|t| Rc::from(t.as_str()));
        self.texts
            .borrow_mut()
            .insert(path.to_string(), text.clone());
        text
    }

    /// Whether `ctor` (`Bot`, `bots.Bot`) constructs `class`. A name nothing
    /// binds — a factory passed in, a local helper — leaves it unknown.
    fn constructor_is(&self, site: &Site, ctor: &str, def: &Definition, class: &str) -> Evidence {
        // `model: Any` states no type: the receiver is unknown, not another
        // class (#425).
        if matches!(ctor, "Any" | "typing.Any" | "t.Any" | "object") {
            return Evidence::Candidate("receiver_unresolved");
        }
        // `list()`, `new Map()`: the language's own type, never a class of
        // the repository (#389).
        if ctor != class && is_builtin_type(site.path, ctor) {
            return Evidence::Rejected;
        }
        let ctor_head = ctor.rsplit('.').next().unwrap_or(ctor);
        if ctor.contains('.') {
            let module = &ctor[..ctor.len() - ctor_head.len() - 1];
            if ctor_head != class {
                // `await Model.from_config(…)`: a factory on the class most
                // often returns one, which the syntax cannot confirm (#425).
                if module.rsplit('.').next() == Some(class) {
                    return match self.constructor_is(site, module, def, class) {
                        Evidence::Rejected => Evidence::Rejected,
                        _ => Evidence::Candidate("receiver_unresolved"),
                    };
                }
                if self.names_python_function(site.path, ctor) {
                    return Evidence::Candidate("receiver_unresolved");
                }
                return Evidence::Rejected;
            }
            let bound = self
                .bindings(site.path)
                .names
                .contains_key(module.split('.').next().unwrap_or(module));
            return match self.chain_is_module(site, module, def.path) {
                Evidence::Rejected if !bound => Evidence::Candidate("receiver_unresolved"),
                e => e,
            };
        }
        if site.path == def.path && ctor_head == class {
            return Evidence::Resolved;
        }
        if self.bindings(site.path).names.contains_key(ctor_head) {
            return match self.names(site, ctor_head, def.path, class) {
                Evidence::Rejected if self.names_python_function(site.path, ctor) => {
                    Evidence::Candidate("receiver_unresolved")
                }
                e => e,
            };
        }
        // A class the site's own file declares is that class, not the
        // definition's (`o: Other` beside `class Other`).
        if self.declares_python_class(site.path, ctor_head) {
            return Evidence::Rejected;
        }
        Evidence::Candidate("receiver_unresolved")
    }

    /// Every reference `site` makes to `def`, as one finding: the strongest
    /// evidence among its occurrences, and the lines that carry it.
    pub fn classify(&self, site: &Site, def: &Definition) -> Option<Finding> {
        let words = self.names_for(site, def);
        let params = params_of(site.content);
        let mut best: Option<Finding> = None;
        let masked = crate::impact::code_text(site.path, site.content);
        let syntax = (lang_of(site.path) == Some(Lang::Js))
            .then(|| self.js_facts(site.path))
            .flatten();
        for ((i, code), raw) in masked.lines().enumerate().zip(site.content.lines()) {
            if is_import_line(code) {
                continue;
            }
            let line_no = site.start_line + i as u32;
            for word in &words {
                for (at, _) in code.match_indices(word) {
                    let lookup = syntax.as_deref().map(|f| f.at(line_no, at, word, raw));
                    let found = match lookup {
                        Some(js::Lookup::NotCode) => None,
                        Some(js::Lookup::Found(o)) if receiver_chain(code, at).is_none() => {
                            self.js_bare_use(site, def, o, code[at + word.len()..].trim_start())
                        }
                        _ => self.occurrence(site, def, code, at, word, &params),
                    };
                    if let Some((evidence, relation)) = found {
                        absorb(&mut best, evidence, relation, line_no);
                    }
                }
            }
        }
        best
    }

    /// The names under which `site`'s file can reach `def`: its own name,
    /// and every local an import binds to it under another name.
    fn names_for<'n>(&self, site: &Site, def: &'n Definition) -> Vec<String> {
        let mut words = vec![def.name.to_string()];
        if def.class.is_none() {
            words.extend(
                self.bindings(site.path)
                    .names
                    .iter()
                    .filter_map(|(local, t)| match t {
                        Target::Member { name, .. }
                            if local != def.name && (name == def.name || name == "default") =>
                        {
                            Some(local.clone())
                        }
                        _ => None,
                    }),
            );
        }
        words
    }

    /// One occurrence of `word` at `at` in a masked line: what it says about
    /// `def`, and how it uses it. `None` for anything that is not a use —
    /// part of a longer name, a definition, an assignment target, a
    /// rejected binding.
    fn occurrence(
        &self,
        site: &Site,
        def: &Definition,
        code: &str,
        at: usize,
        word: &str,
        params: &FxHashSet<String>,
    ) -> Option<(Evidence, Relation)> {
        let bytes = code.as_bytes();
        let after = at + word.len();
        let embedded = at.checked_sub(1).is_some_and(|j| is_word(bytes[j]))
            || bytes.get(after).copied().is_some_and(is_word);
        let before = code[..at].trim_end();
        let defines = ["def", "class", "function", "async def"]
            .iter()
            .any(|k| before.ends_with(k));
        let next = code[after..].trim_start();
        // `x = …` binds a name of its own; `obj.x = …` writes the member,
        // which is a use of it (#488).
        let assigned =
            next.starts_with('=') && !next.starts_with("==") && receiver_chain(code, at).is_none();
        if embedded || defines || assigned {
            return None;
        }
        let relation = if next.starts_with('(') || next.starts_with("?.(") {
            Relation::Call
        } else {
            Relation::Reference
        };
        let own = word == def.name;
        let evidence = match (receiver_chain(code, at), def.class) {
            (Some(chain), Some(class)) if own => self.receiver_is(site, chain, def, class),
            (Some(chain), None) if own => self.module_member(site, chain, def),
            (Some(_), _) | (None, Some(_)) => Evidence::Rejected,
            (None, None) if params.contains(word) || assigns(site.content, word) => {
                Evidence::Rejected
            }
            (None, None) => self.names(site, word, def.path, def.name),
        };
        // Reading `x.name` through a receiver nothing types is any object's
        // attribute (`response.headers`); only a call is worth a guess, and a
        // read is counted as unresolved rather than listed (#399).
        let evidence = if relation == Relation::Reference
            && matches!(evidence, Evidence::Candidate("receiver_unresolved"))
        {
            Evidence::Candidate(UNRESOLVED_READ)
        } else {
            evidence
        };
        (evidence != Evidence::Rejected).then_some((evidence, relation))
    }

    fn module_member(&self, site: &Site, chain: &str, def: &Definition) -> Evidence {
        let found = if matches!(chain, "self" | "cls" | "this") || chain.is_empty() {
            Evidence::Rejected
        } else {
            self.chain_is_module(site, chain, def.path)
        };
        // A module a glob loads arrives as a value nothing types (#357).
        let unbound = !self
            .bindings(site.path)
            .names
            .contains_key(chain.split('.').next().unwrap_or(""));
        if found == Evidence::Rejected
            && chain != "this"
            && unbound
            && self.js_glob_loads(site.path, def.path)
        {
            Evidence::Candidate("import_glob")
        } else {
            found
        }
    }
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
            lines: vec![line],
        });
        return;
    };
    if evidence.rank() > f.evidence.rank() {
        *f = Finding {
            evidence,
            relation,
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

static CLASS_HEADER: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"class\s+[\w$]+\s*(?:\(([^)]*)\)|extends\s+([\w$.]+)(?:\s+implements\s+([\w$.,\s]+))?)",
    )
    .unwrap()
});

fn bases_of(header: &str) -> Vec<String> {
    let Some(c) = CLASS_HEADER.captures(header) else {
        return Vec::new();
    };
    [c.get(1), c.get(2), c.get(3)]
        .into_iter()
        .flatten()
        .flat_map(|m| m.as_str().split(','))
        .map(|b| b.trim().split(['[', '<', '(']).next().unwrap_or("").trim())
        .filter(|b| !b.is_empty() && !b.contains('='))
        .map(|b| b.rsplit('.').next().unwrap_or(b).to_string())
        .collect()
}

/// `x = Bot(...)`, `const x = new Bot(...)`, `with Bot(...) as x`: the
/// constructor `x` holds. A binding is read within one line: across a
/// newline, `as srv:` and the next line's `result = srv.call_tool(...)`
/// read as an annotated assignment from `srv.call_tool` (#399).
fn constructed_as(path: &str, content: &str, var: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r"(?:^|[^\w$.])({})\s*(?::\s*[\w$.\[\]<>]+\s*)?=\s*(?:new\s+|await\s+)?([\w$.]+)\s*(?:<[^()=;]*>)?\s*\(",
        regex::escape(var)
    ))
    .ok()?;
    crate::impact::code_text(path, content)
        .lines()
        .find_map(|code| {
            entered_as(code, var).or_else(|| {
                re.captures_iter(code)
                    .find(|c| c.get(1).is_some_and(|v| !after_as(code, v.start())))
                    .map(|c| c[2].to_string())
            })
        })
}

/// Types the language provides: a receiver built from one is that type.
const PYTHON_BUILTIN_TYPES: &[&str] = &[
    "list",
    "dict",
    "set",
    "tuple",
    "str",
    "int",
    "float",
    "bytes",
    "bytearray",
    "frozenset",
    "bool",
    "complex",
    "range",
    "memoryview",
];
const JS_BUILTIN_TYPES: &[&str] = &[
    "Map",
    "Set",
    "WeakMap",
    "WeakSet",
    "Array",
    "Object",
    "Promise",
    "Date",
    "RegExp",
    "Error",
    "URL",
    "URLSearchParams",
    "Headers",
    "Request",
    "Response",
    "FormData",
    "String",
    "Number",
    "Boolean",
    "ArrayBuffer",
    "DataView",
    "Uint8Array",
    "Int32Array",
    "Float64Array",
    "AbortController",
    "TextEncoder",
    "TextDecoder",
    "Blob",
];

fn is_builtin_type(path: &str, ctor: &str) -> bool {
    match lang_of(path) {
        Some(Lang::Python) => PYTHON_BUILTIN_TYPES.contains(&ctor),
        Some(Lang::Js) => JS_BUILTIN_TYPES.contains(&ctor),
        None => false,
    }
}

/// `x = []`, `const x = {}`, `x = "…"`, `x = 0`: a literal, the language's
/// own container, string or number.
fn assigned_literal(path: &str, content: &str, var: &str) -> bool {
    let Ok(re) = Regex::new(&format!(
        r#"(?:^|[^\w$.])({})\s*(?::\s*[\w$.\[\]<>, ]+\s*)?=\s*[\[{{"'`0-9-]"#,
        regex::escape(var)
    )) else {
        return false;
    };
    crate::impact::code_text(path, content)
        .lines()
        .any(|code| re.is_match(code))
}

/// Whether the name at `at` is the target of an `as` (`with … as x`,
/// `except E as x`), where a `:` after it ends the statement's head.
fn after_as(code: &str, at: usize) -> bool {
    code[..at]
        .trim_end()
        .strip_suffix("as")
        .is_some_and(|rest| rest.is_empty() || rest.ends_with([' ', '\t', ')']))
}

/// `with Bot(...) as x`, `async with Bot(...) as x`, `case Bot() as x`: an
/// instance of the class the call constructs. A function's context
/// (`open(p)`, `closing(conn)`) says nothing of what it yields.
fn entered_as(code: &str, var: &str) -> Option<String> {
    let bytes = code.as_bytes();
    code.match_indices(var).find_map(|(at, _)| {
        let end = at + var.len();
        let embedded = at.checked_sub(1).is_some_and(|j| is_word(bytes[j]))
            || bytes.get(end).copied().is_some_and(is_word);
        if embedded || !after_as(code, at) {
            return None;
        }
        let call = code[..at]
            .trim_end()
            .strip_suffix("as")?
            .trim_end()
            .strip_suffix(')')?;
        let mut depth = 1;
        let open = call.char_indices().rev().find_map(|(i, c)| {
            match c {
                ')' => depth += 1,
                '(' => depth -= 1,
                _ => {}
            }
            (depth == 0).then_some(i)
        })?;
        let head = call[..open].trim_end();
        let start = head
            .rfind(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '.'))
            .map_or(0, |i| i + 1);
        let callee = head[start..].trim_matches('.');
        callee
            .rsplit('.')
            .next()
            .is_some_and(|class| class.starts_with(|c: char| c.is_ascii_uppercase()))
            .then(|| callee.to_string())
    })
}

/// `x: Bot` in a signature or a declaration, within one line; the `:` after
/// `as x` ends a `with` or `except` head and annotates nothing.
fn annotated_as(content: &str, var: &str) -> Option<String> {
    let re = Regex::new(&format!(
        r#"(?:^|[^\w$.])({})\s*:\s*["']?([A-Z][\w$.]*)"#,
        regex::escape(var)
    ))
    .ok()?;
    content.lines().find_map(|line| {
        re.captures_iter(line)
            .find(|c| c.get(1).is_some_and(|v| !after_as(line, v.start())))
            .map(|c| c[2].to_string())
    })
}

impl Resolver<'_> {
    /// Whether `importer` binds anything from the file at `path`.
    pub fn imports_file(&self, importer: &str, path: &str) -> bool {
        let bindings = self.bindings(importer);
        bindings
            .names
            .values()
            .map(|t| match t {
                Target::Module(m) | Target::Member { module: m, .. } => m,
            })
            .chain(&bindings.star_from)
            .any(|spec| matches!(self.resolve(importer, spec), Resolution::File(f) if f == path))
            || self.js_glob_loads(importer, path)
    }
}
