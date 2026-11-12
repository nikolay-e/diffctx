// The module graph is a ratchet (fj#195). Cargo forbids a cycle between crates
// and says nothing about one between the modules of this crate, so any module
// can reach any other and responsibilities blur. The graph is read off the
// source with tree-sitter-rust (comments and strings never count) and held to
// three contracts:
//   - `module_edges.txt` lists every edge, both ways: an edge missing from the
//     list is red, and so is a listed edge the code no longer has;
//   - `LAYERS` is the intended order; an edge pointing against it carries an
//     `against order` mark in the list, and `against_order_edges` in
//     `budget.toml` caps those marks;
//   - `largest_scc` in `budget.toml` caps the biggest knot of modules that
//     reach each other. Both caps only go down.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use tree_sitter::{Node, Parser};

/// Lowest layer first: a module may use its own layer and every one below it.
const LAYERS: &[(&str, &[&str])] = &[
    (
        "base",
        &[
            "types",
            "languages",
            "canon",
            "testfiles",
            "tokenizer",
            "sanitize",
            "peak_rss",
            "fit",
            "interval",
            "container",
            "excerpt",
            "profile",
        ],
    ),
    ("settings", &["config", "mode", "stopwords", "signatures"]),
    (
        "input",
        &[
            "snapshot",
            "paths",
            "resource",
            "source",
            "grep",
            "diff_parse",
            "candidate_files",
        ],
    ),
    (
        "facts",
        &[
            "parsers",
            "facts",
            "change_class",
            "changed_lines",
            "fragmentation",
            "overlay",
            "keytable",
        ],
    ),
    ("resolve", &["resolve", "fold", "vocabulary"]),
    ("graph", &["graph", "edges", "core"]),
    (
        "ranking",
        &["ppr", "filtering", "scoring", "discovery", "token_corpus"],
    ),
    ("selection", &["select", "utility", "postpass"]),
    ("run", &["effective_config", "run_provenance"]),
    (
        "output",
        &["render", "decide", "tokens", "impact", "answer", "locate"],
    ),
    (
        "orchestration",
        &["pipeline", "provenance", "in_memory_harness"],
    ),
    (
        "surface",
        &[
            "artifact",
            "delivery",
            "hook",
            "analytics",
            "graph_export",
            "project_graph",
            "pybridge",
        ],
    ),
];

const AGAINST: &str = "against order";

type Edges = BTreeSet<(String, String)>;

struct Source<'a> {
    text: &'a str,
}

impl Source<'_> {
    fn of(&self, node: Node) -> &str {
        &self.text[node.byte_range()]
    }
}

struct Leaf {
    segments: Vec<String>,
    alias: Option<String>,
}

struct Graph {
    modules: BTreeSet<String>,
    /// `pub use tokens::occurrence;` in lib.rs: `crate::occurrence` names an
    /// item of that module, so it is an edge to it.
    reexports: BTreeMap<String, String>,
    edges: Edges,
}

fn parse(path: &Path) -> (String, tree_sitter::Tree) {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("rust grammar");
    let tree = parser.parse(&text, None).expect("parse");
    (text, tree)
}

/// The children that exist in a product build: an item behind `#[cfg(test)]`
/// is skipped, and so is the attribute itself.
fn live_children<'t>(node: Node<'t>, source: &Source) -> Vec<Node<'t>> {
    let mut gated = false;
    let mut live = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "attribute_item" => {
                let compact: String = source.of(child).split_whitespace().collect();
                gated |= compact == "#[cfg(test)]";
            }
            "line_comment" | "block_comment" => {}
            _ => {
                if !gated {
                    live.push(child);
                }
                gated = false;
            }
        }
    }
    live
}

fn is_scoped(node: Node) -> bool {
    matches!(node.kind(), "scoped_identifier" | "scoped_type_identifier")
}

