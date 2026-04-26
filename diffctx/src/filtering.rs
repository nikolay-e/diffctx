use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::CODE_EXTENSIONS;
use crate::graph::{EdgeCategory, Graph};
use crate::types::{DiffHunk, Fragment, FragmentId, FragmentKind};

const PROXIMITY_FLOOR_MAX: f64 = 0.04;
const PROXIMITY_HALF_DECAY: f64 = 50.0;
const DEFINITION_PROXIMITY_HALF_DECAY: f64 = 5.0;
const HUB_REVERSE_THRESHOLD: usize = 2;
const MAX_CONTEXT_FRAGMENTS_PER_FILE: usize = 30;
const LOW_RELEVANCE_THRESHOLD: f64 = 0.015;
const SIZE_PENALTY_BASE_TOKENS: f64 = 100.0;
const SIZE_PENALTY_EXPONENT: f64 = 0.5;

fn fragment_hunk_gap(frag_start: u32, frag_end: u32, hunk_start: u32, hunk_end: u32) -> u32 {
    if frag_end < hunk_start {
        hunk_start - frag_end
    } else if frag_start > hunk_end {
        frag_start - hunk_end
    } else {
        0
    }
}

fn proximity_score(frag: &Fragment, file_hunks: &[(u32, u32)]) -> f64 {
    let min_gap = file_hunks
        .iter()
        .map(|&(h_start, h_end)| {
            fragment_hunk_gap(frag.start_line(), frag.end_line(), h_start, h_end)
        })
        .min()
        .unwrap_or(u32::MAX);
    let half_decay = if frag.kind == FragmentKind::Definition {
        DEFINITION_PROXIMITY_HALF_DECAY
    } else {
        PROXIMITY_HALF_DECAY
    };
    PROXIMITY_FLOOR_MAX / (1.0 + min_gap as f64 / half_decay)
}

fn effective_relevance_threshold(token_count: u32) -> f64 {
    let size_factor = (token_count as f64 / SIZE_PENALTY_BASE_TOKENS)
        .max(1.0)
        .powf(SIZE_PENALTY_EXPONENT);
    LOW_RELEVANCE_THRESHOLD * size_factor
}

pub fn apply_hunk_proximity_bonus(
    rel: &mut FxHashMap<FragmentId, f64>,
    core_ids: &FxHashSet<FragmentId>,
    fragments: &[Fragment],
    hunks: &[DiffHunk],
) {
    let mut hunks_by_path: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
    for h in hunks {
        let (h_start, h_end) = h.core_selection_range();
        hunks_by_path
            .entry(h.path.as_ref())
            .or_default()
            .push((h_start, h_end));
    }

    let bonuses: Vec<(FragmentId, f64)> = fragments
        .par_iter()
        .filter(|frag| !core_ids.contains(&frag.id))
        .filter_map(|frag| {
            let file_hunks = hunks_by_path.get(frag.path())?;
            let bonus = proximity_score(frag, file_hunks);
            Some((frag.id.clone(), bonus))
        })
        .collect();

    for (id, bonus) in bonuses {
        let current = rel.get(&id).copied().unwrap_or(0.0);
        if current < bonus {
            rel.insert(id, bonus);
        }
    }
}

fn classify_semantic_edges(
    graph: &Graph,
    changed_paths: &FxHashSet<Arc<str>>,
) -> (HashMap<Arc<str>, FxHashSet<Arc<str>>>, FxHashSet<Arc<str>>) {
    let mut reverse_deps: HashMap<Arc<str>, FxHashSet<Arc<str>>> = HashMap::new();
    let mut direct_edge_paths: FxHashSet<Arc<str>> = FxHashSet::default();

    for ((src, dst), category) in &graph.edge_categories {
        if *category != EdgeCategory::Semantic {
            continue;
        }
        let src_changed = changed_paths.contains(&src.path);
        let dst_changed = changed_paths.contains(&dst.path);
        if !(src_changed ^ dst_changed) {
            continue;
        }

        let (changed_frag, other_frag) = if src_changed { (src, dst) } else { (dst, src) };

        let fwd_w = graph
            .forward_edge_weight(changed_frag, other_frag)
            .unwrap_or(0.0);
        let rev_w = graph
            .forward_edge_weight(other_frag, changed_frag)
            .unwrap_or(0.0);

        if rev_w > fwd_w {
            reverse_deps
                .entry(changed_frag.path.clone())
                .or_default()
                .insert(other_frag.path.clone());
        } else {
            direct_edge_paths.insert(other_frag.path.clone());
        }
    }

    (reverse_deps, direct_edge_paths)
}

