// A grammar can say more than our tables read. This test takes every named node
// type each linked grammar declares (the runtime form of its NODE_TYPES: named,
// visible kinds) and keeps the ones no table of ours mentions in
// `node_types_unhandled.txt`, both ways: a type that a table now mentions must
// leave the list, and a type a grammar release adds must join it or the test is
// red.
//
// "Our tables" are the source files that read a tree-sitter tree. Each file is
// scoped to the grammars it reads (`SCOPES`), and a file that imports
// tree-sitter without a scope is red, so a new reader cannot go unnoticed.
// A mention is a string literal of live code (not a comment, a `#[cfg(test)]`
// item or a message macro) holding the type as a whole word, a literal handed to
// `kind.ends_with/starts_with/contains`, or a substring of the declaration
// table in `parsers`. A word that is a type by coincidence counts as a mention,
// so the list is exact (nothing listed is mentioned) and the handled side is an
// upper bound: the listed types are the ones to read for a name they could hide.

#![cfg(all(feature = "lang-core", feature = "lang-extra"))]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use tree_sitter::{Language, Node, Parser};

const LIST: &str = "tests/node_types_unhandled.txt";
const REWRITE: &str = "DIFFCTX_NODE_TYPES_REWRITE";

/// The table of grammars and their definition types; its file never names tree-sitter.
const LANG_CONFIG_FILE: &str = "parsers/tree_sitter_strategy/lang_config.rs";

const JS: &[&str] = &["javascript", "typescript", "tsx"];
const NONE: &[&str] = &[];

/// Every file under a crate's `src` that mentions tree-sitter, and the grammars it reads.
/// `None` is every grammar: the code that walks any tree.
const SCOPES: &[(&str, Option<&[&str]>)] = &[
    ("change_class.rs", None),
    ("parsers/mod.rs", None),
    ("parsers/tree_sitter_strategy.rs", None),
    ("parsers/tree_sitter_strategy/node_kind.rs", None),
    (LANG_CONFIG_FILE, None),
    ("facts/lexical.rs", None),
    ("facts/lang/mod.rs", None),
    ("facts/lang/walker.rs", None),
    ("facts/lang/python.rs", Some(&["python"])),
    ("facts/lang/python/mutations.rs", Some(&["python"])),
    ("overlay/python.rs", Some(&["python"])),
    ("parsers/keyword_calls.rs", Some(&["python"])),
    ("facts/lang/typescript.rs", Some(JS)),
    ("overlay/javascript.rs", Some(JS)),
    ("overlay/css.rs", Some(&["css"])),
    ("facts/lang/rust.rs", Some(&["rust"])),
    ("facts/lang/rust/elements.rs", Some(&["rust"])),
    ("facts/lang/rust/macros.rs", Some(&["rust"])),
    ("parsers/rust_tests.rs", Some(&["rust"])),
    ("facts/lang/go.rs", Some(&["go"])),
    ("facts/lang/java.rs", Some(&["java"])),
    ("facts/java.rs", Some(&["java"])),
    ("facts/lang/kotlin.rs", Some(&["kotlin"])),
    ("facts/kotlin.rs", Some(&["kotlin"])),
    ("facts/reflect.rs", Some(&["java", "kotlin"])),
    ("facts/lang/c.rs", Some(&["c", "cpp"])),
    ("facts/lang/php.rs", Some(&["php"])),
    ("facts/lang/swift.rs", Some(&["swift"])),
    ("facts/lang/shell.rs", Some(&["bash"])),
    ("overlay/shell.rs", Some(&["bash"])),
    ("facts/lang/terraform.rs", Some(&["hcl"])),
    ("facts/config/yaml.rs", Some(&["yaml"])),
    ("facts/config/json.rs", Some(&["json"])),
    ("facts/config/syntax.rs", Some(&["yaml", "json"])),
    ("parsers/scala.rs", Some(&["scala"])),
    // Reads a Bash tree for two kinds (`command`, `redirected_statement`) outside
    // any table; the report of this list names them.
    ("parsers/embedded.rs", Some(NONE)),
];

