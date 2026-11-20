//! Where a `--symbol` value goes when a call passes it by keyword (#708).
//! `build_context(user, tz_offset_hours=SETTINGS.tz_offset_hours)` binds the
//! callee's parameter of that name, so the callee is where the value is read
//! next: the answer lists it beside the definitions of the name, and the call
//! is its caller. A parameter is no definition of the facts (#452, #548), so
//! the callee stands for it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tree_sitter::Node;

use crate::facts::{BlobFacts, Fact, Import, LocalKind, Scope};
use crate::resource::RunContext;
use diffctx_base::types::Fragment;

/// What a call writes before its parentheses: `a.b.f` is `f` qualified by `a.b`,
/// and a receiver that is no plain name (`make().f`) has no qualifier to read.
struct Callee {
    qualifier: Option<Vec<String>>,
    name: String,
}

fn dotted(node: Node<'_>, text: &str) -> Option<Vec<String>> {
    match node.kind() {
        "identifier" => Some(vec![text[node.byte_range()].to_string()]),
        "attribute" => {
            let mut chain = dotted(node.child_by_field_name("object")?, text)?;
            chain.push(text[node.child_by_field_name("attribute")?.byte_range()].to_string());
            Some(chain)
        }
        _ => None,
    }
}

fn callee_of(function: Node<'_>, text: &str) -> Option<Callee> {
    match function.kind() {
        "identifier" => Some(Callee {
            qualifier: Some(Vec::new()),
            name: text[function.byte_range()].to_string(),
        }),
        "attribute" => Some(Callee {
            qualifier: dotted(function.child_by_field_name("object")?, text),
            name: text[function.child_by_field_name("attribute")?.byte_range()].to_string(),
        }),
        _ => None,
    }
}

