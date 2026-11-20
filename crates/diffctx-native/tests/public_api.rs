// The public API of an internal crate is a ratchet (fj#197): what a crate lets
// the layers above it name is the one thing that cannot change by accident.
// Every `pub` item of the crate (functions with their signatures, types with
// their derives, public fields, variants, inherent methods, trait impls) is read
// off its source with tree-sitter-rust and held to two contracts:
//   - the list beside this test names each item, both ways: an item missing
//     from the list is red, and so is a listed item the code no longer has;
//   - `public_items.<crate>` in `budget.toml` caps the count, and it only goes
//     down without the owner's word.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use tempfile::TempDir;
use tree_sitter::Node;

mod graph_reader;
use graph_reader::{Source, budget_number, crate_file, parse, ratchet};

/// `(crate, its list)`: the crate's source is `crates/<crate>/src`.
const CRATES: &[(&str, &str)] = &[("diffctx-base", "tests/public_api_base.txt")];

fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_pub(node: Node, source: &Source) -> bool {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .any(|child| child.kind() == "visibility_modifier" && source.of(child) == "pub")
}

fn text_of(node: Node, field: &str, source: &Source) -> String {
    node.child_by_field_name(field)
        .map(|inner| collapsed(source.of(inner)))
        .unwrap_or_default()
}

/// The children of a node, each with the attributes written before it; an item
/// behind `#[cfg(test)]` is left out.
fn items<'t>(node: Node<'t>, source: &Source) -> Vec<(Node<'t>, Vec<String>)> {
    let mut attributes: Vec<String> = Vec::new();
    let mut found = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "attribute_item" => attributes.push(collapsed(source.of(child))),
            "line_comment" | "block_comment" | "inner_attribute_item" => {}
            _ => {
                let compact = |a: &String| a.split_whitespace().collect::<String>();
                if !attributes.iter().any(|a| compact(a) == "#[cfg(test)]") {
                    found.push((child, std::mem::take(&mut attributes)));
                }
                attributes.clear();
            }
        }
    }
    found
}

fn derives(attributes: &[String]) -> String {
    let names: Vec<&str> = attributes
        .iter()
        .filter_map(|a| a.strip_prefix("#[derive(")?.strip_suffix(")]"))
        .flat_map(|list| list.split(',').map(str::trim))
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        let mut names = names;
        names.sort_unstable();
        format!(" derive({})", names.join(", "))
    }
}

fn signature(node: Node, source: &Source) -> String {
    let generics = text_of(node, "type_parameters", source);
    let params = text_of(node, "parameters", source);
    match text_of(node, "return_type", source).as_str() {
        "" => format!("{generics}{params}"),
        ret => format!("{generics}{params} -> {ret}"),
    }
}

fn name_of(node: Node, source: &Source) -> String {
    text_of(node, "name", source)
}

/// The type an impl is for, without its arguments: `Vec<T>` is `Vec`.
fn head(type_text: &str) -> String {
    type_text
        .split(['<', ' '])
        .next()
        .unwrap_or_default()
        .to_string()
}

struct Reader<'a> {
    source: &'a Source<'a>,
    out: BTreeSet<String>,
}

