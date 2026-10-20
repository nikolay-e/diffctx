//! Shell: a function is called by its name in command position — at top
//! level, in a function, a pipeline, a condition, a `case` arm or a command
//! substitution (#541) — from its own script or from one that sources its
//! file (#533), read off the bash syntax tree.

use std::cell::{OnceCell, RefCell};
use std::path::Path;
use std::rc::Rc;

use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::Node;

use super::{At, DefId, LanguageFacts, Relation, Table, UseSite};
use crate::bindings::{Evidence, Finding, Relation as Use};
use crate::impact::base_symbol;
use crate::source::Source;

/// What a path spelled in a script starts from; the literal rest follows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Head {
    /// Nothing: the path is literal, relative to wherever the script runs.
    Literal,
    /// The script's own directory: `$(dirname "$0")`, `${BASH_SOURCE%/*}`.
    ScriptDir,
    /// A value nothing here reads: `$LIB_DIR`, `$(brew --prefix)`.
    Unknown,
}

#[derive(Clone, PartialEq, Eq)]
struct Spelled {
    head: Head,
    tail: String,
}

impl Spelled {
    fn literal(text: &str) -> Self {
        Self {
            head: Head::Literal,
            tail: text.to_string(),
        }
    }

    fn unknown() -> Self {
        Self {
            head: Head::Unknown,
            tail: String::new(),
        }
    }

    fn then(mut self, next: Self) -> Self {
        match next.head {
            Head::Literal => self.tail.push_str(&next.tail),
            Head::ScriptDir if self.head == Head::Literal && self.tail.is_empty() => return next,
            _ => return Self::unknown(),
        }
        self
    }
}

#[derive(Default)]
struct ShellFacts {
    functions: Vec<String>,
    /// The command word of every simple command, and its line.
    calls: Vec<(String, u32)>,
    sources: Vec<Spelled>,
    /// Text the grammar could not read: a call there is not seen.
    unread: Vec<String>,
}

/// Where a sourced path leads in the repository.
enum Target {
    File(String),
    /// A file name several files of the repository carry.
    Ambiguous(Vec<String>),
    /// Nothing the repository holds: `/etc/os-release`, `$HOME/.cargo/env`.
    Outside,
    /// No file name to go by: `source "$CONFIG"`.
    Unknown,
}

/// How a script reaches a library through what it sources, transitively.
#[derive(PartialEq, Eq)]
enum Reach {
    Bound,
    Ambiguous,
    /// Another file it sources defines the name: the call is that one's.
    Shadowed,
    /// It sources something unknown, which may define the name.
    Unknown,
    None,
}

fn read(text: &str) -> Option<ShellFacts> {
    let tree = crate::parsers::parse_tree("bash", text)?;
    let src = text.as_bytes();
    let text_of = |n: Node| n.utf8_text(src).unwrap_or("");
    let mut facts = ShellFacts::default();
    let mut assigned: FxHashMap<&str, Vec<Node>> = FxHashMap::default();
    let mut sourced: Vec<Node> = Vec::new();
    let mut stack = vec![tree.root_node()];
    while let Some(node) = stack.pop() {
        let mut cursor = node.walk();
        stack.extend(node.children(&mut cursor));
        match node.kind() {
            "ERROR" => facts.unread.push(text_of(node).to_string()),
            "function_definition" => {
                if let Some(name) = node.child_by_field_name("name") {
                    facts.functions.push(text_of(name).to_string());
                }
            }
            "variable_assignment" => {
                if let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) {
                    assigned.entry(text_of(name)).or_default().push(value);
                }
            }
            "command" => {
                let Some(name) = node.child_by_field_name("name") else {
                    continue;
                };
                let word = text_of(name);
                let line = node.start_position().row as u32 + 1;
                if matches!(word, "source" | ".") {
                    if let Some(arg) = node.child_by_field_name("argument") {
                        sourced.push(arg);
                    }
                } else if matches!(word, "time" | "eval") {
                    // These run the command after them in this shell, a
                    // function included; `sudo`, `env` and `exec` run a
                    // program of that name.
                    let mut args = node.walk();
                    if let Some(arg) = node
                        .children_by_field_name("argument", &mut args)
                        .find(|a| !text_of(*a).starts_with('-'))
                        .filter(|a| a.kind() == "word")
                    {
                        facts.calls.push((text_of(arg).to_string(), line));
                    }
                } else if name.named_child(0).is_some_and(|w| w.kind() == "word") {
                    facts.calls.push((word.to_string(), line));
                }
            }
            _ => {}
        }
    }
    facts.sources = sourced
        .into_iter()
        .map(|arg| spelled(arg, src, &assigned, 0))
        .collect();
    Some(facts)
}

