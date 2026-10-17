//! JavaScript/TypeScript imports: `import`, `export … from`, `require`,
//! dynamic `import()`, and how a specifier lands on a file of the snapshot
//! through relative paths, tsconfig `paths` and workspace packages.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::source::Source;

use super::{
    Definition, Evidence, FileBindings, MAX_REEXPORT_DEPTH, Relation, Resolution, Resolver, Site,
    Target, normalize, parent_dir,
};

static JS_IMPORT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)\bimport\s+(type\s+)?([^'"`;]*?)\s+from\s+['"]([^'"]+)['"]"#).unwrap()
});
static JS_EXPORT_FROM: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?s)\bexport\s+(type\s+)?(\*|\*\s+as\s+\w+|\{[^}]*\})\s+from\s+['"]([^'"]+)['"]"#)
        .unwrap()
});
static JS_REQUIRE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)\b(?:const|let|var)\s+(\{[^}]*\}|[\w$]+)\s*=\s*(?:require|await\s+import)\(\s*['"]([^'"]+)['"]\s*\)"#,
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

/// `import('./x').then(m => …)`, `.then((m) => …)`, `.then(function (m)`:
/// the callback's parameter is the module (#479, `React.lazy`).
static JS_DYNAMIC_THEN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"\bimport\(\s*['"]([^'"]+)['"]\s*\)\s*\.then\(\s*(?:async\s+)?(?:function\s*)?\(?\s*([A-Za-z_$][\w$]*)"#,
    )
    .unwrap()
});

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
    for c in JS_DYNAMIC_THEN.captures_iter(text) {
        b.names
            .insert(c[2].to_string(), Target::Module(c[1].to_string()));
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
pub(super) struct TsConfigs {
    by_dir: FxHashMap<String, TsPaths>,
    /// Vite's `resolve.alias` of each `vite.config.*`/`vitest.config.*`,
    /// by directory: a bundler alias with no tsconfig `paths` twin read as
    /// an npm package, and its importers vanished (#372).
    vite: FxHashMap<String, ViteAliases>,
}

/// `find` → the directory it stands for; `readable` is false when an entry
/// could not be read (a regex `find`, a computed replacement).
#[derive(Default)]
struct ViteAliases {
    aliases: Vec<(String, String)>,
    readable: bool,
}

fn is_vite_config(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name.starts_with("vite.config.") || name.starts_with("vitest.config."))
        && [".ts", ".js", ".mts", ".mjs", ".cts", ".cjs"]
            .iter()
            .any(|e| name.ends_with(e))
}

/// The aliases a Vite config's syntax states: `alias: { '@pkg': path.
/// resolve(__dirname, '../pkg') }` or `alias: [{ find, replacement }]`.
/// A replacement is the config's directory joined with the string literals
/// it is built from.
fn vite_aliases(path: &str, text: &str) -> ViteAliases {
    let mut out = ViteAliases {
        readable: true,
        ..ViteAliases::default()
    };
    let Some(tree) = crate::parsers::parse_tree(grammar_of(path), text) else {
        out.readable = false;
        return out;
    };
    let bytes = text.as_bytes();
    let dir = parent_dir(path);
    let key = |n: tree_sitter::Node| {
        let t = n.utf8_text(bytes).unwrap_or("");
        match n.kind() {
            "string" => Some(t[1..t.len().saturating_sub(1).max(1)].to_string()),
            "property_identifier" => Some(t.to_string()),
            _ => None,
        }
    };
    let target = |v: tree_sitter::Node| -> Option<String> {
        let mut parts = Vec::new();
        let mut stack = vec![v];
        while let Some(n) = stack.pop() {
            if n.kind() == "string" {
                parts.extend(key(n));
                continue;
            }
            let mut c = n.walk();
            let at = stack.len();
            stack.extend(n.named_children(&mut c));
            stack[at..].reverse();
        }
        let rel = parts.join("/");
        (!parts.is_empty() && !rel.starts_with('/')).then(|| normalize(&Path::new(dir).join(rel)))
    };
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let alias = node.kind() == "pair"
            && node.child_by_field_name("key").and_then(key).as_deref() == Some("alias");
        let value = node.child_by_field_name("value").filter(|_| alias);
        match value.map(|v| (v.kind(), v)) {
            Some(("object", obj)) => {
                let mut c = obj.walk();
                for pair in obj.named_children(&mut c).filter(|p| p.kind() == "pair") {
                    let find = pair.child_by_field_name("key").and_then(key);
                    let to = pair.child_by_field_name("value").and_then(target);
                    match (find, to) {
                        (Some(f), Some(t)) => out.aliases.push((f, t)),
                        _ => out.readable = false,
                    }
                }
            }
            Some(("array", arr)) => {
                let mut c = arr.walk();
                for entry in arr.named_children(&mut c) {
                    let field = |name: &str| {
                        let mut c = entry.walk();
                        entry
                            .named_children(&mut c)
                            .filter(|p| p.kind() == "pair")
                            .find(|p| {
                                p.child_by_field_name("key").and_then(key).as_deref() == Some(name)
                            })
                            .and_then(|p| p.child_by_field_name("value"))
                    };
                    let find = field("find").and_then(key);
                    let to = field("replacement").and_then(target);
                    match (find, to) {
                        (Some(f), Some(t)) => out.aliases.push((f, t)),
                        _ => out.readable = false,
                    }
                }
            }
            Some(_) => out.readable = false,
            None => {
                let mut c = node.walk();
                stack.extend(node.named_children(&mut c));
            }
        }
    }
    out
}

