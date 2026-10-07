//! Import bindings and the references they resolve, for the languages with a
//! declared binding model: Python and JavaScript/TypeScript. A caller is a
//! definite caller only when a binding links its reference to the changed
//! definition; a spelling match whose binding cannot be resolved is a
//! candidate, and one whose binding points elsewhere is rejected. Other
//! languages keep the lexical model and say so (#343, #344, #349, #353).

use std::cell::RefCell;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::source::Source;

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

static PY_IMPORT: Lazy<Regex> = Lazy::new(|| Regex::new(r"^import\s+(.+)$").unwrap());
static PY_FROM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^from\s+([.\w]+)\s+import\s+(.+)$").unwrap());

/// Import statements as single logical lines: parenthesised and
/// backslash-continued imports joined, comments dropped.
fn paren_balance(line: &str) -> isize {
    line.chars()
        .map(|c| match c {
            '(' => 1,
            ')' => -1,
            _ => 0,
        })
        .sum()
}

fn python_statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    let mut depth = 0isize;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(acc) = pending.as_mut() {
            acc.push(' ');
            acc.push_str(line.trim_end_matches('\\'));
            depth += paren_balance(line);
            if depth <= 0 && !line.ends_with('\\') {
                out.push(pending.take().unwrap_or_default());
            }
            continue;
        }
        if !(line.starts_with("import ") || line.starts_with("from ")) {
            continue;
        }
        depth = paren_balance(line);
        if depth > 0 || line.ends_with('\\') {
            pending = Some(line.trim_end_matches('\\').to_string());
        } else {
            out.push(line.to_string());
        }
    }
    out.extend(pending);
    out
}

pub fn python_bindings(text: &str) -> FileBindings {
    let mut b = FileBindings::default();
    for stmt in python_statements(text) {
        let stmt = stmt.replace(['(', ')'], " ");
        if let Some(c) = PY_FROM.captures(stmt.trim()) {
            for item in c[2].split(',') {
                bind_from_item(&mut b, &c[1], item);
            }
        } else if let Some(c) = PY_IMPORT.captures(stmt.trim()) {
            for item in c[1].split(',') {
                bind_import_item(&mut b, item);
            }
        }
    }
    b
}

/// `x`, `x as y` or `*` of `from module import …`.
fn bind_from_item(b: &mut FileBindings, module: &str, item: &str) {
    let words: Vec<&str> = item.split_whitespace().collect();
    let (name, local) = match words.as_slice() {
        ["*"] => {
            b.star_from.push(module.to_string());
            return;
        }
        [name] => (*name, *name),
        [name, "as", local] => (*name, *local),
        _ => return,
    };
    b.names.insert(
        local.to_string(),
        Target::Member {
            module: module.to_string(),
            name: name.to_string(),
        },
    );
}

/// `a.b.c` (binds `a`) or `a.b as x` (binds `x` to `a.b`) of `import …`.
fn bind_import_item(b: &mut FileBindings, item: &str) {
    let words: Vec<&str> = item.split_whitespace().collect();
    let (local, module) = match words.as_slice() {
        [dotted] => {
            let head = dotted.split('.').next().unwrap_or(dotted);
            (head, head)
        }
        [dotted, "as", local] => (*local, *dotted),
        _ => return,
    };
    b.names
        .insert(local.to_string(), Target::Module(module.to_string()));
}

static JS_IMPORT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)\bimport\s+(type\s+)?([^'"`;]*?)\s+from\s+['"]([^'"]+)['"]"#).unwrap()
});
static JS_EXPORT_FROM: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)\bexport\s+(type\s+)?(\*|\*\s+as\s+\w+|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]"#)
        .unwrap()
});
static JS_REQUIRE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)\b(?:const|let|var)\s+(\{[^}]*\}|[\w$]+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#,
    )
    .unwrap()
});