/// How a word spells a path, its variables read off their assignments in
/// the same script when every assignment agrees.
fn spelled(node: Node, src: &[u8], assigned: &FxHashMap<&str, Vec<Node>>, depth: u8) -> Spelled {
    let text = node.utf8_text(src).unwrap_or("");
    let script_dir = |t: &str| t.contains("$0") || t.contains("${0") || t.contains("BASH_SOURCE");
    match node.kind() {
        "word" | "string_content" => Spelled::literal(text),
        "raw_string" => Spelled::literal(text.trim_matches('\'')),
        "command_substitution" if text.contains("dirname") && script_dir(text) => Spelled {
            head: Head::ScriptDir,
            tail: String::new(),
        },
        "expansion" if text.ends_with("%/*}") && script_dir(text) => Spelled {
            head: Head::ScriptDir,
            tail: String::new(),
        },
        "simple_expansion" | "expansion" => {
            let name = text.trim_start_matches('$').trim_matches(['{', '}']);
            match assigned.get(name) {
                Some(values) if depth < 4 => {
                    let mut all = values.iter().map(|v| spelled(*v, src, assigned, depth + 1));
                    let first = all.next().unwrap_or_else(Spelled::unknown);
                    if all.all(|s| s == first) {
                        first
                    } else {
                        Spelled::unknown()
                    }
                }
                _ => Spelled::unknown(),
            }
        }
        "string" | "concatenation" => {
            let mut cursor = node.walk();
            node.named_children(&mut cursor)
                .fold(Spelled::literal(""), |acc, part| {
                    acc.then(spelled(part, src, assigned, depth))
                })
        }
        _ => Spelled::unknown(),
    }
}

/// Whether a path is a script by its name; `None` when only its shebang
/// can say (`bin/deploy`).
fn by_path(path: &str) -> Option<bool> {
    match crate::languages::get_language_for_file(path) {
        Some(lang) => Some(matches!(lang, "bash" | "zsh")),
        None if Path::new(path).extension().is_none() => None,
        None => Some(false),
    }
}

fn by_shebang(path: &str, text: &str) -> bool {
    crate::languages::sniff_language(path, text) == Some("bash")
}

/// The functions a script defines, when `path` is one.
pub(super) fn function_names(path: &str, text: &str) -> Vec<String> {
    if !by_path(path).unwrap_or_else(|| by_shebang(path, text)) {
        return Vec::new();
    }
    read(text).map_or_else(Vec::new, |f| f.functions)
}

/// `a/./b/../c` as `a/c`; `None` when it climbs out of the repository.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

struct ShellDef {
    path: String,
    name: String,
    lines: (u32, u32),
}

pub(super) struct ShellModel<'a> {
    root: &'a Path,
    source: &'a Source,
    /// Every file of the repository by its file name.
    by_name: FxHashMap<String, Vec<String>>,
    facts: RefCell<FxHashMap<String, Option<Rc<ShellFacts>>>>,
    scripts: RefCell<FxHashMap<String, bool>>,
    /// The scripts that source each file, read once off every script.
    sourced_by: OnceCell<FxHashMap<String, Vec<String>>>,
    defs: Table<(String, String, u32), ShellDef>,
}