fn is_tsconfig(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name.starts_with("tsconfig") || name.starts_with("jsconfig")) && name.ends_with(".json")
}

impl TsConfigs {
    pub(super) fn read(source: &Source, root: &Path, files: &FxHashSet<String>) -> Self {
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
        let vite = files
            .iter()
            .filter(|f| is_vite_config(f) && !f.contains("node_modules/"))
            .filter_map(|f| {
                let text = source.read_to_string(&root.join(f))?;
                Some((parent_dir(f).to_string(), vite_aliases(f, &text)))
            })
            .fold(
                FxHashMap::default(),
                |mut acc: FxHashMap<String, ViteAliases>, (dir, read)| {
                    let entry = acc.entry(dir).or_insert_with(|| ViteAliases {
                        readable: true,
                        ..ViteAliases::default()
                    });
                    entry.aliases.extend(read.aliases);
                    entry.readable &= read.readable;
                    acc
                },
            );
        Self { by_dir, vite }
    }

    fn vite_for(&self, importer: &str) -> Option<&ViteAliases> {
        let mut dir = parent_dir(importer);
        loop {
            if let Some(found) = self.vite.get(dir) {
                return Some(found);
            }
            if dir.is_empty() {
                return None;
            }
            dir = parent_dir(dir);
        }
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

impl Resolver<'_> {
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

    /// A specifier naming one of the repository's own packages — an npm
    /// workspace member — resolves into its source, not to an external
    /// dependency (#474).
    fn workspace_package(&self, spec: &str) -> Option<String> {
        let packages = self.workspace.get_or_init(|| {
            let mut out = FxHashMap::default();
            for file in self.files.iter().filter(|f| {
                (f.as_str() == "package.json" || f.ends_with("/package.json"))
                    && !f.contains("node_modules/")
            }) {
                let Some(doc) = self
                    .source
                    .read_to_string(&self.root.join(file))
                    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                else {
                    continue;
                };
                let Some(name) = doc["name"].as_str() else {
                    continue;
                };
                let exports = &doc["exports"];
                let dot = exports.get(".").unwrap_or(exports);
                let entries: Vec<String> = [
                    doc["source"].as_str(),
                    dot.as_str(),
                    dot["import"].as_str(),
                    dot["default"].as_str(),
                    doc["module"].as_str(),
                    doc["main"].as_str(),
                ]
                .into_iter()
                .flatten()
                .map(|e| e.trim_start_matches("./").to_string())
                .collect();
                out.insert(name.to_string(), (parent_dir(file).to_string(), entries));
            }
            out
        });
        let (name, (dir, entries)) = packages
            .iter()
            .filter(|(n, _)| spec == n.as_str() || spec.starts_with(&format!("{n}/")))
            .max_by_key(|(n, _)| n.len())?;
        let at = |rel: &str| normalize(&Path::new(dir).join(rel));
        let sub = spec[name.len()..].trim_start_matches('/');
        if sub.is_empty() {
            entries
                .iter()
                .map(|e| at(e))
                .chain(["src/index", "index"].map(at))
                .find_map(|p| self.probe_js(&p))
        } else {
            [at(sub), at(&format!("src/{sub}"))]
                .iter()
                .find_map(|p| self.probe_js(p))
        }
    }

    pub(super) fn resolve_js(&self, importer: &str, spec: &str) -> Resolution {
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
        let vite = self.ts_configs.vite_for(importer);
        if let Some((find, to)) = vite.and_then(|v| {
            v.aliases
                .iter()
                .filter(|(f, _)| {
                    spec == f || spec.starts_with(&format!("{}/", f.trim_end_matches('/')))
                })
                .max_by_key(|(f, _)| f.len())
        }) {
            let rest = spec[find.len()..].trim_start_matches('/');
            return self
                .probe_js(&normalize(&Path::new(to).join(rest)))
                .map_or(Resolution::Unsupported, Resolution::File);
        }
        if let Some(base) = &ts.base_url {
            let based = normalize(&Path::new(base).join(spec));
            if let Some(found) = self.probe_js(&based) {
                return Resolution::File(found);
            }
        }
        if let Some(found) = self.workspace_package(spec) {
            return Resolution::File(found);
        }
        let alias_like = spec.starts_with("@/")
            || spec.starts_with("~/")
            || spec.starts_with('#')
            || spec.starts_with('/');
        if alias_like || !ts.readable || vite.is_some_and(|v| !v.readable) {
            Resolution::Unsupported
        } else {
            Resolution::External
        }
    }
}

/// An identifier the grammar shows outside comments, strings and JSX text.
#[derive(Debug)]
pub(super) struct Occurrence {
    pub name: Box<str>,
    pub line: u32,
    pub col: u32,
    /// The name a declaration introduces: a variable, a parameter, an
    /// imported or declared function or class.
    pub declares: bool,
    /// Bound by a declaration of an enclosing function: no other file's
    /// definition reaches it.
    pub local: Option<Local>,
    /// A property: `x.name`, an object key, a class member.
    pub member: bool,
}

/// What one JS/TS file says in its syntax (#506): the identifiers it
/// writes, and the names its default export designates.
#[derive(Debug, Default)]
pub(super) struct JsFacts {
    /// In source order.
    pub occurrences: Vec<Occurrence>,
    /// `export default Badge`, `export default memo(Badge)`,
    /// `export { Badge as default }`, `export default function Badge`.
    pub default_names: Vec<String>,
    /// Modules loaded by pattern (#357).
    pub globs: Vec<Glob>,
    lines: Vec<Box<str>>,
}

/// `import.meta.glob(['./a/*.ts', '!./a/x.ts'])` (Vite) or
/// `require.context('./a', true, /\.ts$/)` (webpack), as written.
#[derive(Debug)]
pub(super) enum Glob {
    Patterns(Vec<String>),
    Context {
        dir: String,
        recursive: bool,
        filter: Option<String>,
    },
}

impl JsFacts {
    /// The occurrence of `word` at `col` of `line`, when `text` is that
    /// line as the file holds it.
    pub(super) fn at(&self, line: u32, col: usize, word: &str, text: &str) -> Lookup<'_> {
        let same_line = self
            .lines
            .get(line.wrapping_sub(1) as usize)
            .is_some_and(|l| **l == *text);
        if !same_line {
            return Lookup::Unknown;
        }
        let first = self.occurrences.partition_point(|o| o.line < line);
        self.occurrences[first..]
            .iter()
            .take_while(|o| o.line == line)
            .find(|o| o.col as usize == col && &*o.name == word)
            .map_or(Lookup::NotCode, Lookup::Found)
    }
}

