pub mod base;
pub mod config_edges;
pub mod document;
pub mod history;
pub mod semantic;
pub mod similarity;
pub mod structural;

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustc_hash::FxHashMap;
use tracing::debug;

use crate::graph::{
    CompactEdge, CompactEdges, EdgeCategory, dedup_compact_edges, intern_fragment_nodes,
};
use crate::types::FragmentId;

pub type EdgeDict = FxHashMap<(FragmentId, FragmentId), f64>;
pub type EdgeCategories = FxHashMap<(FragmentId, FragmentId), EdgeCategory>;

use crate::types::Fragment;

use self::base::EdgeBuilder;

const EXPENSIVE_CATEGORIES: &[&str] = &["similarity", "history"];

struct BuilderCategory {
    name: &'static str,
    builders: fn() -> Vec<Box<dyn EdgeBuilder>>,
}

fn builder_categories() -> Vec<BuilderCategory> {
    vec![
        BuilderCategory {
            name: "semantic",
            builders: || semantic::get_semantic_builders(),
        },
        BuilderCategory {
            name: "structural",
            builders: || structural::get_structural_builders(),
        },
        BuilderCategory {
            name: "config",
            builders: || config_edges::get_config_builders(),
        },
        BuilderCategory {
            name: "document",
            builders: || document::get_document_builders(),
        },
        BuilderCategory {
            name: "similarity",
            builders: || similarity::get_similarity_builders(),
        },
        BuilderCategory {
            name: "history",
            builders: || history::get_history_builders(),
        },
    ]
}

pub fn get_all_builders() -> Vec<Box<dyn EdgeBuilder>> {
    let mut all = Vec::new();
    for cat in builder_categories() {
        all.extend((cat.builders)());
    }
    all
}

/// Collect edges from all builders directly into the interned compact
/// representation. Each builder's FragmentId-keyed map is converted and
/// dropped inside the parallel closure, so at no point does the full
/// edge universe exist as string-keyed hashmaps — the peak that used to
/// OOM-kill multi-million-edge instances. Merge semantics match the
/// historical maps exactly: max weight across builders, category from
/// the first builder (in registration order) that produced the edge.
pub fn collect_all_edges(
    fragments: &[Fragment],
    repo_root: Option<&Path>,
    skip_expensive: bool,
) -> CompactEdges {
    let mut all_builders: Vec<(&str, Box<dyn EdgeBuilder>)> = Vec::new();
    for cat in builder_categories() {
        if skip_expensive && EXPENSIVE_CATEGORIES.contains(&cat.name) {
            debug!("skipping {} edge builders (skip_expensive=true)", cat.name);
            continue;
        }
        for builder in (cat.builders)() {
            all_builders.push((cat.name, builder));
        }
    }

    let (node_to_idx, idx_to_node) = intern_fragment_nodes(fragments);
    let category_weights = *crate::config::category_weights::CATEGORY_WEIGHTS;
    let per_builder_edges: Vec<Vec<CompactEdge>> = all_builders
        .par_iter()
        .map(|(cat_name, builder)| {
            let cat_label = builder.category_label().unwrap_or(cat_name);
            let category = EdgeCategory::from_str(cat_label);
            let multiplier = category_weights.multiplier(category);
            let edges = builder.build(fragments, repo_root);
            let mut compact = Vec::with_capacity(edges.len());
            for ((src, dst), weight) in edges {
                let s = match node_to_idx.get(&src) {
                    Some(&i) => i,
                    None => continue,
                };
                let d = match node_to_idx.get(&dst) {
                    Some(&i) => i,
                    None => continue,
                };
                compact.push(CompactEdge {
                    src: s,
                    dst: d,
                    weight: weight * multiplier,
                    category,
                });
            }
            compact
        })
        .collect();

    let total: usize = per_builder_edges.iter().map(|v| v.len()).sum();
    let mut edges: Vec<CompactEdge> = Vec::with_capacity(total);
    for v in per_builder_edges {
        edges.extend(v);
    }
    dedup_compact_edges(&mut edges);

    CompactEdges {
        node_to_idx,
        idx_to_node,
        edges,
    }
}

pub fn discover_all_related_files(
    changed_files: &[PathBuf],
    all_candidates: &[PathBuf],
    repo_root: Option<&Path>,
    file_cache: Option<&FxHashMap<PathBuf, String>>,
) -> Vec<PathBuf> {
    let mut discovered: FxHashMap<PathBuf, ()> = FxHashMap::default();
    for builder in get_all_builders() {
        for f in
            builder.discover_related_files(changed_files, all_candidates, repo_root, file_cache)
        {
            discovered.entry(f).or_insert(());
        }
    }
    let mut result: Vec<PathBuf> = discovered.into_keys().collect();
    result.sort();
    result
}