impl<'a> ShellModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source, files: &FxHashSet<String>) -> Self {
        let mut by_name: FxHashMap<String, Vec<String>> = FxHashMap::default();
        for f in files {
            let name = f.rsplit('/').next().unwrap_or(f);
            by_name.entry(name.to_string()).or_default().push(f.clone());
        }
        Self {
            root,
            source,
            by_name,
            facts: RefCell::default(),
            scripts: RefCell::default(),
            sourced_by: OnceCell::new(),
            defs: Table::new(),
        }
    }

    fn facts(&self, rel: &str) -> Option<Rc<ShellFacts>> {
        if let Some(known) = self.facts.borrow().get(rel) {
            return known.clone();
        }
        let read = self
            .source
            .read_to_string(&self.root.join(rel))
            .and_then(|text| read(&text))
            .map(Rc::new);
        self.facts
            .borrow_mut()
            .insert(rel.to_string(), read.clone());
        read
    }

    fn exists(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.by_name
            .get(name)
            .is_some_and(|all| all.iter().any(|p| p == path))
    }

    fn by_file_name(&self, path: &str) -> Target {
        let name = path.rsplit('/').next().unwrap_or(path);
        match self.by_name.get(name).map(Vec::as_slice) {
            None | Some([]) => Target::Outside,
            Some([one]) => Target::File(one.clone()),
            Some(many) => {
                let mut many = many.to_vec();
                many.sort();
                Target::Ambiguous(many)
            }
        }
    }

    fn resolve(&self, script: &str, s: &Spelled) -> Target {
        let dir = script.rsplit_once('/').map_or("", |(d, _)| d);
        let file_name = s.tail.rsplit('/').next().unwrap_or("");
        match s.head {
            _ if file_name.is_empty() => Target::Unknown,
            Head::ScriptDir => match normalize(&format!("{dir}/{}", s.tail)) {
                Some(p) if self.exists(&p) => Target::File(p),
                _ => Target::Outside,
            },
            // Run from anywhere, a relative path names a file beside the
            // script, at the root, or wherever the caller stood.
            Head::Literal if !s.tail.starts_with(['/', '~']) => {
                [format!("{dir}/{}", s.tail), s.tail.clone()]
                    .iter()
                    .filter_map(|p| normalize(p))
                    .find(|p| self.exists(p))
                    .map_or_else(|| self.by_file_name(&s.tail), Target::File)
            }
            // `$LIB_DIR/x-lib.sh`: a whole file name after an unknown
            // directory is looked up by name.
            Head::Unknown if !s.tail.contains('/') => Target::Unknown,
            _ => self.by_file_name(&s.tail),
        }
    }

    /// Every script that sources `path`, directly or through a library.
    fn sourcers(&self, path: &str) -> Vec<String> {
        let by = self.sourced_by.get_or_init(|| {
            let mut scripts: Vec<&String> = self
                .by_name
                .values()
                .flatten()
                .filter(|p| self.covers(p))
                .collect();
            scripts.sort();
            let mut by: FxHashMap<String, Vec<String>> = FxHashMap::default();
            for script in scripts {
                for s in self.facts(script).iter().flat_map(|f| f.sources.iter()) {
                    if let Target::File(f) = self.resolve(script, s) {
                        by.entry(f).or_default().push(script.clone());
                    }
                }
            }
            by
        });
        let mut seen: FxHashSet<String> = FxHashSet::default();
        let mut stack = vec![path.to_string()];
        while let Some(p) = stack.pop() {
            for r in by.get(&p).into_iter().flatten() {
                if seen.insert(r.clone()) {
                    stack.push(r.clone());
                }
            }
        }
        let mut out: Vec<String> = seen.into_iter().collect();
        out.sort();
        out
    }

    fn reach(&self, from: &str, target: &str, name: &str) -> Reach {
        let mut exact: FxHashSet<String> = FxHashSet::default();
        let mut any: FxHashSet<String> = FxHashSet::default();
        let mut queue = vec![(from.to_string(), true)];
        let (mut ambiguous, mut shadowed, mut unknown) = (false, false, false);
        while let Some((path, bound)) = queue.pop() {
            if !(if bound { &mut exact } else { &mut any }).insert(path.clone()) {
                continue;
            }
            let Some(facts) = self.facts(&path) else {
                continue;
            };
            if path != from && facts.functions.iter().any(|n| n == name) {
                shadowed |= bound;
            }
            for s in &facts.sources {
                match self.resolve(&path, s) {
                    Target::File(f) if f == target && bound => return Reach::Bound,
                    Target::File(f) if f == target => ambiguous = true,
                    Target::File(f) => queue.push((f, bound)),
                    Target::Ambiguous(all) => {
                        ambiguous |= all.iter().any(|f| f == target);
                        queue.extend(all.into_iter().map(|f| (f, false)));
                    }
                    Target::Unknown => unknown = true,
                    Target::Outside => {}
                }
            }
        }
        if ambiguous {
            Reach::Ambiguous
        } else if shadowed {
            Reach::Shadowed
        } else if unknown {
            Reach::Unknown
        } else {
            Reach::None
        }
    }
}

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses];