/// What an enclosing function binds a local name to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Local {
    /// A parameter or a variable holding a value.
    Value,
    /// A nested function: the definition a caller in its own file reaches.
    Function,
}

pub(super) enum Lookup<'f> {
    Found(&'f Occurrence),
    /// The grammar has no identifier there: JSX text, part of a longer
    /// name, a regex literal.
    NotCode,
    /// The text is not the file's line: the syntax cannot say.
    Unknown,
}

fn grammar_of(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("ts" | "mts" | "cts") => "typescript",
        Some("tsx") => "tsx",
        _ => "javascript",
    }
}

const FUNCTIONS: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "function",
    "generator_function",
    "arrow_function",
    "method_definition",
];

pub(super) fn js_facts(path: &str, text: &str) -> Option<JsFacts> {
    let tree = crate::parsers::parse_tree(grammar_of(path), text)?;
    let bytes = text.as_bytes();
    let mut scopes: Vec<Scope> = vec![(None, FxHashMap::default())];
    let mut declared: FxHashSet<usize> = FxHashSet::default();
    let mut found: Vec<(tree_sitter::Node, usize)> = Vec::new();
    let mut facts = JsFacts {
        lines: text.lines().map(Box::from).collect(),
        ..JsFacts::default()
    };
    let mut stack = vec![(tree.root_node(), 0usize)];
    while let Some((node, scope)) = stack.pop() {
        let mut inner = scope;
        let kind = node.kind();
        if FUNCTIONS.contains(&kind) {
            scopes.push((Some(scope), FxHashMap::default()));
            inner = scopes.len() - 1;
            if let Some(name) = node
                .child_by_field_name("name")
                .filter(|_| kind != "method_definition")
            {
                let owner = (kind.ends_with("_declaration") && scope != 0)
                    .then_some((scope, Local::Function));
                declare(name, bytes, owner, &mut scopes, &mut declared);
            }
            if let Some(params) = node
                .child_by_field_name("parameters")
                .or_else(|| node.child_by_field_name("parameter"))
            {
                for name in pattern_names(params) {
                    declare(
                        name,
                        bytes,
                        Some((inner, Local::Value)),
                        &mut scopes,
                        &mut declared,
                    );
                }
            }
        }
        match kind {
            "variable_declarator" => {
                // `const helper = () => …` defines a function, like a
                // declaration; `const { f } = await import('./m')` binds
                // the module's own, as an import does.
                let value = node.child_by_field_name("value");
                let local = if value
                    .is_some_and(|v| FUNCTIONS.contains(&v.kind()) || v.kind() == "class")
                {
                    Some(Local::Function)
                } else if value.is_some_and(|v| imports_module(v, bytes)) {
                    None
                } else {
                    Some(Local::Value)
                };
                if let Some(name) = node.child_by_field_name("name") {
                    for n in pattern_names(name) {
                        let owner = local.filter(|_| scope != 0).map(|l| (scope, l));
                        declare(n, bytes, owner, &mut scopes, &mut declared);
                    }
                }
            }
            "class_declaration"
            | "abstract_class_declaration"
            | "interface_declaration"
            | "type_alias_declaration"
            | "enum_declaration" => {
                if let Some(name) = node.child_by_field_name("name") {
                    declare(name, bytes, None, &mut scopes, &mut declared);
                }
            }
            "catch_clause" | "for_in_statement" => {
                let field = if kind == "catch_clause" {
                    "parameter"
                } else {
                    "left"
                };
                if let Some(p) = node.child_by_field_name(field) {
                    for n in pattern_names(p) {
                        let owner = (scope != 0).then_some((scope, Local::Value));
                        declare(n, bytes, owner, &mut scopes, &mut declared);
                    }
                }
            }
            "import_statement" => {
                let mut todo = vec![node];
                while let Some(n) = todo.pop() {
                    if n.kind() == "identifier" {
                        declared.insert(n.start_byte());
                    }
                    let mut cursor = n.walk();
                    todo.extend(n.named_children(&mut cursor));
                }
            }
            "export_statement" => default_export(node, bytes, &mut facts.default_names),
            "call_expression" => facts.globs.extend(glob_of(node, bytes)),
            "identifier"
            | "shorthand_property_identifier"
            | "type_identifier"
            | "property_identifier"
            | "private_property_identifier" => {
                found.push((node, scope));
            }
            // Comments, strings and JSX text hold no identifiers; a
            // template's `${…}` is an expression and is walked.
            "comment" | "string" | "regex" | "jsx_text" => continue,
            _ => {}
        }
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor).map(|c| (c, inner)));
    }
    for (node, scope) in found {
        let name = node.utf8_text(bytes).unwrap_or("");
        let member = matches!(
            node.kind(),
            "property_identifier" | "private_property_identifier"
        );
        let mut local = None;
        let mut at = Some(scope);
        while let Some(s) = at {
            let (parent, names) = &scopes[s];
            if let Some(kind) = names.get(name) {
                local = Some(*kind);
                break;
            }
            at = *parent;
        }
        facts.occurrences.push(Occurrence {
            name: name.into(),
            line: node.start_position().row as u32 + 1,
            col: node.start_position().column as u32,
            declares: declared.contains(&node.start_byte()),
            local: local.filter(|_| !member),
            member,
        });
    }
    facts.occurrences.sort_by_key(|o| (o.line, o.col));
    Some(facts)
}