/// The grammars the crate parses with, by the name `parsers` gives them.
fn grammars() -> Vec<(&'static str, Language)> {
    let l = Language::new;
    vec![
        ("python", l(tree_sitter_python::LANGUAGE)),
        ("javascript", l(tree_sitter_javascript::LANGUAGE)),
        ("typescript", l(tree_sitter_typescript::LANGUAGE_TYPESCRIPT)),
        ("tsx", l(tree_sitter_typescript::LANGUAGE_TSX)),
        ("go", l(tree_sitter_go::LANGUAGE)),
        ("rust", l(tree_sitter_rust::LANGUAGE)),
        ("java", l(tree_sitter_java::LANGUAGE)),
        ("c", l(tree_sitter_c::LANGUAGE)),
        ("cpp", l(tree_sitter_cpp::LANGUAGE)),
        ("ruby", l(tree_sitter_ruby::LANGUAGE)),
        ("c_sharp", l(tree_sitter_c_sharp::LANGUAGE)),
        ("php", l(tree_sitter_php::LANGUAGE_PHP)),
        ("scala", l(tree_sitter_scala::LANGUAGE)),
        ("swift", l(tree_sitter_swift::LANGUAGE)),
        ("kotlin", l(brokk_tree_sitter_kotlin::LANGUAGE)),
        ("html", l(tree_sitter_html::LANGUAGE)),
        ("bash", l(tree_sitter_bash::LANGUAGE)),
        ("css", l(tree_sitter_css::LANGUAGE)),
        ("haskell", l(tree_sitter_haskell::LANGUAGE)),
        ("elixir", l(tree_sitter_elixir::LANGUAGE)),
        ("lua", l(tree_sitter_lua::LANGUAGE)),
        ("r", l(tree_sitter_r::LANGUAGE)),
        ("ocaml", l(tree_sitter_ocaml::LANGUAGE_OCAML)),
        ("erlang", l(tree_sitter_erlang::LANGUAGE)),
        ("julia", l(tree_sitter_julia::LANGUAGE)),
        ("zig", l(tree_sitter_zig::LANGUAGE)),
        ("clojure", l(tree_sitter_clojure::LANGUAGE)),
        ("nix", l(tree_sitter_nix::LANGUAGE)),
        ("groovy", l(tree_sitter_groovy::LANGUAGE)),
        ("objc", l(tree_sitter_objc::LANGUAGE)),
        ("cmake", l(tree_sitter_cmake::LANGUAGE)),
        ("make", l(tree_sitter_make::LANGUAGE)),
        ("hcl", l(tree_sitter_hcl::LANGUAGE)),
        ("graphql", l(tree_sitter_graphql::LANGUAGE)),
        ("dart", l(tree_sitter_dart::LANGUAGE)),
        ("prisma", l(tree_sitter_prisma_io::LANGUAGE)),
        ("svelte", l(tree_sitter_svelte_ng::LANGUAGE)),
        ("json", l(tree_sitter_json::LANGUAGE)),
        ("yaml", l(tree_sitter_yaml::LANGUAGE)),
    ]
}

/// Symbols a grammar declares named and visible that no tree holds: a scanner's
/// error sentinel and the keyword-extraction token of Nix (neither is in the
/// grammar's `NODE_TYPES`).
const NEVER_IN_A_TREE: &[(&str, &str)] = &[
    ("erlang", "error_sentinel"),
    ("haskell", "error_sentinel"),
    ("scala", "error_sentinel"),
    ("php", "sentinel_error"),
    ("nix", "keyword"),
];

fn named_types(name: &str, language: &Language) -> BTreeSet<String> {
    (0..language.node_kind_count())
        .filter_map(|id| u16::try_from(id).ok())
        .filter(|&id| language.node_kind_is_named(id) && language.node_kind_is_visible(id))
        .filter_map(|id| language.node_kind_for_id(id))
        .filter(|kind| *kind != "ERROR" && !NEVER_IN_A_TREE.contains(&(name, *kind)))
        .map(str::to_string)
        .collect()
}