/// A path as written: `crate::a::B` is `crate, a, B`; generic arguments and
/// anything that is not a name at the head leave no segment.
fn path_segments(node: Node, source: &Source) -> Vec<String> {
    match node.kind() {
        "scoped_identifier" | "scoped_type_identifier" => {
            let mut segments = node
                .child_by_field_name("path")
                .map(|path| path_segments(path, source))
                .unwrap_or_default();
            if let Some(name) = node.child_by_field_name("name") {
                segments.push(source.of(name).to_string());
            }
            segments
        }
        "identifier" | "type_identifier" | "crate" | "super" | "self" => {
            vec![source.of(node).to_string()]
        }
        "generic_type" | "generic_type_with_turbofish" => node
            .child_by_field_name("type")
            .map(|inner| path_segments(inner, source))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn use_leaves(node: Node, source: &Source, prefix: &[String], out: &mut Vec<Leaf>) {
    let joined = |tail: Vec<String>, alias: Option<String>| Leaf {
        segments: prefix.iter().cloned().chain(tail).collect(),
        alias,
    };
    match node.kind() {
        "identifier" | "crate" | "super" | "self" | "scoped_identifier" => {
            out.push(joined(path_segments(node, source), None));
        }
        "use_wildcard" => {
            if let Some(path) = node.named_child(0) {
                out.push(joined(path_segments(path, source), None));
            }
        }
        "use_as_clause" => {
            let tail = node
                .child_by_field_name("path")
                .map(|path| path_segments(path, source))
                .unwrap_or_default();
            let alias = node
                .child_by_field_name("alias")
                .map(|alias| source.of(alias).to_string());
            out.push(joined(tail, alias));
        }
        "scoped_use_list" => {
            let mut deeper = prefix.to_vec();
            if let Some(path) = node.child_by_field_name("path") {
                deeper.extend(path_segments(path, source));
            }
            if let Some(list) = node.child_by_field_name("list") {
                use_leaves(list, source, &deeper, out);
            }
        }
        "use_list" => {
            let mut cursor = node.walk();
            for item in node.named_children(&mut cursor) {
                use_leaves(item, source, prefix, out);
            }
        }
        _ => {}
    }
}

impl Graph {
    /// The top-level module a path written inside `module_path` reaches.
    fn target(&self, segments: &[String], module_path: &[String]) -> Option<String> {
        let name = match segments.first()?.as_str() {
            "crate" => segments.get(1)?,
            "super" | "self" => {
                let mut up = module_path.len();
                let mut at = 0;
                while let Some(step) = segments.get(at) {
                    match step.as_str() {
                        "self" => {}
                        "super" => up = up.checked_sub(1)?,
                        _ => break,
                    }
                    at += 1;
                }
                // Out of every module, the next name is an item of the crate root.
                if up != 0 {
                    return None;
                }
                segments.get(at)?
            }
            _ => return None,
        };
        if self.modules.contains(name) {
            Some(name.clone())
        } else {
            self.reexports.get(name).cloned()
        }
    }

    fn reach(&mut self, segments: &[String], module_path: &[String]) {
        if let Some(to) = self.target(segments, module_path) {
            let from = &module_path[0];
            if *from != to {
                self.edges.insert((from.clone(), to));
            }
        }
    }

    fn visit(
        &mut self,
        node: Node,
        source: &Source,
        module_path: &[String],
        dir: &Path,
        in_path: bool,
    ) {
        match node.kind() {
            "mod_item" => {
                let name = source.of(node.child_by_field_name("name").expect("mod name"));
                let mut nested = module_path.to_vec();
                nested.push(name.to_string());
                match node.child_by_field_name("body") {
                    Some(body) => self.visit_children(body, source, &nested, &dir.join(name)),
                    None => self.walk_declared(dir, name, &nested),
                }
                return;
            }
            "use_declaration" => {
                if let Some(argument) = node.child_by_field_name("argument") {
                    let mut leaves = Vec::new();
                    use_leaves(argument, source, &[], &mut leaves);
                    for leaf in leaves {
                        self.reach(&leaf.segments, module_path);
                    }
                }
                return;
            }
            "scoped_identifier" | "scoped_type_identifier" if !in_path => {
                self.reach(&path_segments(node, source), module_path);
            }
            "token_tree" => self.reach_in_macro(node, source, module_path),
            _ => {}
        }
        for child in live_children(node, source) {
            let inner = is_scoped(node) && is_scoped(child);
            self.visit(child, source, module_path, dir, inner);
        }
    }

    fn visit_children(&mut self, node: Node, source: &Source, module_path: &[String], dir: &Path) {
        for child in live_children(node, source) {
            self.visit(child, source, module_path, dir, false);
        }
    }

    /// `crate::a::f` among a macro's tokens is not a path node to the grammar.
    fn reach_in_macro(&mut self, tree: Node, source: &Source, module_path: &[String]) {
        let mut cursor = tree.walk();
        let tokens: Vec<Node> = tree.children(&mut cursor).collect();
        for window in tokens.windows(3) {
            let is_crate = window[0].kind() == "crate" || source.of(window[0]) == "$crate";
            if is_crate && source.of(window[1]) == "::" && window[2].kind() == "identifier" {
                let segments = vec!["crate".to_string(), source.of(window[2]).to_string()];
                self.reach(&segments, module_path);
            }
        }
    }

    /// `mod name;` declared in a file whose child modules live in `dir`.
    fn walk_declared(&mut self, dir: &Path, name: &str, module_path: &[String]) {
        let flat = dir.join(format!("{name}.rs"));
        let (file, children) = if flat.is_file() {
            (flat, dir.join(name))
        } else {
            (dir.join(name).join("mod.rs"), dir.join(name))
        };
        let (text, tree) = parse(&file);
        let source = Source { text: &text };
        self.visit_children(tree.root_node(), &source, module_path, &children);
    }
}

/// The top-level modules of the crate whose root is `src`, and every edge
/// between them.
fn read_graph(src: &Path) -> Graph {
    let (text, tree) = parse(&src.join("lib.rs"));
    let source = Source { text: &text };
    let declared = live_children(tree.root_node(), &source);
    let mut graph = Graph {
        modules: BTreeSet::new(),
        reexports: BTreeMap::new(),
        edges: Edges::new(),
    };
    for item in &declared {
        if item.kind() == "mod_item" && item.child_by_field_name("body").is_none() {
            let name = item.child_by_field_name("name").expect("mod name");
            graph.modules.insert(source.of(name).to_string());
        }
    }
    for item in declared
        .iter()
        .filter(|item| item.kind() == "use_declaration")
    {
        let Some(argument) = item.child_by_field_name("argument") else {
            continue;
        };
        let mut leaves = Vec::new();
        use_leaves(argument, &source, &[], &mut leaves);
        for leaf in leaves {
            let Some(top) = leaf
                .segments
                .first()
                .filter(|top| graph.modules.contains(*top))
            else {
                continue;
            };
            let name = leaf.alias.clone().or_else(|| leaf.segments.last().cloned());
            if let Some(name) = name.filter(|name| !graph.modules.contains(name)) {
                graph.reexports.insert(name, top.clone());
            }
        }
    }
    for module in graph.modules.clone() {
        graph.walk_declared(src, &module, &[module.clone()]);
    }
    graph
}

/// Size of the largest set of modules that all reach one another (Tarjan).
fn largest_scc(edges: &Edges, modules: &BTreeSet<String>) -> usize {
    struct State<'a> {
        next: BTreeMap<&'a str, Vec<&'a str>>,
        index: BTreeMap<&'a str, usize>,
        low: BTreeMap<&'a str, usize>,
        stack: Vec<&'a str>,
        largest: usize,
    }
    fn strong<'a>(v: &'a str, s: &mut State<'a>) {
        let at = s.index.len();
        s.index.insert(v, at);
        s.low.insert(v, at);
        s.stack.push(v);
        for w in s.next.get(v).cloned().unwrap_or_default() {
            if !s.index.contains_key(w) {
                strong(w, s);
                let low = s.low[v].min(s.low[w]);
                s.low.insert(v, low);
            } else if s.stack.contains(&w) {
                let low = s.low[v].min(s.index[w]);
                s.low.insert(v, low);
            }
        }
        if s.low[v] == s.index[v] {
            let at = s.stack.iter().position(|m| *m == v).expect("on stack");
            s.largest = s.largest.max(s.stack.len() - at);
            s.stack.truncate(at);
        }
    }
    let mut state = State {
        next: BTreeMap::new(),
        index: BTreeMap::new(),
        low: BTreeMap::new(),
        stack: Vec::new(),
        largest: 0,
    };
    for (from, to) in edges {
        state
            .next
            .entry(from.as_str())
            .or_default()
            .push(to.as_str());
    }
    for module in modules {
        if !state.index.contains_key(module.as_str()) {
            strong(module.as_str(), &mut state);
        }
    }
    state.largest
}