/// The pattern a glob-importing call states, when its arguments are
/// literals; a computed pattern loads nothing this can name.
fn glob_of(call: tree_sitter::Node, bytes: &[u8]) -> Option<Glob> {
    let callee = call
        .child_by_field_name("function")?
        .utf8_text(bytes)
        .ok()?;
    let args = call.child_by_field_name("arguments")?;
    let mut cursor = args.walk();
    let args: Vec<tree_sitter::Node> = args.named_children(&mut cursor).collect();
    let literal = |n: &tree_sitter::Node| {
        (n.kind() == "string").then(|| {
            let t = n.utf8_text(bytes).unwrap_or("");
            t[1..t.len().saturating_sub(1).max(1)].to_string()
        })
    };
    match callee {
        "import.meta.glob" | "import.meta.globEager" => {
            let first = args.first()?;
            let patterns: Vec<String> = if first.kind() == "array" {
                let mut c = first.walk();
                first
                    .named_children(&mut c)
                    .filter_map(|n| literal(&n))
                    .collect()
            } else {
                vec![literal(first)?]
            };
            Some(Glob::Patterns(patterns))
        }
        "require.context" => Some(Glob::Context {
            dir: literal(args.first()?)?,
            recursive: args.get(1).is_none_or(|n| n.kind() != "false"),
            filter: args.get(2).and_then(|n| {
                (n.kind() == "regex")
                    .then(|| n.child_by_field_name("pattern"))
                    .flatten()
                    .and_then(|p| p.utf8_text(bytes).ok())
                    .map(str::to_string)
            }),
        }),
        _ => None,
    }
}