/// What the tables of one grammar say about node types.
#[derive(Default, Clone)]
struct Reads {
    words: BTreeSet<String>,
    ends: Vec<String>,
    starts: Vec<String>,
    contains: Vec<String>,
    /// `declares` in `parsers`: a kind holding one of these and none of `excluded`.
    declaring: Vec<String>,
    excluded: Vec<String>,
}

impl Reads {
    fn merge(&mut self, other: &Self) {
        self.words.extend(other.words.iter().cloned());
        self.ends.extend(other.ends.iter().cloned());
        self.starts.extend(other.starts.iter().cloned());
        self.contains.extend(other.contains.iter().cloned());
        self.declaring.extend(other.declaring.iter().cloned());
        self.excluded.extend(other.excluded.iter().cloned());
    }

    fn mentions(&self, kind: &str) -> bool {
        self.words.contains(kind)
            || self.ends.iter().any(|p| kind.ends_with(p.as_str()))
            || self.starts.iter().any(|p| kind.starts_with(p.as_str()))
            || self.contains.iter().any(|p| kind.contains(p.as_str()))
            || (self.declaring.iter().any(|p| kind.contains(p.as_str()))
                && !self.excluded.iter().any(|p| kind.contains(p.as_str())))
    }
}

/// The literal text of a macro that prints or fails: prose, never a node type.
const MESSAGES: &[&str] = &[
    "format",
    "format_args",
    "write",
    "writeln",
    "print",
    "println",
    "eprint",
    "eprintln",
    "panic",
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
    "unreachable",
    "todo",
    "unimplemented",
    "trace",
    "debug",
    "info",
    "warn",
    "error",
    "bail",
    "anyhow",
    "ensure",
];

/// What one source file says: for every grammar, and for a named one.
#[derive(Default)]
struct FileReads {
    shared: Reads,
    own: BTreeMap<String, Reads>,
}

struct Reader<'a> {
    text: &'a str,
    file: &'a str,
}

