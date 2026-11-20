use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::facts::kotlin::{KotlinFacts, UseKind};
use diffctx_base::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids, discover_by_refs};

fn is_swift_file(path: &Path) -> bool {
    base::has_ext(path, &[".swift"])
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+(?:class|struct|enum|protocol|func\s+)?(\w+)").unwrap()
});

fn extract_imports(content: &str) -> FxHashSet<String> {
    base::captures1(&IMPORT_RE, content).collect()
}

fn reconstruct_file<'a>(frags: &[&'a Fragment]) -> String {
    let max_line = frags.iter().map(|f| f.end_line()).max().unwrap_or(0) as usize;
    let mut lines = vec![""; max_line];
    for f in frags {
        let base_idx = f.start_line().saturating_sub(1) as usize;
        for (offset, line) in f.content.lines().enumerate() {
            if let Some(slot) = lines.get_mut(base_idx + offset) {
                *slot = line;
            }
        }
    }
    lines.join("\n")
}

fn covering<'a>(frags: &[&'a Fragment], line: u32) -> Vec<&'a Fragment> {
    frags
        .iter()
        .copied()
        .filter(|f| line >= f.start_line() && line <= f.end_line())
        .collect()
}

fn narrowest_covering<'a>(frags: &[&'a Fragment], line: u32) -> Vec<&'a Fragment> {
    let hits = covering(frags, line);
    let Some(min_span) = hits
        .iter()
        .map(|f| f.end_line().saturating_sub(f.start_line()))
        .min()
    else {
        return Vec::new();
    };
    hits.into_iter()
        .filter(|f| f.end_line().saturating_sub(f.start_line()) == min_span)
        .collect()
}

pub(crate) struct SwiftEdgeBuilder;

impl EdgeBuilder for SwiftEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_swift_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let import_w = EDGE_WEIGHTS["swift_import"].forward;
        let conform_w = EDGE_WEIGHTS["swift_conformance"].forward;
        let ext_w = EDGE_WEIGHTS["swift_extension"].forward;
        let type_w = EDGE_WEIGHTS["swift_type"].forward;
        let reverse_factor = EDGE_WEIGHTS["swift_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mut by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        for f in &frags {
            by_path.entry(f.path()).or_default().push(*f);
        }

        let mut parsed: FxHashMap<&str, KotlinFacts> = FxHashMap::default();
        let mut defs_by_name: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut file_types: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        let mut file_type_uses: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        let mut frag_own_defs: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();

        for (&path, file_frags) in &by_path {
            let source = reconstruct_file(file_frags);
            let Some(facts) = crate::facts::swift::facts(&source) else {
                continue;
            };
            for decl in &facts.declarations {
                if decl.kind == "extension" {
                    file_types
                        .entry(path)
                        .or_default()
                        .insert(decl.name.clone());
                    continue;
                }
                let owners = if decl.is_type() {
                    file_types
                        .entry(path)
                        .or_default()
                        .insert(decl.name.clone());
                    covering(file_frags, decl.start_line)
                } else if decl.kind == "fun" {
                    narrowest_covering(file_frags, decl.start_line)
                } else {
                    continue;
                };
                let key = decl.name.to_ascii_lowercase();
                for f in owners {
                    frag_own_defs
                        .entry(f.id.clone())
                        .or_default()
                        .insert(decl.name.clone());
                    let bucket = defs_by_name.entry(key.clone()).or_default();
                    if !bucket.contains(&f.id) {
                        bucket.push(f.id.clone());
                    }
                }
            }
            for u in &facts.uses {
                if u.kind == UseKind::TypeUse
                    || u.name.chars().next().is_some_and(char::is_uppercase)
                {
                    file_type_uses
                        .entry(path)
                        .or_default()
                        .insert(u.name.clone());
                }
            }
            parsed.insert(path, facts);
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (&path, file_frags) in &by_path {
            let Some(facts) = parsed.get(path) else {
                for f in file_frags {
                    for imp in extract_imports(&f.content) {
                        base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, reverse_factor);
                    }
                }
                continue;
            };

            for imp in &facts.imports {
                for f in covering(file_frags, imp.line) {
                    base::link_by_name(
                        &f.id,
                        &imp.path,
                        &idx,
                        &mut edges,
                        import_w,
                        reverse_factor,
                    );
                }
            }

            let super_map: FxHashMap<&str, &[String]> = facts
                .supertypes
                .iter()
                .map(|(n, s)| (n.as_str(), s.as_slice()))
                .collect();

            for decl in &facts.declarations {
                if decl.kind == "extension" {
                    if let Some(targets) = defs_by_name.get(&decl.name.to_ascii_lowercase()) {
                        for f in covering(file_frags, decl.start_line) {
                            add_edges_from_ids(&mut edges, &f.id, targets, ext_w, reverse_factor);
                        }
                    }
                }
                if let Some(supers) = super_map.get(decl.name.as_str()) {
                    for sup in *supers {
                        if let Some(targets) = defs_by_name.get(&sup.to_ascii_lowercase()) {
                            for f in covering(file_frags, decl.start_line) {
                                add_edges_from_ids(
                                    &mut edges,
                                    &f.id,
                                    targets,
                                    conform_w,
                                    reverse_factor,
                                );
                            }
                        }
                    }
                }
            }

            let mentioned_types = file_type_uses.get(path);
            for u in &facts.uses {
                if !matches!(u.kind, UseKind::TypeUse | UseKind::Call) {
                    continue;
                }
                let Some(targets) = defs_by_name.get(&u.name.to_ascii_lowercase()) else {
                    continue;
                };
                for f in narrowest_covering(file_frags, u.line) {
                    if frag_own_defs
                        .get(&f.id)
                        .is_some_and(|own| own.contains(&u.name))
                    {
                        continue;
                    }
                    if u.kind == UseKind::TypeUse {
                        add_edges_from_ids(&mut edges, &f.id, targets, type_w, reverse_factor);
                    } else {
                        let scoped: Vec<FragmentId> = targets
                            .iter()
                            .filter(|t| {
                                t.path.as_ref() == path
                                    || file_types.get(t.path.as_ref()).is_some_and(|declared| {
                                        mentioned_types
                                            .is_some_and(|used| !declared.is_disjoint(used))
                                    })
                            })
                            .cloned()
                            .collect();
                        let chosen = if scoped.is_empty() {
                            targets.as_slice()
                        } else {
                            scoped.as_slice()
                        };
                        add_edges_from_ids(&mut edges, &f.id, chosen, type_w, reverse_factor);
                    }
                }
            }
        }

        edges
    }

    discover_by_refs!(is_swift_file, extract_imports);
}