/// A Vite glob as a path regex: `**/` any directories, `*` and `?` within
/// one, `{a,b}` either.
fn glob_regex(pattern: &str) -> Option<Regex> {
    let mut re = String::from("^");
    let mut braces = 0;
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' if chars.peek() == Some(&'*') => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    chars.next();
                    re.push_str("(?:.*/)?");
                } else {
                    re.push_str(".*");
                }
            }
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => {
                braces += 1;
                re.push_str("(?:");
            }
            '}' if braces > 0 => {
                braces -= 1;
                re.push(')');
            }
            ',' if braces > 0 => re.push('|'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    Regex::new(&re).ok()
}

/// `import('./m')`, `await import('./m')`, `require('./m')`.
fn imports_module(value: tree_sitter::Node, bytes: &[u8]) -> bool {
    let call = if value.kind() == "await_expression" {
        value.named_child(0)
    } else {
        Some(value)
    };
    call.filter(|c| c.kind() == "call_expression")
        .and_then(|c| c.child_by_field_name("function"))
        .is_some_and(|f| f.kind() == "import" || f.utf8_text(bytes) == Ok("require"))
}

/// A function's parent scope and the names it binds; the module is the
/// root, whose names are no locals.
type Scope = (Option<usize>, FxHashMap<String, Local>);

/// Marks `name` as declaring, and as a local of a function scope.
fn declare(
    name: tree_sitter::Node,
    bytes: &[u8],
    owner: Option<(usize, Local)>,
    scopes: &mut [Scope],
    declared: &mut FxHashSet<usize>,
) {
    declared.insert(name.start_byte());
    if let (Some((scope, local)), Ok(text)) = (owner, name.utf8_text(bytes)) {
        scopes[scope].1.insert(text.to_string(), local);
    }
}