impl<'a> Reader<'a> {
    fn of(&self, node: Node) -> &'a str {
        &self.text[node.byte_range()]
    }

    fn literal(&self, node: Node) -> &'a str {
        let text = self.of(node);
        text.get(1..text.len().saturating_sub(1)).unwrap_or("")
    }

    fn literals(&self, node: Node, out: &mut Vec<&'a str>) {
        if node.kind() == "string_literal" {
            out.push(self.literal(node));
            return;
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.literals(child, out);
        }
    }

    fn words(&self, node: Node) -> BTreeSet<String> {
        let mut found = Vec::new();
        self.literals(node, &mut found);
        found
            .iter()
            .flat_map(|text| text.split_whitespace())
            .map(str::to_string)
            .collect()
    }

    fn field(&self, node: Node, name: &str) -> Option<String> {
        node.child_by_field_name(name)
            .map(|n| self.of(n).to_string())
    }

    fn visit(
        &self,
        node: Node,
        out: &mut FileReads,
        consts: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        let mut gated = false;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            match child.kind() {
                "attribute_item" => {
                    let compact: String = self.of(child).split_whitespace().collect();
                    gated |= compact == "#[cfg(test)]";
                    continue;
                }
                "line_comment" | "block_comment" => continue,
                _ => {}
            }
            if std::mem::take(&mut gated) {
                continue;
            }
            self.visit_one(child, out, consts);
        }
    }

    fn visit_one(
        &self,
        node: Node,
        out: &mut FileReads,
        consts: &mut BTreeMap<String, BTreeSet<String>>,
    ) {
        match node.kind() {
            "macro_invocation" => {
                let name = self.field(node, "macro").unwrap_or_default();
                let name = name.rsplit("::").next().unwrap_or_default();
                if !MESSAGES.contains(&name) {
                    self.visit(node, out, consts);
                }
            }
            "string_literal" => out
                .shared
                .words
                .extend(self.literal(node).split_whitespace().map(str::to_string)),
            "const_item" | "static_item" => {
                let name = self.field(node, "name").unwrap_or_default();
                if self.file == LANG_CONFIG_FILE && name == "LANG_CONFIGS" {
                    return self.visit(node, out, consts);
                }
                let words = self.words(node);
                match name.as_str() {
                    "DECLARING" => out.shared.declaring.extend(words),
                    "NOT" => out.shared.excluded.extend(words),
                    _ if self.file == LANG_CONFIG_FILE && name.ends_with("_DEFINITION_TYPES") => {
                        consts.insert(name, words);
                    }
                    _ => out.shared.words.extend(words),
                }
            }
            "struct_expression" => {
                let name = self.field(node, "name").unwrap_or_default();
                match (self.file, name.as_str()) {
                    (LANG_CONFIG_FILE, "LangConfig") => {
                        self.lang_config(node, out, consts);
                    }
                    ("facts/lexical.rs", "Row") => self.lexical_row(node, out),
                    _ => self.visit(node, out, consts),
                }
            }
            "call_expression" => {
                if !self.pattern_call(node, out) {
                    self.visit(node, out, consts);
                }
            }
            "raw_string_literal" => {}
            _ => self.visit(node, out, consts),
        }
    }

    /// `kind.ends_with("_statement")` and its kin: a literal that names no type
    /// but the kinds of one shape.
    fn pattern_call(&self, call: Node, out: &mut FileReads) -> bool {
        let Some(function) = call.child_by_field_name("function") else {
            return false;
        };
        if function.kind() != "field_expression" {
            return false;
        }
        let method = self.field(function, "field").unwrap_or_default();
        let receiver = self.field(function, "value").unwrap_or_default();
        if !receiver.ends_with("kind()") && receiver != "kind" {
            return false;
        }
        let mut found = Vec::new();
        if let Some(args) = call.child_by_field_name("arguments") {
            self.literals(args, &mut found);
        }
        let sink = match method.as_str() {
            "ends_with" => &mut out.shared.ends,
            "starts_with" => &mut out.shared.starts,
            "contains" => &mut out.shared.contains,
            _ => return false,
        };
        sink.extend(found.iter().map(|s| (*s).to_string()));
        !found.is_empty()
    }

    fn initializers<'t>(&self, node: Node<'t>) -> Vec<(String, Node<'t>)> {
        let mut fields = Vec::new();
        let Some(body) = node.child_by_field_name("body") else {
            return fields;
        };
        let mut cursor = body.walk();
        for init in body.children(&mut cursor) {
            if let (Some(name), Some(value)) = (
                init.child_by_field_name("field"),
                init.child_by_field_name("value"),
            ) {
                fields.push((self.of(name).to_string(), value));
            }
        }
        fields
    }

    /// One `LangConfig`: the types it lists belong to its grammar alone.
    fn lang_config(
        &self,
        node: Node,
        out: &mut FileReads,
        consts: &BTreeMap<String, BTreeSet<String>>,
    ) {
        let mut ts_name = String::new();
        let mut words = BTreeSet::new();
        for (field, value) in self.initializers(node) {
            match field.as_str() {
                "ts_name" => ts_name = self.literal(value).to_string(),
                "definition_types" => {
                    words.extend(self.words(value));
                    if let Some(listed) = consts.get(self.of(value)) {
                        words.extend(listed.iter().cloned());
                    }
                }
                _ => {}
            }
        }
        let grammar = if ts_name == "jsx" {
            "javascript"
        } else {
            &ts_name
        };
        out.own
            .entry(grammar.to_string())
            .or_default()
            .words
            .extend(words);
    }

    /// One row of the lexical table: the kinds it lists belong to the grammars
    /// of its languages.
    fn lexical_row(&self, node: Node, out: &mut FileReads) {
        let mut grammars: Vec<&str> = Vec::new();
        let mut words = BTreeSet::new();
        for (field, value) in self.initializers(node) {
            if field == "languages" {
                grammars = self
                    .of(value)
                    .split(|c: char| !(c.is_alphanumeric() || c == ':'))
                    .filter_map(|path| path.strip_prefix("Language::"))
                    .flat_map(grammars_of_language)
                    .copied()
                    .collect();
            } else {
                words.extend(self.words(value));
            }
        }
        for grammar in grammars {
            out.own
                .entry(grammar.to_string())
                .or_default()
                .words
                .extend(words.iter().cloned());
        }
    }
}