impl Reader<'_> {
    fn module(&mut self, node: Node, path: &[String], dir: &Path) {
        let prefix = path.join("::");
        let at = |name: &str| {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}::{name}")
            }
        };
        let mut public_types = BTreeSet::new();
        let mut impls: Vec<(String, Vec<String>)> = Vec::new();
        for (item, attributes) in items(node, self.source) {
            let public = is_pub(item, self.source);
            let name = name_of(item, self.source);
            match item.kind() {
                "mod_item" if public => {
                    self.out.insert(format!("{} mod", at(&name)));
                    let mut nested = path.to_vec();
                    nested.push(name.clone());
                    self.child_module(item, &nested, dir, &name);
                }
                "function_item" if public => {
                    let line = format!("{} fn {}", at(&name), signature(item, self.source));
                    self.out.insert(line);
                }
                "struct_item" if public => {
                    public_types.insert(name.clone());
                    self.out
                        .insert(format!("{} struct{}", at(&name), derives(&attributes)));
                    self.out.extend(self.fields(item, &at(&name)));
                }
                "enum_item" if public => {
                    public_types.insert(name.clone());
                    self.out
                        .insert(format!("{} enum{}", at(&name), derives(&attributes)));
                    self.out.extend(self.variants(item, &at(&name)));
                }
                "trait_item" if public => {
                    public_types.insert(name.clone());
                    self.out.insert(format!("{} trait", at(&name)));
                    self.out.extend(self.members(item, &at(&name), false));
                }
                "type_item" | "const_item" | "static_item" if public => {
                    let kind = item.kind().trim_end_matches("_item");
                    let what = text_of(item, "type", self.source);
                    self.out.insert(format!("{} {kind} {what}", at(&name)));
                }
                "use_declaration" if public => {
                    let used = text_of(item, "argument", self.source);
                    let at = if prefix.is_empty() { "crate" } else { &prefix };
                    self.out.insert(format!("{at} use {used}"));
                }
                "impl_item" => {
                    let type_text = text_of(item, "type", self.source);
                    let owner = at(&head(&type_text));
                    let lines = match item.child_by_field_name("trait") {
                        Some(tr) => {
                            let tr = collapsed(self.source.of(tr));
                            vec![format!("{owner} impl {tr} for {type_text}")]
                        }
                        None => self.members(item, &owner, true),
                    };
                    impls.push((head(&type_text), lines));
                }
                _ => {}
            }
        }
        for (type_name, lines) in impls {
            if public_types.contains(&type_name) {
                self.out.extend(lines);
            }
        }
    }

    fn child_module(&mut self, item: Node, path: &[String], dir: &Path, name: &str) {
        if let Some(body) = item.child_by_field_name("body") {
            self.module(body, path, &dir.join(name));
            return;
        }
        let flat = dir.join(format!("{name}.rs"));
        let (file, children) = if flat.is_file() {
            (flat, dir.join(name))
        } else {
            (dir.join(name).join("mod.rs"), dir.join(name))
        };
        let (text, tree) = parse(&file);
        let source = Source { text: &text };
        let mut inner = Reader {
            source: &source,
            out: BTreeSet::new(),
        };
        inner.module(tree.root_node(), path, &children);
        self.out.extend(inner.out);
    }

    fn fields(&self, item: Node, owner: &str) -> Vec<String> {
        let Some(body) = item.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut lines = Vec::new();
        let mut cursor = body.walk();
        let mut position = 0;
        let mut pending_pub = false;
        for field in body.children(&mut cursor) {
            match field.kind() {
                "field_declaration" if is_pub(field, self.source) => {
                    let name = name_of(field, self.source);
                    let what = text_of(field, "type", self.source);
                    lines.push(format!("{owner}.{name} field {what}"));
                }
                "visibility_modifier" => pending_pub = self.source.of(field) == "pub",
                _ if body.kind() == "ordered_field_declaration_list"
                    && field.is_named()
                    && !field.kind().ends_with("comment")
                    && field.kind() != "attribute_item" =>
                {
                    if pending_pub {
                        let what = collapsed(self.source.of(field));
                        lines.push(format!("{owner}.{position} field {what}"));
                    }
                    pending_pub = false;
                    position += 1;
                }
                _ => {}
            }
        }
        lines
    }

    fn variants(&self, item: Node, owner: &str) -> Vec<String> {
        let Some(body) = item.child_by_field_name("body") else {
            return Vec::new();
        };
        let mut cursor = body.walk();
        body.children(&mut cursor)
            .filter(|v| v.kind() == "enum_variant")
            .map(|variant| {
                let name = name_of(variant, self.source);
                let text = collapsed(self.source.of(variant));
                format!("{owner}::{name} variant {text}")
            })
            .collect()
    }

    /// The associated items of an impl (only `pub` ones) or a trait (all of them).
    fn members(&self, item: Node, owner: &str, only_pub: bool) -> Vec<String> {
        let Some(body) = item.child_by_field_name("body") else {
            return Vec::new();
        };
        items(body, self.source)
            .into_iter()
            .filter(|(member, _)| !only_pub || is_pub(*member, self.source))
            .filter_map(|(member, _)| {
                let name = name_of(member, self.source);
                match member.kind() {
                    "function_item" | "function_signature_item" => Some(format!(
                        "{owner}::{name} fn {}",
                        signature(member, self.source)
                    )),
                    "const_item" => Some(format!(
                        "{owner}::{name} const {}",
                        text_of(member, "type", self.source)
                    )),
                    "type_item" | "associated_type" => Some(format!("{owner}::{name} type")),
                    _ => None,
                }
            })
            .collect()
    }
}