/// The names a binding pattern introduces: `x`, `{ a, b: c, d = 1 }`,
/// `[e, ...f]`, a typed parameter. Default values are expressions, not
/// names.
fn pattern_names(node: tree_sitter::Node) -> Vec<tree_sitter::Node> {
    let mut out = Vec::new();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        match n.kind() {
            "identifier" | "shorthand_property_identifier_pattern" => out.push(n),
            "pair_pattern" => stack.extend(n.child_by_field_name("value")),
            "assignment_pattern" | "object_assignment_pattern" => {
                stack.extend(n.child_by_field_name("left"));
            }
            "required_parameter" | "optional_parameter" => {
                stack.extend(n.child_by_field_name("pattern"));
            }
            "formal_parameters" | "object_pattern" | "array_pattern" | "rest_pattern" => {
                let mut cursor = n.walk();
                stack.extend(n.named_children(&mut cursor));
            }
            _ => {}
        }
    }
    out
}

/// The local names an `export default` statement designates: the value
/// itself, or what a wrapper call is given — `memo(Badge)`,
/// `forwardRef(Badge)`, `connect(map)(Badge)`.
fn default_export(node: tree_sitter::Node, bytes: &[u8], out: &mut Vec<String>) {
    let text = |n: tree_sitter::Node| n.utf8_text(bytes).unwrap_or("").to_string();
    let mut cursor = node.walk();
    let is_default = node.children(&mut cursor).any(|c| c.kind() == "default");
    if let Some(decl) = node
        .child_by_field_name("declaration")
        .filter(|_| is_default)
    {
        out.extend(decl.child_by_field_name("name").map(text));
    }
    if let Some(value) = node.child_by_field_name("value") {
        let mut stack = vec![value];
        while let Some(v) = stack.pop() {
            match v.kind() {
                "identifier" => out.push(text(v)),
                "call_expression" => {
                    if let Some(args) = v.child_by_field_name("arguments") {
                        let mut c = args.walk();
                        stack.extend(args.named_children(&mut c));
                    }
                }
                "parenthesized_expression" | "as_expression" | "satisfies_expression" => {
                    stack.extend(v.named_child(0));
                }
                _ => {}
            }
        }
    }
    // `export { Badge as default }`
    let mut cursor = node.walk();
    for clause in node
        .children(&mut cursor)
        .filter(|c| c.kind() == "export_clause")
    {
        let mut c = clause.walk();
        for spec in clause.named_children(&mut c) {
            let alias = spec.child_by_field_name("alias").map(text);
            if alias.as_deref() == Some("default") {
                out.extend(spec.child_by_field_name("name").map(text));
            }
        }
    }
}

impl Resolver<'_> {
    pub(super) fn js_facts(&self, path: &str) -> Option<Rc<JsFacts>> {
        if super::lang_of(path) != Some(super::Lang::Js) {
            return None;
        }
        if let Some(known) = self.js_facts.borrow().get(path) {
            return known.clone();
        }
        let facts = self
            .text(path)
            .and_then(|t| js_facts(path, &t))
            .map(Rc::new);
        self.js_facts
            .borrow_mut()
            .insert(path.to_string(), facts.clone());
        facts
    }

    /// The lines of `path` where a use of `name` defined in `def_path` can
    /// sit, read off the syntax rather than a ranking edge (#476): every
    /// identifier spelling it or a local an import binds to it, minus
    /// declarations and an enclosing function's own locals. A member is
    /// looked for only where its file is in reach — its own file, or one
    /// importing it.
    pub fn js_use_lines(&self, path: &str, def_path: &str, name: &str, member: bool) -> Vec<u32> {
        if member && path != def_path && !self.imports_file(path, def_path) {
            return Vec::new();
        }
        let mut words = vec![name.to_string()];
        if !member {
            words.extend(
                self.bindings(path)
                    .names
                    .iter()
                    .filter_map(|(local, t)| match t {
                        Target::Member { name: n, .. } if n == name || n == "default" => {
                            Some(local.clone())
                        }
                        _ => None,
                    }),
            );
        }
        let Some(text) = self.text(path) else {
            return Vec::new();
        };
        if !words.iter().any(|w| text.contains(w.as_str())) {
            return Vec::new();
        }
        let Some(facts) = self.js_facts(path) else {
            return Vec::new();
        };
        let mut lines: Vec<u32> = facts
            .occurrences
            .iter()
            .filter(|o| {
                !o.declares
                    && (o.local.is_none() || (o.local == Some(Local::Function) && path == def_path))
                    && words.iter().any(|w| *w == *o.name)
            })
            .map(|o| o.line)
            .collect();
        lines.dedup();
        lines
    }
}

