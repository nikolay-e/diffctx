//! JavaScript imports of an HTML page's scripts: `import`, `export … from`,
//! `require`, dynamic `import()`, and how a relative specifier lands on a file
//! of the snapshot.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::facts::{DeclKind, Declaration};

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

/// `const Card = lazy(() => import('./Card'))`, `React.lazy(async () =>
/// import('./Card'))`: `lazy` resolves the module's default export, so the
/// constant is that export (Forgejo #106).
static JS_LAZY_DEFAULT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)\b(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:React\s*\.\s*)?lazy\(\s*(?:async\s*)?\(\s*\)\s*=>\s*(?:await\s+)?import\(\s*['"]([^'"]+)['"]\s*\)\s*,?\s*\)"#,
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
    for c in JS_LAZY_DEFAULT.captures_iter(text) {
        b.names.insert(
            c[1].to_string(),
            Target::Member {
                module: c[2].to_string(),
                name: "default".to_string(),
            },
        );
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

    pub(super) fn resolve_js(&self, importer: &str, spec: &str) -> Resolution {
        // `./sw.js?raw`, `./w.ts?worker`: a bundler query names the same
        // file (#558); a leading `#` is a subpath import, not a fragment.
        let spec = spec.split('?').next().unwrap_or(spec);
        if spec.starts_with("./") || spec.starts_with("../") || spec == "." || spec == ".." {
            let joined = normalize(&PathBuf::from(parent_dir(importer)).join(spec));
            return self
                .probe_js(&joined)
                .map_or(Resolution::Unsupported, Resolution::File);
        }
        // A bare specifier of a page names a package or a bundler alias; the
        // projects' own configs are read by the resolver of the facts layer.
        let alias_like = spec.starts_with("@/")
            || spec.starts_with("~/")
            || spec.starts_with('#')
            || spec.starts_with('/');
        if alias_like {
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
/// writes, the names its default export designates, and what it declares.
#[derive(Debug, Default)]
pub(crate) struct JsFacts {
    /// In source order.
    pub(super) occurrences: Vec<Occurrence>,
    /// `export default Badge`, `export default memo(Badge)`,
    /// `export { Badge as default }`, `export default function Badge`.
    pub(super) default_names: Vec<String>,
    /// Modules loaded by pattern (#357).
    pub(super) globs: Vec<Glob>,
    /// Functions, classes and methods with the class and the function
    /// they sit in (#554), and as assignments what the fragmenter does not
    /// cut: a property of the global object or of the module's exports
    /// (#426), an interface or object type member, a class field (#488).
    pub(crate) declarations: Vec<Declaration>,
    /// What `importScripts(…)` loads into a worker, as written; `None` for
    /// an argument that is no string literal.
    pub(super) scripts: Vec<Option<String>>,
    /// A page's top-level script statements: in a `.js` file they are the
    /// fragmenter's gap chunks, in a page the script element holds them.
    pub(crate) script_statements: Vec<(u32, u32)>,
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

fn is_html(path: &str) -> bool {
    let ext = path.rsplit('.').next().map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("html" | "htm"))
}

/// The code of a file: an HTML page's is its inline scripts, at their own
/// positions in the page (#420).
pub(super) fn script_code<'t>(path: &str, text: &'t str) -> std::borrow::Cow<'t, str> {
    if is_html(path) {
        crate::parsers::inline_scripts(text)
            .unwrap_or_default()
            .into()
    } else {
        text.into()
    }
}

const FACTS_LIMIT: usize = 8192;

static FACTS: Lazy<std::sync::Mutex<FxHashMap<u64, Arc<JsFacts>>>> =
    Lazy::new(|| std::sync::Mutex::new(FxHashMap::default()));

/// The facts of one blob, parsed once per run whoever asks first: the
/// overlay of declarations, then the resolver. `None` when the grammar is
/// not compiled in or the parse timed out.
pub(crate) fn js_facts(path: &str, text: &str) -> Option<Arc<JsFacts>> {
    use std::hash::{Hash, Hasher};
    let key = {
        let mut h = rustc_hash::FxHasher::default();
        (grammar_of(path), is_html(path), text).hash(&mut h);
        h.finish()
    };
    if let Some(hit) = FACTS.lock().ok().and_then(|c| c.get(&key).cloned()) {
        return Some(hit);
    }
    let facts = Arc::new(read_js_facts(path, &script_code(path, text))?);
    if let Ok(mut cache) = FACTS.lock() {
        if cache.len() >= FACTS_LIMIT {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&facts));
    }
    Some(facts)
}