fn js_named(list: &str, module: &str, b: &mut FileBindings, separator: &str) {
    for item in list.trim_matches(|c| c == '{' || c == '}').split(',') {
        let item = item.trim().trim_start_matches("type ").trim();
        if item.is_empty() {
            continue;
        }
        let (name, local) = match item.split_once(separator) {
            Some((n, l)) => (n.trim(), l.trim()),
            None => (item, item),
        };
        if is_ident(name) && is_ident(local) {
            b.names.insert(
                local.to_string(),
                Target::Member {
                    module: module.to_string(),
                    name: name.to_string(),
                },
            );
        }
    }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

pub fn js_bindings(text: &str) -> FileBindings {
    let mut b = FileBindings::default();
    for c in JS_IMPORT.captures_iter(text) {
        bind_js_import(&mut b, c[2].trim(), &c[3]);
    }
    for c in JS_EXPORT_FROM.captures_iter(text) {
        bind_js_reexport(&mut b, c[2].trim(), &c[3]);
    }
    for c in JS_REQUIRE.captures_iter(text) {
        let lhs = c[1].trim();
        if lhs.starts_with('{') {
            js_named(lhs, &c[2], &mut b, ":");
        } else if is_ident(lhs) {
            b.names
                .insert(lhs.to_string(), Target::Module(c[2].to_string()));
        }
    }
    b
}

/// The clause of `import <clause> from 'module'`: a default, `* as ns`,
/// `{ a, b as c }`, or a default followed by one of the others.
fn bind_js_import(b: &mut FileBindings, clause: &str, module: &str) {
    let (default, rest) = match clause.split_once(',') {
        Some((d, r)) if !d.trim_start().starts_with('{') => (Some(d.trim()), r.trim()),
        _ if !clause.starts_with('{') && !clause.starts_with('*') => (Some(clause), ""),
        _ => (None, clause),
    };
    if let Some(d) = default.filter(|d| is_ident(d)) {
        b.names.insert(
            d.to_string(),
            Target::Member {
                module: module.to_string(),
                name: "default".to_string(),
            },
        );
    }
    if rest.starts_with('{') {
        js_named(rest, module, b, " as ");
    } else if let Some(local) = namespace_local(rest) {
        b.names
            .insert(local.to_string(), Target::Module(module.to_string()));
    }
}

/// The clause of `export <clause> from 'module'`.
fn bind_js_reexport(b: &mut FileBindings, clause: &str, module: &str) {
    if clause == "*" {
        b.star_from.push(module.to_string());
    } else if let Some(local) = namespace_local(clause) {
        b.names
            .insert(local.to_string(), Target::Module(module.to_string()));
    } else {
        js_named(clause, module, b, " as ");
    }
}

/// `ns` of `* as ns`.
fn namespace_local(clause: &str) -> Option<&str> {
    let local = clause.strip_prefix('*')?.trim().strip_prefix("as")?.trim();
    is_ident(local).then_some(local)
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

const JS_PROBES: &[&str] = &[
    "",
    ".ts",
    ".tsx",
    ".js",
    ".jsx",
    ".mjs",
    ".cjs",
    ".mts",
    ".cts",
    ".d.ts",
    "/index.ts",
    "/index.tsx",
    "/index.js",
    "/index.jsx",
    "/index.mjs",
    "/index.cjs",
];

/// `compilerOptions.paths` of one tsconfig, its `extends` chain folded in,
/// read as data: `"@/*": ["src/*"]`-style patterns with one `*`. Both
/// directories are repo-relative, `""` the root: `base_url` is `baseUrl`
/// from its config's directory, the only base a bare specifier resolves
/// against; `paths_dir` is the directory of the config that declares
/// `paths`, what their targets resolve against when there is no `baseUrl`.
#[derive(Default, Clone)]
struct TsPaths {
    base_url: Option<String>,
    paths_dir: Option<String>,
    patterns: Vec<(String, Vec<String>)>,
    readable: bool,
}

fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('"', _) => copy_json_string(&mut chars, &mut out),
            ('/', Some('/')) => {
                if chars.by_ref().any(|n| n == '\n') {
                    out.push('\n');
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    static TRAILING_COMMA: Lazy<Regex> = Lazy::new(|| Regex::new(r",(\s*[}\]])").unwrap());
    TRAILING_COMMA.replace_all(&out, "$1").into_owned()
}

/// A JSON string from its opening quote on, escapes kept: a `//` inside it
/// is not a comment.
fn copy_json_string(chars: &mut std::iter::Peekable<std::str::Chars>, out: &mut String) {
    out.push('"');
    while let Some(c) = chars.next() {
        out.push(c);
        match c {
            '\\' => out.extend(chars.next()),
            '"' => return,
            _ => {}
        }
    }
}

/// Every tsconfig/jsconfig of the snapshot by directory: an import is
/// resolved with the one nearest to it, as `tsc` and the bundlers do. A
/// monorepo keeps its aliases in each app's `tsconfig.app.json`, and reading
/// the root file alone left them all unresolved (#372).
#[derive(Default)]
struct TsConfigs {
    by_dir: FxHashMap<String, TsPaths>,
}

fn is_tsconfig(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json")
}

impl TsConfigs {
    fn read(source: &Source, root: &Path, files: &FxHashSet<String>) -> Self {
        let mut by_dir: FxHashMap<String, TsPaths> = FxHashMap::default();
        let mut configs: Vec<&String> = files.iter().filter(|f| is_tsconfig(f)).collect();
        // `tsconfig.json` first: its patterns win a clash with a sibling
        // config of the same directory.
        configs.sort_by_key(|f| {
            (
                !f.ends_with("/tsconfig.json") && f.as_str() != "tsconfig.json",
                f.as_str(),
            )
        });
        for config in configs {
            let dir = parent_dir(config).to_string();
            let read = TsPaths::read_chain(source, root, files, config, 0);
            let entry = by_dir.entry(dir).or_insert_with(|| TsPaths {
                readable: true,
                ..TsPaths::default()
            });
            if entry.base_url.is_none() {
                entry.base_url = read.base_url;
            }
            if entry.paths_dir.is_none() {
                entry.paths_dir = read.paths_dir;
            }
            for (pattern, targets) in read.patterns {
                if !entry.patterns.iter().any(|(p, _)| *p == pattern) {
                    entry.patterns.push((pattern, targets));
                }
            }
            entry.readable &= read.readable;
        }
        Self { by_dir }
    }

    fn for_importer(&self, importer: &str) -> Option<&TsPaths> {
        let mut dir = parent_dir(importer);
        loop {
            if let Some(found) = self.by_dir.get(dir) {
                return Some(found);
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent_dir(dir);
        }
    }
}

impl TsPaths {
    /// One config with its relative `extends` chain; a package `extends`
    /// (`@tsconfig/node20`) cannot be read here, so the result is not
    /// trusted to be complete.
    fn read_chain(
        source: &Source,
        root: &Path,
        files: &FxHashSet<String>,
        config: &str,
        depth: u8,
    ) -> Self {
        let parsed = source
            .read_to_string(&root.join(config))
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&strip_json_comments(&t)).ok());
        let Some(doc) = parsed else {
            return Self::default();
        };
        let dir = parent_dir(config);
        let mut inherited = Self {
            readable: true,
            ..Self::default()
        };
        let extends: Vec<&str> = match &doc["extends"] {
            serde_json::Value::String(e) => vec![e.as_str()],
            serde_json::Value::Array(a) => a.iter().filter_map(|e| e.as_str()).collect(),
            _ => Vec::new(),
        };
        for parent in extends {
            let local = parent.starts_with('.');
            let path = normalize(&Path::new(dir).join(parent));
            let path = if path.ends_with(".json") {
                path
            } else {
                format!("{path}.json")
            };
            if local && depth < 8 && files.contains(&path) {
                let base = Self::read_chain(source, root, files, &path, depth + 1);
                inherited.readable &= base.readable;
                if base.base_url.is_some() {
                    inherited.base_url = base.base_url;
                }
                if base.paths_dir.is_some() {
                    inherited.paths_dir = base.paths_dir;
                }
                for p in base.patterns {
                    inherited.patterns.retain(|(k, _)| *k != p.0);
                    inherited.patterns.push(p);
                }
            } else {
                inherited.readable = false;
            }
        }
        let opts = &doc["compilerOptions"];
        if let Some(b) = opts["baseUrl"].as_str() {
            inherited.base_url = Some(normalize(&Path::new(dir).join(b)));
        }
        if let Some(m) = opts["paths"].as_object() {
            inherited.paths_dir = Some(normalize(Path::new(dir)));
            inherited.patterns = m
                .iter()
                .map(|(k, v)| {
                    let targets = v
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    (k.clone(), targets)
                })
                .collect();
        }
        inherited
    }

    /// The targets of the pattern tsc picks: an exact one, else the one with
    /// the longest prefix before its `*`, the first on a tie.
    fn expand(&self, spec: &str) -> Option<Vec<String>> {
        let (_, star, targets) = self
            .patterns
            .iter()
            .rev()
            .filter_map(|(pattern, targets)| match pattern.split_once('*') {
                Some((pre, post)) => spec
                    .strip_prefix(pre)
                    .and_then(|r| r.strip_suffix(post))
                    .map(|star| (pre.len(), star.to_string(), targets)),
                None => (spec == pattern).then(|| (usize::MAX, String::new(), targets)),
            })
            .max_by_key(|(rank, _, _)| *rank)?;
        let base = self.base_url.as_ref().or(self.paths_dir.as_ref());
        Some(
            targets
                .iter()
                .map(|t| {
                    normalize(
                        &Path::new(base.map_or("", String::as_str)).join(t.replace('*', &star)),
                    )
                })
                .collect(),
        )
    }
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

    fn python_module_files(&self, rel: &str) -> [String; 2] {
        [format!("{rel}.py"), format!("{rel}/__init__.py")]
    }

    fn resolve_python(&self, importer: &str, spec: &str) -> Resolution {
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let rest = spec[dots..].replace('.', "/");
        if dots > 0 {
            return self.resolve_python_relative(importer, dots, &rest);
        }
        let [module_file, package_file] = self.python_module_files(&rest);
        let matches: Vec<&String> = self
            .files
            .iter()
            .filter(|f| {
                [&module_file, &package_file].into_iter().any(|suffix| {
                    f.as_str() == suffix.as_str() || f.ends_with(&format!("/{suffix}"))
                })
            })
            .collect();
        match matches.as_slice() {
            [] => Resolution::External,
            [one] => Resolution::File((*one).clone()),
            _ => narrow_to_root(importer, &matches, &[&module_file, &package_file]),
        }
    }

    fn resolve_python_relative(&self, importer: &str, dots: usize, rest: &str) -> Resolution {
        let mut dir = parent_dir(importer).to_string();
        for _ in 1..dots {
            dir = parent_dir(&dir).to_string();
        }
        let base = match (dir.is_empty(), rest.is_empty()) {
            (_, true) => dir,
            (true, false) => rest.to_string(),
            (false, false) => format!("{dir}/{rest}"),
        };
        let found: Vec<String> = if base.is_empty() {
            vec!["__init__.py".to_string()]
        } else {
            self.python_module_files(&base).into()
        };
        found
            .into_iter()
            .find(|f| self.files.contains(f))
            .map_or(Resolution::Unsupported, Resolution::File)
    }

    fn probe_js(&self, base: &str) -> Option<String> {
        let stem = base
            .strip_suffix(".js")
            .or_else(|| base.strip_suffix(".jsx"))
            .or_else(|| base.strip_suffix(".mjs"))
            .unwrap_or(base);
        JS_PROBES
            .iter()
            .map(|ext| format!("{base}{ext}"))
            .chain(
                (stem != base)
                    .then(|| [".ts", ".tsx", ".mts"].map(|e| format!("{stem}{e}")))
                    .into_iter()
                    .flatten(),
            )
            .find(|p| self.files.contains(p))
    }

    fn resolve_js(&self, importer: &str, spec: &str) -> Resolution {
        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            let joined = normalize(&PathBuf::from(parent_dir(importer)).join(spec));
            return self
                .probe_js(&joined)
                .map_or(Resolution::Unsupported, Resolution::File);
        }
        let none = TsPaths {
            readable: true,
            ..TsPaths::default()
        };
        let ts = self.ts_configs.for_importer(importer).unwrap_or(&none);
        if let Some(targets) = ts.expand(spec) {
            return targets
                .iter()
                .find_map(|t| self.probe_js(t))
                .map_or(Resolution::Unsupported, Resolution::File);
        }
        if let Some(base) = &ts.base_url {
            let based = normalize(&Path::new(base).join(spec));
            if let Some(found) = self.probe_js(&based) {
                return Resolution::File(found);
            }
        }
        let alias_like = spec.starts_with("@/")
            || spec.starts_with("~/")
            || spec.starts_with('#')
            || spec.starts_with('/');
        if alias_like || !ts.readable {
            Resolution::Unsupported
        } else {
            Resolution::External
        }
    }
}

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
            Self::Candidate(_) => 1,
            Self::Resolved => 2,
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
            Target::Member { .. } => return Evidence::Rejected,
        };
        self.module_is(site.path, &spec, path)
    }

    /// What `receiver.method` says about the receiver being `class`.
    fn receiver_is(&self, site: &Site, chain: &str, def: &Definition, class: &str) -> Evidence {
        if chain.is_empty() {
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
        let python = lang_of(site.path) == Some(Lang::Python);
        if let Some(ctor) =
            constructed_as(site.content, head, python).or_else(|| annotated_as(site.content, head))
        {
            return self.constructor_is(site, &ctor, def, class);
        }
        Evidence::Candidate("receiver_unresolved")
    }

    /// Whether `ctor` (`Bot`, `bots.Bot`) constructs `class`. A name nothing
    /// binds — a factory passed in, a local helper — leaves it unknown.
    fn constructor_is(&self, site: &Site, ctor: &str, def: &Definition, class: &str) -> Evidence {
        let ctor_head = ctor.rsplit('.').next().unwrap_or(ctor);
        if ctor.contains('.') {
            let module = &ctor[..ctor.len() - ctor_head.len() - 1];
            if ctor_head != class {
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
            return self.names(site, ctor_head, def.path, class);
        }
        Evidence::Candidate("receiver_unresolved")
    }

    /// Every reference `site` makes to `def`, as one finding: the strongest
    /// evidence among its occurrences, and the lines that carry it.
    pub fn classify(&self, site: &Site, def: &Definition) -> Option<Finding> {
        let words = self.names_for(site, def);
        let params = params_of(site.content);
        let python = lang_of(site.path) == Some(Lang::Python);
        let mut triple = None;
        let mut best: Option<Finding> = None;
        for (i, line) in site.content.lines().enumerate() {
            let code = crate::impact::mask_code(line, python, &mut triple);
            if is_import_line(&code) {
                continue;
            }
            let line_no = site.start_line + i as u32;
            for word in &words {
                for (at, _) in code.match_indices(word) {
                    if let Some((evidence, relation)) =
                        self.occurrence(site, def, &code, at, word, &params)
                    {
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
        let assigned = next.starts_with('=') && !next.starts_with("==");
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
        // attribute (`response.headers`); only a call is worth a guess.
        let unresolved_read = relation == Relation::Reference
            && matches!(evidence, Evidence::Candidate("receiver_unresolved"));
        (evidence != Evidence::Rejected && !unresolved_read).then_some((evidence, relation))
    }

    fn module_member(&self, site: &Site, chain: &str, def: &Definition) -> Evidence {
        if matches!(chain, "self" | "cls" | "this") || chain.is_empty() {
            Evidence::Rejected
        } else {
            self.chain_is_module(site, chain, def.path)
        }
    }
}

fn is_import_line(code: &str) -> bool {
    let head = code.trim_start();
    head.starts_with("import ")
        || head.starts_with("from ")
        || (head.starts_with("export ") && head.contains(" from "))
}

/// Folds one occurrence into the finding: stronger evidence replaces the
/// lines, equal evidence adds its line, a call outranks a reference.
fn absorb(best: &mut Option<Finding>, evidence: Evidence, relation: Relation, line: u32) {
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
fn constructed_as(content: &str, var: &str, python: bool) -> Option<String> {
    let re = Regex::new(&format!(
        r"(?:^|[^\w$.])({})\s*(?::\s*[\w$.\[\]<>]+\s*)?=\s*(?:new\s+|await\s+)?([\w$.]+)\s*\(",
        regex::escape(var)
    ))
    .ok()?;
    let mut triple = None;
    content.lines().find_map(|line| {
        let code = crate::impact::mask_code(line, python, &mut triple);
        entered_as(&code, var).or_else(|| {
            re.captures_iter(&code)
                .find(|c| c.get(1).is_some_and(|v| !after_as(&code, v.start())))
                .map(|c| c[2].to_string())
        })
    })
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
    }
}