impl Resolver<'_> {
    /// `Ui.Panel` where `Ui` is a name `module` exports: a namespace it
    /// re-exports (`export * as Ui from './Card'`) or imported and passed
    /// on, followed to the module it stands for (#504).
    pub(super) fn js_namespace_is(
        &self,
        importer: &str,
        module: &str,
        name: &str,
        path: &str,
        depth: usize,
    ) -> Evidence {
        match self.resolve(importer, module) {
            Resolution::File(f) => match self.bindings(&f).names.get(name) {
                Some(Target::Module(m)) => self.module_is(&f, m, path),
                Some(Target::Member { module, name }) if depth < MAX_REEXPORT_DEPTH => {
                    self.js_namespace_is(&f, module, name, path, depth + 1)
                }
                _ => Evidence::Rejected,
            },
            Resolution::Unsupported => Evidence::Candidate("import_unresolved"),
            _ => Evidence::Rejected,
        }
    }

    /// A bare identifier the grammar placed: a declaration, a property key
    /// or an enclosing function's own local is no use of another
    /// definition; anything else is, read or written — `executing = v`
    /// assigns the module's binding (#503).
    pub(super) fn js_bare_use(
        &self,
        site: &Site,
        def: &Definition,
        occurrence: &Occurrence,
        next: &str,
    ) -> Option<(Evidence, Relation)> {
        // A nested helper is the definition a caller in its own file
        // reaches; a parameter or a local value shadows any other.
        let shadowed = match occurrence.local {
            Some(Local::Value) => true,
            Some(Local::Function) => site.path != def.path,
            None => false,
        };
        if occurrence.declares || occurrence.member || shadowed || def.class.is_some() {
            return None;
        }
        let relation = if next.starts_with('(') || next.starts_with("?.(") {
            Relation::Call
        } else {
            Relation::Reference
        };
        let evidence = self.names(site, &occurrence.name, def.path, def.name);
        (evidence != Evidence::Rejected).then_some((evidence, relation))
    }
}

impl Resolver<'_> {
    /// Whether a glob import of `importer` loads the file at `path`.
    pub(super) fn js_glob_loads(&self, importer: &str, path: &str) -> bool {
        let Some(facts) = self.js_facts(importer) else {
            return false;
        };
        let dir = parent_dir(importer);
        // `/src/x/*.ts` is rooted at the project the importer sits in.
        let project = {
            let mut d = dir;
            loop {
                let manifest = if d.is_empty() {
                    "package.json".to_string()
                } else {
                    format!("{d}/package.json")
                };
                if d.is_empty() || self.files.contains(&manifest) {
                    break d;
                }
                d = parent_dir(d);
            }
        };
        let place = |p: &str| match p.strip_prefix('/') {
            Some(rooted) => normalize(&Path::new(project).join(rooted)),
            None => normalize(&Path::new(dir).join(p)),
        };
        facts.globs.iter().any(|glob| match glob {
            Glob::Patterns(patterns) => {
                let hits = |negated: bool| {
                    patterns
                        .iter()
                        .filter(|p| p.starts_with('!') == negated)
                        .filter_map(|p| glob_regex(&place(p.trim_start_matches('!'))))
                        .any(|re| re.is_match(path))
                };
                hits(false) && !hits(true)
            }
            Glob::Context {
                dir: base,
                recursive,
                filter,
            } => {
                let base = place(base);
                let Some(rest) = path.strip_prefix(&format!("{base}/")) else {
                    return false;
                };
                (*recursive || !rest.contains('/'))
                    && filter.as_deref().is_none_or(|f| {
                        Regex::new(f).map_or(true, |re| re.is_match(&format!("./{rest}")))
                    })
            }
        })
    }
}