fn grammars_of_language(language: &str) -> &'static [&'static str] {
    match language {
        "Python" => &["python"],
        "JavaScript" | "TypeScript" | "Html" | "Astro" => JS,
        "Rust" => &["rust"],
        "Kotlin" => &["kotlin"],
        "Java" => &["java"],
        "Go" => &["go"],
        "Php" => &["php"],
        "C" | "Cpp" => &["c", "cpp"],
        "Swift" => &["swift"],
        "Shell" => &["bash"],
        "Terraform" => &["hcl"],
        other => panic!("the lexical table names a language this test does not know: {other}"),
    }
}

fn read_source(file: &str, text: &str) -> FileReads {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("rust grammar");
    let tree = parser.parse(text, None).expect("parse");
    let reader = Reader { text, file };
    let mut out = FileReads::default();
    let mut consts = BTreeMap::new();
    // The consts a `LangConfig` names come first in the file; read them in a
    // first pass so the order of items never matters.
    let mut scratch = FileReads::default();
    reader.visit(tree.root_node(), &mut scratch, &mut consts);
    reader.visit(tree.root_node(), &mut out, &mut consts);
    out
}

fn srcs() -> Vec<PathBuf> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found: Vec<PathBuf> = fs::read_dir(&crates)
        .expect("crates")
        .map(|e| e.expect("entry").path().join("src"))
        .filter(|p| p.is_dir())
        .collect();
    found.sort();
    found
}

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// The readers' files: those that mention tree-sitter, with their scope.
fn tables() -> BTreeMap<String, Reads> {
    let known: Vec<&str> = grammars().iter().map(|(name, _)| *name).collect();
    let mut found = Vec::new();
    for root in srcs() {
        sources(&root, &mut found);
    }
    let mut per_grammar: BTreeMap<String, Reads> = BTreeMap::new();
    let mut unscoped = Vec::new();
    for path in found {
        let root = srcs()
            .into_iter()
            .find(|s| path.starts_with(s))
            .expect("under a crate's src");
        let relative = path
            .strip_prefix(&root)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let text = fs::read_to_string(&path).expect("source");
        if !text.contains("tree_sitter") && relative != LANG_CONFIG_FILE {
            continue;
        }
        let Some((_, scope)) = SCOPES.iter().find(|(file, _)| *file == relative) else {
            unscoped.push(relative);
            continue;
        };
        let reads = read_source(&relative, &text);
        let scope: Vec<&str> = match scope {
            Some(grammars) => grammars.to_vec(),
            None => known.clone(),
        };
        for grammar in scope {
            per_grammar
                .entry(grammar.to_string())
                .or_default()
                .merge(&reads.shared);
        }
        for (grammar, own) in &reads.own {
            assert!(
                known.contains(&grammar.as_str()),
                "{relative} names a grammar the crate does not link: {grammar}"
            );
            per_grammar.entry(grammar.clone()).or_default().merge(own);
        }
    }
    assert!(
        unscoped.is_empty(),
        "a file reads a tree-sitter tree and has no scope in SCOPES (the grammars it reads): {unscoped:?}"
    );
    per_grammar
}

fn unhandled() -> (BTreeSet<(String, String)>, Vec<(String, usize)>) {
    let tables = tables();
    let none = Reads::default();
    let mut listed = BTreeSet::new();
    let mut totals = Vec::new();
    for (name, language) in grammars() {
        let reads = tables.get(name).unwrap_or(&none);
        let types = named_types(name, &language);
        totals.push((name.to_string(), types.len()));
        for kind in types.iter().filter(|kind| !reads.mentions(kind)) {
            listed.insert((name.to_string(), kind.clone()));
        }
    }
    (listed, totals)
}

fn recorded(text: &str) -> BTreeSet<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let (grammar, kind) = line
                .split_once(char::is_whitespace)
                .unwrap_or_else(|| panic!("`<grammar> <node type>`: {line}"));
            (grammar.to_string(), kind.trim().to_string())
        })
        .collect()
}

fn shown(rows: &[&(String, String)]) -> String {
    let mut lines: Vec<String> = rows
        .iter()
        .take(40)
        .map(|(grammar, kind)| format!("  {grammar} {kind}"))
        .collect();
    if rows.len() > 40 {
        lines.push(format!("  ... and {} more", rows.len() - 40));
    }
    lines.join("\n")
}