impl LanguageFacts for ShellModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        MODELLED
    }

    fn covers(&self, path: &str) -> bool {
        if let Some(known) = by_path(path) {
            return known;
        }
        if let Some(known) = self.scripts.borrow().get(path) {
            return *known;
        }
        let script = self
            .source
            .read_to_string(&self.root.join(path))
            .is_some_and(|text| by_shebang(path, &text));
        self.scripts.borrow_mut().insert(path.to_string(), script);
        script
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let name = at.frag.symbol_name.as_deref().map(base_symbol)?;
        let lines = (at.frag.start_line(), at.frag.end_line());
        self.defs
            .intern((at.path.to_string(), name.to_string(), lines.0), || {
                Some(ShellDef {
                    path: at.path.to_string(),
                    name: name.to_string(),
                    lines,
                })
            })
    }

    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding> {
        let def = self.defs.get(def);
        let facts = self.facts(site.path)?;
        let own = site.path == def.path;
        let lines: Vec<u32> = facts
            .calls
            .iter()
            .filter(|(n, l)| {
                *n == def.name
                    && (site.lines.0..=site.lines.1).contains(l)
                    && !(own && (def.lines.0..=def.lines.1).contains(l))
            })
            .map(|(_, l)| *l)
            .collect();
        if lines.is_empty() {
            return None;
        }
        let evidence = if own {
            Evidence::Resolved
        } else if facts.functions.contains(&def.name) {
            return None;
        } else {
            match self.reach(site.path, &def.path, &def.name) {
                Reach::Bound => Evidence::Resolved,
                Reach::Ambiguous => Evidence::Candidate("ambiguous_module"),
                Reach::Shadowed => return None,
                // A library calls what the scripts sourcing it source
                // beside it: functions are global once sourced.
                _ if self
                    .sourcers(site.path)
                    .iter()
                    .any(|r| self.reach(r, &def.path, &def.name) == Reach::Bound) =>
                {
                    Evidence::Resolved
                }
                Reach::Unknown => Evidence::Candidate("import_unresolved"),
                Reach::None => return None,
            }
        };
        Some(Finding {
            evidence,
            relation: Use::Call,
            lines,
        })
    }

    fn reaches_file(&self, test: &str, caller: &str) -> bool {
        self.reach(test, caller, "") == Reach::Bound
    }

    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        let def = self.defs.get(def);
        let facts = self.facts(path)?;
        Some(
            facts
                .calls
                .iter()
                .filter(|(n, _)| *n == def.name)
                .map(|(_, l)| *l)
                .collect(),
        )
    }

    fn names_unread(&self, path: &str, name: &str) -> Option<bool> {
        let facts = self.facts(path)?;
        let part = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-');
        Some(facts.unread.iter().any(|text| {
            text.match_indices(name).any(|(at, _)| {
                !text[..at].chars().next_back().is_some_and(part)
                    && !text[at + name.len()..].chars().next().is_some_and(part)
            })
        }))
    }
}