fn layer_of(module: &str) -> Option<usize> {
    LAYERS
        .iter()
        .position(|(_, members)| members.contains(&module))
}

fn against_order(edge: &(String, String)) -> bool {
    matches!((layer_of(&edge.0), layer_of(&edge.1)), (Some(from), Some(to)) if to > from)
}

/// `from -> to` per line; `# against order` marks an edge the layers refuse.
fn listed_edges(text: &str) -> BTreeMap<(String, String), bool> {
    text.lines()
        .filter_map(|line| {
            let (edge, note) = line.split_once('#').unwrap_or((line, ""));
            let (from, to) = edge.trim().split_once("->")?;
            Some((
                (from.trim().to_string(), to.trim().to_string()),
                note.trim() == AGAINST,
            ))
        })
        .collect()
}

fn budget_number(budget: &toml::Table, key: &str) -> usize {
    budget
        .get(key)
        .and_then(toml::Value::as_integer)
        .and_then(|n| usize::try_from(n).ok())
        .unwrap_or_else(|| panic!("budget.toml: {key} is missing or not a count"))
}

fn ratchet(problems: &mut Vec<String>, key: &str, recorded: usize, measured: usize) {
    if measured > recorded {
        problems.push(format!("{key} grew {recorded} -> {measured}"));
    } else if measured < recorded {
        problems.push(format!(
            "{key} dropped {recorded} -> {measured}: lower it in budget.toml in the same commit"
        ));
    }
}

