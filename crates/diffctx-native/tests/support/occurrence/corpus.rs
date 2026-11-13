//! Seeds for the class "an occurrence is not lost": identifier positions taken
//! from the files of the impact corpus, layered by the grammar's node kind
//! (`parent kind/own kind`). At each position the name of the scaffold
//! definition replaces the identifier, the file is placed in the scaffold's
//! repository with an import of the definition, and the answer must name the
//! file or say what it did not read. A family's positions that no corpus file
//! holds are layers with no seed.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use tree_sitter::{Language, Node, Parser};
use walkdir::WalkDir;

use super::{Seed, scaffolds};
use crate::support::families::{Family, Files};

const PER_LAYER: usize = 2;
const MAX_FILE: usize = 20_000;

// A position is a reference only when it is not the name a declaration
// introduces, a path of an import or a member of an object the file never
// typed: those can legitimately be explained away without a candidate.
const NAME_FIELDS: [&str; 9] = [
    "name",
    "parameter",
    "parameters",
    "pattern",
    "left",
    "declarator",
    "alias",
    "label",
    "type_parameters",
];
const SKIP_ANCESTOR: [&str; 13] = [
    "import",
    "use_declaration",
    "use_list",
    "extern_crate",
    "package",
    "namespace_use",
    "namespace_definition",
    "export_specifier",
    "export_clause",
    "preproc_include",
    "pattern",
    "param",
    "lifetime",
];
const SKIP_PARENT: [&str; 11] = [
    "type",
    "jsx_attribute",
    "field_initializer",
    "macro_invocation",
    "token_tree",
    "attribute_item",
    "implements",
    "class_interface",
    "scoped",
    "qualified",
    "pair",
];
const MEMBER: [&str; 7] = [
    "attribute",
    "member",
    "field_expression",
    "field_access",
    "selector",
    "navigation",
    "method_invocation",
];

// How a corpus file of a family becomes a file of the scaffold's repository.
struct Grammar {
    langs: Vec<(&'static str, Language)>,
    path: fn(&str) -> String,
    // The statement that binds the scaffold's definition, and the package
    // line of a file that has one, which it replaces.
    header: &'static str,
    package: Option<(&'static str, &'static str)>,
}

impl Grammar {
    fn place(&self, base: &str, text: &str) -> (String, String) {
        let package = self
            .package
            .map(|(line, with)| (Regex::new(line).expect("regex"), with))
            .filter(|(line, _)| line.is_match(text));
        let placed = match package {
            Some((line, with)) => line.replace(text, with).into_owned(),
            None => format!("{}{text}", self.header),
        };
        ((self.path)(base), placed)
    }
}

// The grammars of a family, or why its positions are not read here. A full
// match: a family added to the engine's list is a compile error until it has
// a grammar or a reason.
fn grammar(family: Family) -> Result<Grammar, &'static str> {
    let tsx = || Language::new(tree_sitter_typescript::LANGUAGE_TSX);
    let c = || Language::new(tree_sitter_c::LANGUAGE);
    Ok(match family {
        Family::Python => Grammar {
            langs: vec![(".py", Language::new(tree_sitter_python::LANGUAGE))],
            path: |b| format!("occ/{b}"),
            header: "from shop.pricing import total\n",
            package: None,
        },
        Family::TypeScript => Grammar {
            langs: vec![
                (
                    ".ts",
                    Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
                ),
                (".tsx", tsx()),
                (".js", tsx()),
                (".jsx", tsx()),
            ],
            path: |b| format!("occ/{b}"),
            header: "import { total } from \"../src/pricing\";\n",
            package: None,
        },
        Family::Rust => Grammar {
            langs: vec![(".rs", Language::new(tree_sitter_rust::LANGUAGE))],
            path: |_| "src/occ.rs".to_string(),
            header: "use crate::pricing::total;\n",
            package: None,
        },
        Family::Kotlin => Grammar {
            langs: vec![(".kt", Language::new(brokk_tree_sitter_kotlin::LANGUAGE))],
            path: |_| "src/main/kotlin/shop/Occ.kt".to_string(),
            header: "package shop\n",
            package: Some((r"(?m)^package [^\n]*", "package shop")),
        },
        Family::Java => Grammar {
            langs: vec![(".java", Language::new(tree_sitter_java::LANGUAGE))],
            path: |_| "src/main/java/shop/Occ.java".to_string(),
            header: "package shop;\nimport static shop.Pricing.total;\n",
            package: Some((
                r"(?m)^package [^;]+;",
                "package shop;\nimport static shop.Pricing.total;",
            )),
        },
        Family::Go => Grammar {
            langs: vec![(".go", Language::new(tree_sitter_go::LANGUAGE))],
            path: |b| format!("pricing/occ_{b}"),
            header: "package pricing\n",
            package: Some((r"(?m)^package \w+", "package pricing")),
        },
        Family::C => Grammar {
            langs: vec![(".c", c()), (".h", c())],
            path: |b| format!("src/occ_{b}"),
            header: "#include \"pricing.h\"\n",
            package: None,
        },
        Family::Shell => Grammar {
            langs: vec![(".sh", Language::new(tree_sitter_bash::LANGUAGE))],
            path: |b| format!("scripts/occ_{b}"),
            header: "source ./pricing.sh\n",
            package: None,
        },
        Family::Php | Family::Swift => {
            return Err("its reference positions are not yet read against its grammar");
        }
        Family::Config | Family::Terraform | Family::Dockerfile | Family::Sql => {
            return Err(
                "no code grammar: its occurrences are keys and statements, not identifiers",
            );
        }
    })
}

