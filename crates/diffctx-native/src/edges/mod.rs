pub mod base;
pub mod config_edges;
pub mod document;
pub mod history;
pub mod semantic;
pub mod similarity;
pub mod structural;

use std::cmp::Reverse;
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use tracing::debug;

use crate::graph::{
    CappedEdges, CompactEdge, EdgeCapStats, EdgeCategory, RankedCandidate, SourceTopK,
    SuppressionFactors, cap_out_edges_per_source, dedup_compact_edges, intern_fragment_nodes,
    push_bounded_top_k, read_max_out_edges_per_node,
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

fn pack_pair(src: u32, dst: u32) -> u64 {
    ((src as u64) << 32) | dst as u64
}

struct LoggedEmission {
    src: u32,
    dst: u32,
    weight: f64,
}

/// Two-pass edge construction that never retains the raw edge universe
/// as keyed dictionaries and runs every builder exactly once.
///
/// Pass 1 runs every builder and records its emissions into a compact
/// per-builder log of (src, dst, weight) triples (16 bytes/edge, builder
/// tag implicit in the outer index); a first-seen scan in builder
/// registration order reproduces `dedup_compact_edges` semantics exactly
/// (each pair counted once, category from the first builder that
/// produced it) and yields per-node in-degree, per-source out-degree,
/// the semantic distinct-file fan counts, and a sorted `(src, dst) ->
/// category` lookup used only internally by pass 2 (below) — it is
/// *not* returned to the caller; `assemble_graph` derives the exported
/// category table from the post-cap edges instead, which is what keeps
/// it aligned with the CSR (see `graph::assemble_graph`).
///
/// Pass 2 replays the log instead of rerunning the builders, damps each
/// emission on the fly with the pass-1 hub-suppression factors — always
/// under the pair's canonical first-builder category — and keeps at most
/// K candidates per source per builder in a bounded min-heap, freeing
/// each builder's log shard as it is consumed. Any edge evicted from a
/// per-builder heap is outranked by K surviving same-source edges, so
/// the final merge + dedup + cap over the survivors is bit-identical to
/// capping the full materialized universe.
pub fn collect_capped_edges(
    fragments: &[Fragment],
    repo_root: Option<&Path>,
    skip_expensive: bool,
) -> CappedEdges {
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
    let builder_meta: Vec<(EdgeCategory, f64)> = all_builders
        .iter()
        .map(|(cat_name, builder)| {
            let category = EdgeCategory::from_str(builder.category_label().unwrap_or(cat_name));
            (category, category_weights.multiplier(category))
        })
        .collect();

    let per_builder_log: Vec<Vec<LoggedEmission>> = all_builders
        .par_iter()
        .map(|(name, builder)| {
            crate::deadline::check_compute_deadline("edge construction");
            let t = std::time::Instant::now();
            let edges = builder.build(fragments, repo_root);
            if std::env::var_os("DIFFCTX_TRACE_BUILDERS").is_some() {
                eprintln!(
                    "builder {name}: {:.1}s, {} edges",
                    t.elapsed().as_secs_f64(),
                    edges.len()
                );
            }
            let mut log = Vec::with_capacity(edges.len());
            for ((src, dst), weight) in edges {
                let (Some(&s), Some(&d)) = (node_to_idx.get(&src), node_to_idx.get(&dst)) else {
                    continue;
                };
                log.push(LoggedEmission {
                    src: s,
                    dst: d,
                    weight,
                });
            }
            log
        })
        .collect();
    drop(all_builders);

    let n_nodes = idx_to_node.len();
    let mut in_degree = vec![0u32; n_nodes];
    let mut out_degree = vec![0u32; n_nodes];
    let mut category_entries: Vec<(u32, u32, EdgeCategory)> = Vec::new();
    let mut sem_out_files: FxHashMap<u32, FxHashSet<&str>> = FxHashMap::default();
    let mut raw_by_category: FxHashMap<EdgeCategory, u64> = FxHashMap::default();
    let mut deduped_by_category: FxHashMap<EdgeCategory, u64> = FxHashMap::default();
    let mut seen: FxHashSet<u64> = FxHashSet::default();
    for (builder_idx, log) in per_builder_log.iter().enumerate() {
        let (category, _) = builder_meta[builder_idx];
        *raw_by_category.entry(category).or_default() += log.len() as u64;
        for e in log {
            if !seen.insert(pack_pair(e.src, e.dst)) {
                continue;
            }
            *deduped_by_category.entry(category).or_default() += 1;
            in_degree[e.dst as usize] += 1;
            out_degree[e.src as usize] += 1;
            category_entries.push((e.src, e.dst, category));
            if category == EdgeCategory::Semantic {
                sem_out_files
                    .entry(e.src)
                    .or_default()
                    .insert(idx_to_node[e.dst as usize].path.as_ref());
            }
        }
    }
    drop(seen);
    let mut emissions_by_category: Vec<(EdgeCategory, u64, u64)> = raw_by_category
        .iter()
        .map(|(&category, &raw)| {
            let deduped = deduped_by_category.get(&category).copied().unwrap_or(0);
            (category, raw, deduped)
        })
        .collect();
    emissions_by_category.sort_unstable_by_key(|e| e.0.as_str());
    category_entries.sort_unstable_by_key(|e| (e.0, e.1));

    let mut sem_file_deg = vec![0u32; n_nodes];
    for (&src, files) in &sem_out_files {
        sem_file_deg[src as usize] = files.len() as u32;
    }
    drop(sem_out_files);

    let deduped_edge_count = category_entries.len();
    let factors = SuppressionFactors::from_counters(in_degree, sem_file_deg);
    let max_per_node = read_max_out_edges_per_node();

    let capped_per_builder: Vec<Vec<CompactEdge>> = per_builder_log
        .into_par_iter()
        .enumerate()
        .map(|(builder_idx, log)| {
            let (builder_category, multiplier) = builder_meta[builder_idx];
            let mut per_source: FxHashMap<u32, SourceTopK> = FxHashMap::default();
            for e in log {
                let category = category_entries
                    .binary_search_by_key(&(e.src, e.dst), |c| (c.0, c.1))
                    .map(|k| category_entries[k].2)
                    .unwrap_or(builder_category);
                let damped = factors.damp(e.weight * multiplier, category, e.src, e.dst);
                push_bounded_top_k(
                    per_source.entry(e.src).or_default(),
                    RankedCandidate {
                        weight: damped,
                        dst: e.dst,
                        category,
                    },
                    max_per_node,
                );
            }
            let mut survivors =
                Vec::with_capacity(per_source.values().map(|h| h.len()).sum::<usize>());
            for (src, heap) in per_source {
                for Reverse(c) in heap {
                    survivors.push(CompactEdge {
                        src,
                        dst: c.dst,
                        weight: c.weight,
                        category: c.category,
                    });
                }
            }
            survivors
        })
        .collect();

    let total: usize = capped_per_builder.iter().map(|v| v.len()).sum();
    let mut edges: Vec<CompactEdge> = Vec::with_capacity(total);
    for v in capped_per_builder {
        edges.extend(v);
    }
    dedup_compact_edges(&mut edges);
    cap_out_edges_per_source(&mut edges, max_per_node);

    let nodes_capped = out_degree
        .iter()
        .filter(|&&d| d as usize > max_per_node)
        .count();
    let cap_stats = EdgeCapStats {
        edges_before_cap: deduped_edge_count,
        edges_after_cap: edges.len(),
        edges_dropped_by_cap: deduped_edge_count - edges.len(),
        nodes_capped,
        max_out_edges_per_node: max_per_node,
        emissions_by_category,
    };

    CappedEdges {
        node_to_idx,
        idx_to_node,
        edges,
        cap_stats,
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