fn problems(graph: &Graph, list: &str, budget: &toml::Table) -> Vec<String> {
    let listed = listed_edges(list);
    let mut problems = Vec::new();
    for edge in &graph.edges {
        let (from, to) = edge;
        match listed.get(edge) {
            None => problems.push(format!(
                "new edge {from} -> {to}: a module does not start using another. Depend downward, \
                 or the owner's word lists it in module_edges.txt"
            )),
            Some(&marked) if marked != against_order(edge) => problems.push(if marked {
                format!("edge {from} -> {to} no longer points against the layer order: drop its mark")
            } else {
                format!("edge {from} -> {to} points against the layer order: fix the dependency, or mark it `# {AGAINST}`")
            }),
            Some(_) => {}
        }
    }
    for (from, to) in listed.keys().filter(|edge| !graph.edges.contains(*edge)) {
        problems.push(format!(
            "edge {from} -> {to} is gone: delete its line from module_edges.txt"
        ));
    }
    for module in graph
        .modules
        .iter()
        .filter(|module| layer_of(module).is_none())
    {
        problems.push(format!("module {module} has no layer: add it to LAYERS"));
    }
    for member in LAYERS.iter().flat_map(|(_, members)| *members) {
        if !graph.modules.contains(*member) {
            problems.push(format!(
                "LAYERS names {member}, which is not a module: delete it"
            ));
        }
    }
    let against = graph
        .edges
        .iter()
        .filter(|edge| against_order(edge))
        .count();
    ratchet(
        &mut problems,
        "against_order_edges",
        budget_number(budget, "against_order_edges"),
        against,
    );
    let scc = largest_scc(&graph.edges, &graph.modules);
    ratchet(
        &mut problems,
        "largest_scc",
        budget_number(budget, "largest_scc"),
        scc,
    );
    problems
}

fn crate_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn the_module_graph_matches_its_list_and_its_caps() {
    let graph = read_graph(&crate_file("src"));
    let list = fs::read_to_string(crate_file("tests/module_edges.txt")).unwrap_or_default();
    let budget: toml::Table = fs::read_to_string(crate_file("../../budget.toml"))
        .expect("budget.toml")
        .parse()
        .expect("budget.toml parses");
    let found = problems(&graph, &list, &budget);
    eprintln!(
        "{} modules, {} edges",
        graph.modules.len(),
        graph.edges.len()
    );
    assert!(
        found.is_empty(),
        "the module graph moved:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_reader_follows_every_way_a_module_names_another() {
    let dir = TempDir::new().expect("tempdir");
    let write = |path: &str, text: &str| {
        let file = dir.path().join(path);
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(file, text).expect("write");
    };
    write(
        "lib.rs",
        "pub mod a;\nmod b;\nmod c;\nmod d;\nmod e;\nmod f;\nmod g;\nmod h;\nmod i;\npub use b::inner;\n#[cfg(test)]\nmod t;\n",
    );
    write(
        "a.rs",
        "use crate::b::X;\nfn f() { let _ = crate::c::run(); }\n",
    );
    write(
        "b.rs",
        "pub mod inner;\npub(crate) fn g() {}\n// crate::d::never\nconst S: &str = \"crate::d::never\";\n",
    );
    write("b/inner.rs", "pub fn x() {}\n");
    write("c.rs", "use crate::{d, e::{self, Y}, f as renamed};\n");
    write("d.rs", "fn m() { println!(\"{}\", crate::g::z()); }\n");
    write("e.rs", "use crate::inner::x;\n");
    write(
        "f.rs",
        "#[cfg(test)]\nmod tests { use crate::h::H; }\nfn k() -> Vec<crate::i::I> { vec![] }\n",
    );
    write(
        "g.rs",
        "use super::h::K;\nmod nested { use super::super::i::J; }\n",
    );
    write("h.rs", "");
    write("i.rs", "");
    write("t.rs", "use crate::a::A;\n");
    let edges: Vec<String> = read_graph(dir.path())
        .edges
        .iter()
        .map(|(from, to)| format!("{from}->{to}"))
        .collect();
    assert_eq!(
        edges,
        [
            "a->b", "a->c", "c->d", "c->e", "c->f", "d->g", "e->b", "f->i", "g->h", "g->i"
        ]
    );
}