fn find_hub_noise_paths(graph: &Graph, changed_paths: &FxHashSet<Arc<str>>) -> FxHashSet<Arc<str>> {
    let (reverse_deps, direct_edge_paths) = classify_semantic_edges(graph, changed_paths);

    let changed_dirs: FxHashSet<String> = changed_paths
        .iter()
        .filter_map(|p| {
            Path::new(p.as_ref())
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
        })
        .collect();

    let mut noise_counts: HashMap<Arc<str>, usize> = HashMap::new();
    for (hub_path, deps) in &reverse_deps {
        if changed_paths.contains(hub_path) {
            continue;
        }
        if deps.len() >= HUB_REVERSE_THRESHOLD {
            for dep in deps {
                *noise_counts.entry(dep.clone()).or_insert(0) += 1;
            }
        }
    }

    noise_counts
        .into_iter()
        .filter(|(p, count)| {
            *count >= 1
                && !direct_edge_paths.contains(p)
                && !changed_dirs.contains(
                    &Path::new(p.as_ref())
                        .parent()
                        .map(|d| d.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
        })
        .map(|(p, _)| p)
        .collect()
}

fn find_config_generic_code_files(
    graph: &Graph,
    changed_paths: &FxHashSet<Arc<str>>,
) -> FxHashSet<Arc<str>> {
    let mut has_real_edge: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut has_generic_config: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut generic_edge_count: HashMap<Arc<str>, usize> = HashMap::new();
    let config_stems: FxHashSet<String> = changed_paths
        .iter()
        .filter_map(|p| {
            Path::new(p.as_ref())
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
        })
        .collect();

    for ((src, dst), category) in &graph.edge_categories {
        let src_changed = changed_paths.contains(&src.path);
        let dst_changed = changed_paths.contains(&dst.path);
        if !(src_changed ^ dst_changed) {
            continue;
        }
        let other_path = if src_changed { &dst.path } else { &src.path };
        match category {
            EdgeCategory::ConfigGeneric => {
                has_generic_config.insert(other_path.clone());
                *generic_edge_count.entry(other_path.clone()).or_insert(0) += 1;
            }
            EdgeCategory::Semantic | EdgeCategory::Config => {
                has_real_edge.insert(other_path.clone());
            }
            _ => {}
        }
    }

    let generic_only: FxHashSet<Arc<str>> = has_generic_config
        .difference(&has_real_edge)
        .cloned()
        .collect();

    generic_only
        .into_iter()
        .filter(|p| {
            let path = Path::new(p.as_ref());
            let ext = path
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
                .unwrap_or_default();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            CODE_EXTENSIONS.contains(ext.as_str())
                && generic_edge_count.get(p).copied().unwrap_or(0) <= 1
                && !config_stems.contains(&stem)
        })
        .collect()
}

pub fn filter_unrelated_fragments(
    fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    graph: &Graph,
) -> Vec<Fragment> {
    let changed_paths: FxHashSet<Arc<str>> = core_ids.iter().map(|fid| fid.path.clone()).collect();

    let mut paths_to_remove = find_hub_noise_paths(graph, &changed_paths);
    let config_generic = find_config_generic_code_files(graph, &changed_paths);
    for p in config_generic {
        paths_to_remove.insert(p);
    }
    for p in &changed_paths {
        paths_to_remove.remove(p);
    }

    fragments
        .iter()
        .filter(|f| !paths_to_remove.contains(&f.id.path))
        .cloned()
        .collect()
}

pub fn filter_low_relevance(
    fragments: Vec<Fragment>,
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
) -> Vec<Fragment> {
    fragments
        .into_iter()
        .filter(|f| {
            core_ids.contains(&f.id)
                || rel.get(&f.id).copied().unwrap_or(0.0)
                    >= effective_relevance_threshold(f.token_count)
        })
        .collect()
}

pub fn filter_positive_relevance(
    fragments: Vec<Fragment>,
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
) -> Vec<Fragment> {
    fragments
        .into_iter()
        .filter(|f| core_ids.contains(&f.id) || rel.get(&f.id).copied().unwrap_or(0.0) > 0.0)
        .collect()
}

pub fn cap_context_fragments(
    fragments: Vec<Fragment>,
    core_ids: &FxHashSet<FragmentId>,
    rel: &FxHashMap<FragmentId, f64>,
) -> Vec<Fragment> {
    let changed_paths: FxHashSet<Arc<str>> = core_ids.iter().map(|fid| fid.path.clone()).collect();

    let mut ctx_by_path: HashMap<Arc<str>, Vec<Fragment>> = HashMap::new();
    let mut result: Vec<Fragment> = Vec::new();

    for f in fragments {
        if changed_paths.contains(&f.id.path) {
            result.push(f);
        } else {
            ctx_by_path.entry(f.id.path.clone()).or_default().push(f);
        }
    }

    for (_path, mut file_frags) in ctx_by_path {
        if file_frags.len() <= MAX_CONTEXT_FRAGMENTS_PER_FILE {
            result.extend(file_frags);
        } else {
            file_frags.sort_by(|a, b| {
                let sa = rel.get(&a.id).copied().unwrap_or(0.0);
                let sb = rel.get(&b.id).copied().unwrap_or(0.0);
                sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
            });
            result.extend(file_frags.into_iter().take(MAX_CONTEXT_FRAGMENTS_PER_FILE));
        }
    }

    result
}