/// Every `pub` item of the crate whose source is `src`, one line each.
fn read_api(src: &Path) -> BTreeSet<String> {
    let (text, tree) = parse(&src.join("lib.rs"));
    let source = Source { text: &text };
    let mut reader = Reader {
        source: &source,
        out: BTreeSet::new(),
    };
    reader.module(tree.root_node(), &[], src);
    reader.out
}

fn listed(text: &str) -> BTreeSet<String> {
    text.lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

fn problems(
    name: &str,
    found: &BTreeSet<String>,
    list: &BTreeSet<String>,
    budget: &toml::Table,
) -> Vec<String> {
    let mut problems = Vec::new();
    for item in found.difference(list) {
        problems.push(format!(
            "new public item of {name}: {item}. A crate does not widen what the layers above may \
             name; the owner's word lists it in the public API list"
        ));
    }
    for item in list.difference(found) {
        problems.push(format!(
            "public item of {name} is gone: delete its line from the list: {item}"
        ));
    }
    let caps = budget
        .get("public_items")
        .and_then(toml::Value::as_table)
        .expect("budget.toml: public_items is missing");
    ratchet(
        &mut problems,
        &format!("public_items.{name}"),
        budget_number(caps, name),
        found.len(),
    );
    problems
}

#[test]
fn the_public_api_of_each_internal_crate_matches_its_list_and_its_cap() {
    let budget: toml::Table = fs::read_to_string(crate_file("../../budget.toml"))
        .expect("budget.toml")
        .parse()
        .expect("budget.toml parses");
    let mut found = Vec::new();
    for (name, list) in CRATES {
        let api = read_api(&crate_file(&format!("../{name}/src")));
        let text = fs::read_to_string(crate_file(list)).unwrap_or_else(|e| panic!("{list}: {e}"));
        eprintln!("{name}: {} public items", api.len());
        found.extend(problems(name, &api, &listed(&text), &budget));
    }
    assert!(
        found.is_empty(),
        "the public API moved:\n  {}",
        found.join("\n  ")
    );
}

#[test]
fn the_reader_lists_what_a_layer_above_may_name() {
    let dir = TempDir::new().expect("tempdir");
    let write = |path: &str, text: &str| {
        let file = dir.path().join(path);
        fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        fs::write(file, text).expect("write");
    };
    write(
        "lib.rs",
        "pub mod shapes;\nmod hidden;\npub fn top(a: u8) -> u16 { 0 }\npub(crate) fn inner() {}\n#[cfg(test)]\npub fn only_in_tests() {}\n",
    );
    write(
        "shapes.rs",
        "#[derive(Debug, Clone)]\npub struct Point { pub x: f64, y: f64 }\npub struct Pair(pub u8, u8);\n\
         impl Point { pub fn new() -> Self { todo!() } fn private(&self) {} pub const ORIGIN: u8 = 0; }\n\
         impl std::fmt::Display for Point {}\nimpl Hidden { pub fn nope() {} }\nstruct Hidden;\n\
         pub enum Kind { A, B(u8) }\npub trait Shape { fn area(&self) -> f64; }\n\
         pub const N: usize = 3;\npub use std::fmt::Debug;\npub mod nested;\n",
    );
    write("shapes/nested.rs", "pub fn deep() {}\n");
    write("hidden.rs", "pub fn not_reachable() {}\n");
    let api: Vec<String> = read_api(dir.path()).into_iter().collect();
    assert_eq!(
        api,
        [
            "shapes mod",
            "shapes use std::fmt::Debug",
            "shapes::Kind enum",
            "shapes::Kind::A variant A",
            "shapes::Kind::B variant B(u8)",
            "shapes::N const usize",
            "shapes::Pair struct",
            "shapes::Pair.0 field u8",
            "shapes::Point impl std::fmt::Display for Point",
            "shapes::Point struct derive(Clone, Debug)",
            "shapes::Point.x field f64",
            "shapes::Point::ORIGIN const u8",
            "shapes::Point::new fn () -> Self",
            "shapes::Shape trait",
            "shapes::Shape::area fn (&self) -> f64",
            "shapes::nested mod",
            "shapes::nested::deep fn ()",
            "top fn (a: u8) -> u16",
        ]
    );
}