#[test]
fn the_unhandled_node_types_are_the_list_both_ways() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST);
    let text = fs::read_to_string(&path).unwrap_or_default();
    let (found, totals) = unhandled();

    let mut summary = String::from("grammar types handled listed\n");
    for (name, total) in &totals {
        let listed = found.iter().filter(|(g, _)| g == name).count();
        let _ = writeln!(summary, "{name} {total} {} {listed}", total - listed);
    }
    eprintln!("{summary}");

    if std::env::var_os(REWRITE).is_some() {
        let mut out = String::new();
        for line in text.lines().take_while(|line| line.starts_with('#')) {
            let _ = writeln!(out, "{line}");
        }
        for (grammar, kind) in &found {
            let _ = writeln!(out, "{grammar} {kind}");
        }
        fs::write(&path, out).expect("write the list");
        return;
    }

    let listed = recorded(&text);
    let stale: Vec<_> = listed.difference(&found).collect();
    let missing: Vec<_> = found.difference(&listed).collect();
    assert!(
        stale.is_empty() && missing.is_empty(),
        "{LIST} and the grammars disagree ({REWRITE}=1 rewrites the list; read each row first).\n\
         Listed, but a table mentions it now or the grammar dropped it ({}):\n{}\n\
         Not listed, and no table mentions it ({}):\n{}",
        stale.len(),
        shown(&stale),
        missing.len(),
        shown(&missing),
    );
}

#[test]
fn the_reader_counts_only_what_a_table_says() {
    let source = r#"
        const KINDS: &[&str] = &["alpha beta", "gamma"];
        const DECLARING: [&str; 1] = ["method"];
        const NOT: [&str; 1] = ["call"];
        // "in_a_comment"
        fn read(node: Node) -> bool {
            println!("in_a_message");
            let s = "in_a_string";
            node.kind() == "compared" || kind.ends_with("_tail") || node.kind().starts_with("head_")
        }
        #[cfg(test)]
        mod tests { const T: &str = "in_a_test"; }
        const AFTER: &str = "after_the_tests";
    "#;
    let mut reads = read_source("x.rs", source).shared;
    for kind in [
        "alpha",
        "beta",
        "gamma",
        "compared",
        "in_a_string",
        "after_the_tests",
    ] {
        assert!(reads.mentions(kind), "{kind}");
    }
    for kind in ["in_a_comment", "in_a_message", "in_a_test", "delta"] {
        assert!(!reads.mentions(kind), "{kind}");
    }
    assert!(reads.mentions("any_tail") && reads.mentions("head_any"));
    assert!(reads.mentions("method_definition") && !reads.mentions("method_call"));
    reads.words.clear();
    assert!(!reads.mentions("alpha"));
}

#[test]
fn a_config_row_belongs_to_its_own_grammar() {
    let source = r#"
        const JS_DEFINITION_TYPES: &[&str] = &["shared_def"];
        const LANG_CONFIGS: &[LangConfig] = &[
            LangConfig { extensions: &[".py"], ts_name: "python", definition_types: &["only_python"] },
            LangConfig { extensions: &[".jsx"], ts_name: "jsx", definition_types: JS_DEFINITION_TYPES },
        ];
    "#;
    let reads = read_source(LANG_CONFIG_FILE, source);
    assert!(reads.own["python"].words.contains("only_python"));
    assert!(!reads.own["python"].words.contains("shared_def"));
    assert!(reads.own["javascript"].words.contains("shared_def"));
    assert!(!reads.shared.words.contains("only_python"));
}

#[test]
fn every_grammar_has_named_types_and_the_list_names_only_linked_ones() {
    let grammars = grammars();
    for (name, language) in &grammars {
        assert!(!named_types(name, language).is_empty(), "{name}");
    }
    let linked: BTreeSet<&str> = grammars.iter().map(|(name, _)| *name).collect();
    let text =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(LIST)).unwrap_or_default();
    for (grammar, kind) in recorded(&text) {
        assert!(linked.contains(grammar.as_str()), "{grammar} {kind}");
    }
}