fn keyword_calls(node: Node<'_>, text: &str, name: &str, found: &mut Vec<Callee>) {
    if node.kind() == "call" {
        let passes = node.child_by_field_name("arguments").is_some_and(|args| {
            let mut cursor = args.walk();
            args.named_children(&mut cursor).any(|arg| {
                arg.kind() == "keyword_argument"
                    && arg
                        .child_by_field_name("name")
                        .is_some_and(|n| &text[n.byte_range()] == name)
            })
        });
        if passes {
            found.extend(
                node.child_by_field_name("function")
                    .and_then(|f| callee_of(f, text)),
            );
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        keyword_calls(child, text, name, found);
    }
}

/// `record/tools.py` is `record.tools`, a package's `__init__.py` its directory.
fn module_of(path: &str) -> String {
    let stem = path.strip_suffix(".py").unwrap_or(path);
    let stem = stem.strip_suffix("/__init__").unwrap_or(stem);
    stem.replace('/', ".")
}

fn names_module(module: &str, imported: &str) -> bool {
    let imported = imported.trim_start_matches('.');
    !imported.is_empty() && (module == imported || module.ends_with(&format!(".{imported}")))
}

/// The module an import brings a local name from, with the names it adds to it:
/// `from record import tools as t` binds `t` to `record.tools`.
fn bound_modules(imports: &[&Import], local: &str) -> Vec<String> {
    let mut bound = Vec::new();
    for import in imports {
        let module = import.module.as_str();
        for (name, alias) in &import.names {
            if alias.as_ref().unwrap_or(name).as_str() == local {
                let target = if module.trim_start_matches('.').is_empty() {
                    name.as_str().to_string()
                } else {
                    format!("{module}.{}", name.as_str())
                };
                bound.push(target);
            }
        }
        if import.names.is_empty() {
            let plain = import.default.as_ref().map(|d| d.as_str());
            let alias = import.star.alias().map(|a| a.as_str());
            if alias.or(plain) == Some(local) || module.split('.').next() == Some(local) {
                bound.push(module.to_string());
            }
        }
    }
    bound
}

/// The functions a call somewhere in `files` passes `name` to by keyword, where
/// the function takes a parameter of that name. A callee a file imports is the
/// one its import names; an unimported one (a method on a receiver nothing
/// types) is taken only when it is the one function of that name that has the
/// parameter, and a name an import binds elsewhere is never guessed.
pub(crate) fn parameter_owners<'f>(
    root_dir: &Path,
    files: &[PathBuf],
    fragments: &'f [Fragment],
    name: &str,
    run: &RunContext,
) -> Vec<&'f Fragment> {
    let python =
        |path: &str| diffctx_base::languages::get_language_for_file(path) == Some("python");
    let mut facts: BTreeMap<&str, Option<BlobFacts>> = BTreeMap::new();
    let mut owners_named: BTreeMap<String, Vec<&'f Fragment>> = BTreeMap::new();
    for fragment in fragments
        .iter()
        .filter(|f| f.kind == diffctx_base::types::FragmentKind::Function && python(f.path()))
    {
        let Some(callee) = fragment
            .symbol_name
            .as_deref()
            .and_then(|s| s.rsplit('.').next())
        else {
            continue;
        };
        let blob = facts.entry(fragment.path()).or_insert_with(|| {
            let text = run.source().read_to_string(Path::new(fragment.path()))?;
            text.contains(name)
                .then(|| crate::facts::local_facts(fragment.path(), &text))
                .flatten()
        });
        let takes = blob.as_ref().is_some_and(|blob| {
            blob.facts.iter().any(|fact| {
                let Fact::Definition(def) = fact else {
                    return false;
                };
                def.local.name.as_str() == callee
                    && def.span.lines.0 <= fragment.end_line()
                    && fragment.start_line() <= def.span.lines.1
                    && blob.facts.iter().any(|fact| {
                        matches!(fact, Fact::LocalBinding(local)
                            if local.kind == LocalKind::Parameter
                                && local.name.as_str() == name
                                && local.enclosing == Scope::Def(def.local.clone()))
                    })
            })
        });
        if takes {
            owners_named
                .entry(callee.to_string())
                .or_default()
                .push(fragment);
        }
    }
    if owners_named.is_empty() {
        return Vec::new();
    }
    let mut chosen: Vec<&'f Fragment> = Vec::new();
    for file in files {
        let display = crate::paths::display_rel_or_abs(root_dir, file);
        if !python(&display) {
            continue;
        }
        let Some(text) = run
            .source()
            .read_to_string(file)
            .filter(|text| text.contains(name))
        else {
            continue;
        };
        let Some((tree, _)) = crate::parsers::parse_file(&display, &text) else {
            continue;
        };
        let mut calls = Vec::new();
        keyword_calls(tree.root_node(), &text, name, &mut calls);
        let imports: Vec<Import> = crate::facts::local_facts(&display, &text)
            .map(|blob| {
                blob.facts
                    .into_iter()
                    .filter_map(|fact| match fact {
                        Fact::Import(import) => Some(import),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let imports: Vec<&Import> = imports.iter().collect();
        for call in calls {
            let Some(candidates) = owners_named.get(&call.name) else {
                continue;
            };
            let Some(qualifier) = call.qualifier else {
                if let [only] = candidates.as_slice() {
                    chosen.push(only);
                }
                continue;
            };
            let (root, rest) = match qualifier.split_first() {
                Some((root, rest)) => (root.as_str(), rest.join(".")),
                None => (call.name.as_str(), String::new()),
            };
            let bound = bound_modules(&imports, root);
            let reaches = |fragment: &&&Fragment| {
                let own = module_of(&crate::paths::display_rel_or_abs(
                    root_dir,
                    Path::new(fragment.path()),
                ));
                Path::new(fragment.path()) == file.as_path()
                    || bound.iter().any(|target| {
                        let module = if qualifier.is_empty() {
                            target
                                .rsplit_once('.')
                                .map_or("", |(module, _)| module)
                                .to_string()
                        } else if rest.is_empty() {
                            target.clone()
                        } else {
                            format!("{target}.{rest}")
                        };
                        names_module(&own, &module)
                    })
            };
            let reached: Vec<&&Fragment> = candidates.iter().filter(reaches).collect();
            if !reached.is_empty() {
                chosen.extend(reached.into_iter().copied());
            } else if bound.is_empty() {
                if let [only] = candidates.as_slice() {
                    chosen.push(only);
                }
            }
        }
    }
    chosen.sort_by(|a, b| a.id.cmp(&b.id));
    chosen.dedup_by(|a, b| a.id == b.id);
    chosen
}