fn read_js_facts(path: &str, text: &str) -> Option<JsFacts> {
    let tree = crate::parsers::parse_tree(grammar_of(path), text)?;
    let bytes = text.as_bytes();
    let mut scopes: Vec<Scope> = vec![(None, FxHashMap::default())];
    // The lines of each scope's function: what a definition inside is
    // local to.
    let mut spans: Vec<Option<(u32, u32)>> = vec![None];
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
        let local_to = spans[scope];
        let mut define = |name: Option<tree_sitter::Node>, kind, class: Option<String>| {
            if let Some(name) = name.filter(|n| n.kind().ends_with("identifier")) {
                facts.declarations.push(Declaration {
                    name: name.utf8_text(bytes).unwrap_or("").to_string(),
                    kind,
                    lines: lines_of(node),
                    class,
                    scope: local_to,
                });
            }
        };
        if FUNCTIONS.contains(&kind) {
            let name = node.child_by_field_name("name");
            if kind == "method_definition" {
                if let Some(class) = class_of(node, bytes) {
                    define(name, DeclKind::Function, Some(class));
                }
            } else if kind.ends_with("_declaration") {
                define(name, DeclKind::Function, None);
            }
            scopes.push((Some(scope), FxHashMap::default()));
            spans.push(Some(lines_of(node)));
            inner = scopes.len() - 1;
            if let Some(name) = name.filter(|_| kind != "method_definition") {
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
                    let class = value.is_some_and(|v| v.kind() == "class");
                    let kind = if class {
                        DeclKind::Class
                    } else {
                        DeclKind::Function
                    };
                    define(node.child_by_field_name("name"), kind, None);
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
                if kind.ends_with("class_declaration") {
                    define(node.child_by_field_name("name"), DeclKind::Class, None);
                }
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
            "assignment_expression"
            | "property_signature"
            | "method_signature"
            | "public_field_definition"
            | "field_definition" => {
                if let Some((name, class, span)) = declared_member(node, bytes) {
                    declared.insert(name.start_byte());
                    facts.declarations.push(Declaration {
                        name: name.utf8_text(bytes).unwrap_or("").to_string(),
                        kind: DeclKind::Assignment,
                        lines: lines_of(span),
                        class,
                        scope: None,
                    });
                }
            }
            "call_expression" => {
                facts.globs.extend(glob_of(node, bytes));
                facts.scripts.extend(imported_scripts(node, bytes));
            }
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
    if is_html(path) {
        let root = tree.root_node();
        let mut cursor = root.walk();
        facts.script_statements = root
            .named_children(&mut cursor)
            .filter(|n| {
                let kind = n.kind();
                !(kind.ends_with("_declaration")
                    || kind == "comment"
                    || kind == "import_statement"
                    || kind == "export_statement")
            })
            .map(lines_of)
            .collect();
    }
    Some(facts)
}

/// A member a statement or a body declares: its name, its owner and the
/// span of the declaration. `self.x = …`, `globalThis.x`, `window.x` set a
/// property of the global object, which any script of the realm reaches
/// by name; `exports.x`, `module.exports.x` export `x` from the module
/// (#426). An interface or object type states its members, a class its
/// fields (#488).
fn declared_member<'t>(
    node: tree_sitter::Node<'t>,
    bytes: &[u8],
) -> Option<(tree_sitter::Node<'t>, Option<String>, tree_sitter::Node<'t>)> {
    if node.kind() == "assignment_expression" {
        let left = node
            .child_by_field_name("left")
            .filter(|l| l.kind() == "member_expression")?;
        let name = left
            .child_by_field_name("property")
            .filter(|p| p.kind() == "property_identifier")?;
        let class = match left.child_by_field_name("object")?.utf8_text(bytes).ok()? {
            o if super::GLOBAL_OBJECTS.contains(&o) => Some(super::GLOBAL_OWNER.to_string()),
            "exports" | "module.exports" => None,
            _ => return None,
        };
        let statement = node
            .parent()
            .filter(|p| p.kind() == "expression_statement")
            .unwrap_or(node);
        return Some((name, class, statement));
    }
    let name = node
        .child_by_field_name("name")
        .or_else(|| node.child_by_field_name("property"))
        .filter(|n| {
            matches!(
                n.kind(),
                "property_identifier" | "private_property_identifier"
            )
        })?;
    let class = match node.kind() {
        "public_field_definition" | "field_definition" => class_of(node, bytes)?,
        _ => {
            let body = node
                .parent()
                .filter(|b| matches!(b.kind(), "interface_body" | "object_type"))?;
            let owner = body.parent().filter(|o| {
                matches!(o.kind(), "interface_declaration" | "type_alias_declaration")
            })?;
            owner
                .child_by_field_name("name")?
                .utf8_text(bytes)
                .ok()?
                .to_string()
        }
    };
    Some((name, Some(class), node))
}

/// The named class whose body holds `member`; a method of an object
/// literal has none the syntax names.
fn class_of(member: tree_sitter::Node, bytes: &[u8]) -> Option<String> {
    let class = member
        .parent()
        .filter(|b| b.kind() == "class_body")?
        .parent()
        .filter(|c| {
            matches!(
                c.kind(),
                "class_declaration" | "abstract_class_declaration" | "class"
            )
        })?;
    Some(
        class
            .child_by_field_name("name")?
            .utf8_text(bytes)
            .ok()?
            .to_string(),
    )
}

fn lines_of(node: tree_sitter::Node) -> (u32, u32) {
    (
        node.start_position().row as u32 + 1,
        node.end_position().row as u32 + 1,
    )
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

/// The arguments of an `importScripts(…)` call.
fn imported_scripts(call: tree_sitter::Node, bytes: &[u8]) -> Vec<Option<String>> {
    let callee = call
        .child_by_field_name("function")
        .and_then(|f| f.utf8_text(bytes).ok());
    if !matches!(callee, Some("importScripts" | "self.importScripts")) {
        return Vec::new();
    }
    let Some(args) = call.child_by_field_name("arguments") else {
        return Vec::new();
    };
    let mut cursor = args.walk();
    args.named_children(&mut cursor)
        .map(|a| {
            (a.kind() == "string").then(|| {
                let t = a.utf8_text(bytes).unwrap_or("");
                t[1..t.len().saturating_sub(1).max(1)].to_string()
            })
        })
        .collect()
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
    pub(crate) fn js_facts(&self, path: &str) -> Option<Arc<JsFacts>> {
        if super::lang_of(path) != Some(super::Lang::Js) {
            return None;
        }
        if let Some(known) = self.js_facts.borrow().get(path) {
            return known.clone();
        }
        let facts = self.text(path).and_then(|t| js_facts(path, &t));
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
            words.extend(self.import_locals(path, def_path, name));
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
        if occurrence.declares || occurrence.member || shadowed {
            return None;
        }
        let relation = if next.starts_with('(') || next.starts_with("?.(") {
            Relation::Call
        } else {
            Relation::Reference
        };
        let evidence = match def.class {
            None => self.names(site, &occurrence.name, def.path, def.name),
            // A global is what a bare name means where no import and no
            // declaration of the file's own binds it (#426).
            Some(super::GLOBAL_OWNER)
                if !self
                    .bindings(site.path)
                    .names
                    .contains_key(&*occurrence.name)
                    && !self.js_declares(site.path, &occurrence.name) =>
            {
                self.global_reach(site.path, def.path)
            }
            Some(_) => return None,
        };
        (evidence != Evidence::Rejected).then_some((evidence, relation))
    }
}

impl Resolver<'_> {
    /// The locals `path` imports under another name that may stand for
    /// `name` of `def_path`: a named import of it, or a default import
    /// when it is its module's default export. A default import of an
    /// unresolved module named every changed definition (#558).
    pub(super) fn import_locals(&self, path: &str, def_path: &str, name: &str) -> Vec<String> {
        let mut default = None;
        self.bindings(path)
            .names
            .iter()
            .filter(|(local, t)| match t {
                Target::Member { name: n, .. } if n == name => *local != name,
                Target::Member { name: n, .. } if n == "default" => {
                    *default.get_or_insert_with(|| self.default_export_is(def_path, name))
                }
                _ => false,
            })
            .map(|(local, _)| local.clone())
            .collect()
    }

    /// What a use in `site` says of a property `def` sets on the global
    /// object: its own file's; a worker's when its `importScripts` load
    /// that file, or a script that loads it; none of a worker whose
    /// scripts all land elsewhere. Any other script reaches it once the
    /// realm has loaded the file, which no import states (#426).
    pub(super) fn global_reach(&self, site: &str, def: &str) -> Evidence {
        if site == def {
            return Evidence::Resolved;
        }
        let loads = self.js_facts(site).is_some_and(|f| !f.scripts.is_empty());
        match self.loads(site, def, 0) {
            Some(found) if loads => found,
            _ => Evidence::Candidate("global"),
        }
    }

    /// Whether the scripts `worker` imports load `target`; `None` when one
    /// of them lands on no file this can name.
    fn loads(&self, worker: &str, target: &str, depth: usize) -> Option<Evidence> {
        let facts = self.js_facts(worker)?;
        let mut known = true;
        for spec in &facts.scripts {
            match spec.as_deref().and_then(|s| self.script_file(worker, s)) {
                Some(f) if f == target => return Some(Evidence::Resolved),
                Some(f) if f.is_empty() => {}
                Some(f) if depth < MAX_REEXPORT_DEPTH => match self.loads(&f, target, depth + 1) {
                    Some(Evidence::Resolved) => return Some(Evidence::Resolved),
                    None => known = false,
                    _ => {}
                },
                _ => known = false,
            }
        }
        known.then_some(Evidence::Rejected)
    }

    /// The file a script URL lands on: relative to the worker, or, rooted
    /// at a served directory this cannot see, the one file whose path ends
    /// with it. `""` for another origin's.
    fn script_file(&self, worker: &str, spec: &str) -> Option<String> {
        let spec = spec.split(['?', '#']).next().unwrap_or(spec);
        if spec.contains("://") || spec.starts_with("//") {
            return Some(String::new());
        }
        if let Some(rooted) = spec.strip_prefix('/') {
            let tail = format!("/{rooted}");
            let mut hits = self
                .files
                .iter()
                .filter(|f| f.as_str() == rooted || f.ends_with(&tail));
            return match (hits.next(), hits.next()) {
                (Some(one), None) => Some(one.clone()),
                _ => None,
            };
        }
        let joined = normalize(&Path::new(parent_dir(worker)).join(spec));
        self.files.contains(&joined).then_some(joined)
    }

    /// Whether `path` declares `name` at its top level.
    fn js_declares(&self, path: &str, name: &str) -> bool {
        self.js_facts(path).is_some_and(|f| {
            f.occurrences
                .iter()
                .any(|o| o.declares && !o.member && o.local.is_none() && &*o.name == name)
        })
    }

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
