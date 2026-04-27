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

use crate::graph::EdgeCategory;
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

pub fn collect_all_edges(
    fragments: &[Fragment],
    repo_root: Option<&Path>,
    skip_expensive: bool,
) -> (EdgeDict, EdgeCategories) {
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

    let category_weights = *crate::config::category_weights::CATEGORY_WEIGHTS;
    let per_builder_edges: Vec<(EdgeDict, Vec<((FragmentId, FragmentId), EdgeCategory)>)> =
        all_builders
            .par_iter()
            .map(|(cat_name, builder)| {
                let cat_label = builder.category_label().unwrap_or(cat_name);
                let category = EdgeCategory::from_str(cat_label);
                let multiplier = category_weights.multiplier(category);
                let mut edges = builder.build(fragments, repo_root);
                if multiplier != 1.0 {
                    for w in edges.values_mut() {
                        *w *= multiplier;
                    }
                }
                let cats: Vec<_> = edges.keys().map(|k| (k.clone(), category)).collect();
                (edges, cats)
            })
            .collect();

    let mut all_edges: EdgeDict = FxHashMap::default();
    let mut edge_categories: EdgeCategories = FxHashMap::default();
    for (edges, cats) in per_builder_edges {
        for ((src, dst), weight) in edges {
            if weight > *all_edges.get(&(src.clone(), dst.clone())).unwrap_or(&0.0) {
                all_edges.insert((src.clone(), dst.clone()), weight);
            }
        }
        for (key, cat) in cats {
            edge_categories.entry(key).or_insert(cat);
        }
    }

    (all_edges, edge_categories)
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