// Every file of every case of the impact corpus, as (case, path, text).
fn files() -> &'static [(String, String, String)] {
    static ALL: OnceLock<Vec<(String, String, String)>> = OnceLock::new();
    ALL.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/cases/impact");
        let mut all = Vec::new();
        for entry in WalkDir::new(&root)
            .sort_by_file_name()
            .into_iter()
            .flatten()
        {
            if entry.path().extension().is_none_or(|e| e != "yaml") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            let Ok(case) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
                continue;
            };
            let name = entry.path().strip_prefix(&root).expect("under root");
            let mut texts = BTreeMap::new();
            for key in ["initial_files", "changed_files"] {
                for (path, body) in case["repo"][key].as_mapping().into_iter().flatten() {
                    if let (Some(path), Some(body)) = (path.as_str(), body.as_str()) {
                        texts.insert(path.to_string(), body.to_string());
                    }
                }
            }
            all.extend(
                texts
                    .into_iter()
                    .map(|(path, body)| (name.display().to_string(), path, body)),
            );
        }
        all
    })
}

// The identifier leaves of a parsed file that are reference positions, as
// (start, end, layer).
fn positions(text: &str, lang: &Language, family: Family) -> Vec<(usize, usize, String)> {
    let mut parser = Parser::new();
    parser.set_language(lang).expect("grammar");
    let Some(tree) = parser.parse(text, None) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    walk(tree.root_node(), None, None, family, &mut out);
    out
}

fn walk(
    node: Node,
    field: Option<&'static str>,
    parent_field: Option<&'static str>,
    family: Family,
    out: &mut Vec<(usize, usize, String)>,
) {
    if node.child_count() == 0 {
        let identifier = if family == Family::Shell {
            node.kind() == "word"
        } else {
            node.kind().ends_with("identifier") || node.kind() == "name"
        };
        if let Some(layer) = identifier
            .then(|| reference(node, field, parent_field, family))
            .flatten()
        {
            out.push((node.start_byte(), node.end_byte(), layer));
        }
        return;
    }
    for i in 0..node.child_count() {
        let child = node.child(i).expect("a child");
        walk(
            child,
            node.field_name_for_child(i as u32),
            field,
            family,
            out,
        );
    }
}

// The layer of a leaf that is a reference position, or none.
fn reference(
    leaf: Node,
    field: Option<&str>,
    parent_field: Option<&str>,
    family: Family,
) -> Option<String> {
    let parent = leaf.parent()?;
    let kind = parent.kind();
    let mut up = Some(parent);
    while let Some(node) = up {
        if SKIP_ANCESTOR.iter().any(|k| node.kind().contains(k)) {
            return None;
        }
        up = node.parent();
    }
    let skipped = field.is_some_and(|f| NAME_FIELDS.contains(&f))
        || leaf.kind() == "type_identifier"
        || (kind == "expression_list" && parent_field == Some("left"))
        || (family == Family::Shell && kind != "command_name")
        || (family == Family::Rust && kind == "attribute")
        || (family == Family::Kotlin && ["variable_declaration", "for_statement"].contains(&kind))
        || SKIP_PARENT.iter().any(|k| kind.contains(k));
    // Of a member expression only its first operand is read as a name.
    let member = MEMBER.iter().any(|k| kind.contains(k)) || kind.ends_with("suffix");
    if skipped
        || (member && parent.named_child(0).map(|c| c.start_byte()) != Some(leaf.start_byte()))
    {
        return None;
    }
    Some(format!("{kind}/{}", leaf.kind()))
}

// For a family, up to PER_LAYER seeds per layer taken from distinct corpus
// files; or the reason it has none.
pub fn seeds(family: Family) -> Result<Vec<Seed>, &'static str> {
    let grammar = grammar(family)?;
    let symbol = &scaffolds()[family.label()].symbol;
    let mut layers: BTreeMap<String, Vec<Seed>> = BTreeMap::new();
    let mut seen = HashSet::new();
    for (case, rel, text) in files() {
        let Some((_, lang)) = grammar.langs.iter().find(|(ext, _)| rel.ends_with(ext)) else {
            continue;
        };
        if text.len() > MAX_FILE
            || text.to_lowercase().contains(&symbol.to_lowercase())
            || !seen.insert(text)
        {
            continue;
        }
        let base = rel.rsplit('/').next().unwrap_or(rel);
        let (path, placed) = grammar.place(base, text);
        for (start, end, layer) in positions(&placed, lang, family) {
            let at = format!("{case}:{rel}");
            let picked = layers.entry(layer.clone()).or_default();
            if picked.len() >= PER_LAYER || picked.iter().any(|s| s.name.contains(&at)) {
                continue;
            }
            let line = placed[..start].matches('\n').count() + 1;
            let mut files = Files::from([(
                path.clone(),
                format!("{}{symbol}{}", &placed[..start], &placed[end..]),
            )]);
            if family == Family::Rust {
                let lib = &scaffolds()["rust"].base["src/lib.rs"];
                files.insert("src/lib.rs".to_string(), format!("{lib}pub mod occ;\n"));
            }
            picked.push(Seed {
                name: format!("{layer} {at}:{line}"),
                family: family.label().to_string(),
                kind: layer,
                user: path.clone(),
                symbol: symbol.clone(),
                files,
                base: Files::new(),
                change: Files::new(),
            });
        }
    }
    Ok(layers.into_values().flatten().collect())
}
