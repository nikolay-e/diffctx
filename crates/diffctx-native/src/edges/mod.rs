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

use crate::types::Fragment;

use self::base::EdgeBuilder;

/// Raw-weight floor above which a Semantic, Config or TestEdge edge counts as
/// a naming channel (imports/using, calls, type and member refs at >=0.35 and
/// test_direct/test_naming at 0.60/0.50 clear it; tags fallback at 0.30,
/// test_reverse at 0.30 and same-package/namespace markers at 0.05 stay
/// diffuse — `naming_reachable_files` walks undirected, so the reverse arc
/// adds nothing the forward one lacks).
pub const NAMING_WEIGHT_FLOOR: f64 = 0.30;

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
    deadline: crate::deadline::Deadline,
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
    collect_capped_edges_from(all_builders, fragments, repo_root, deadline)
}

/// The builder list is a parameter so the merge rules can be exercised with
/// builders that emit exactly the pair under test; the shipped list goes
/// through `collect_capped_edges`.
fn collect_capped_edges_from(
    all_builders: Vec<(&str, Box<dyn EdgeBuilder>)>,
    fragments: &[Fragment],
    repo_root: Option<&Path>,
    deadline: crate::deadline::Deadline,
) -> CappedEdges {
    let (node_to_idx, idx_to_node) = intern_fragment_nodes(fragments);
    let category_weights = *crate::config::category_weights::CATEGORY_WEIGHTS;
    let builder_meta: Vec<(EdgeCategory, f64)> = all_builders
        .iter()
        .map(|(cat_name, builder)| {
            let category = EdgeCategory::from_str(builder.category_label().unwrap_or(cat_name));
            (category, category_weights.multiplier(category))
        })
        .collect();
    let fallback_flags: Vec<bool> = all_builders
        .iter()
        .map(|(_, builder)| builder.is_fallback())
        .collect();

    let per_builder_log: Vec<Vec<LoggedEmission>> = all_builders
        .par_iter()
        .enumerate()
        .map(|(builder_idx, (name, builder))| {
            deadline.check("edge construction");
            let _in_builder = deadline.enter();
            let t = std::time::Instant::now();
            let edges = builder.build(fragments, repo_root);
            if std::env::var_os("DIFFCTX_TRACE_BUILDERS").is_some() {
                // The index is the registration order within
                // builder_categories(); category names alone cannot tell two
                // semantic builders apart when one of them floods.
                eprintln!(
                    "builder {name}[{builder_idx}]: {:.1}s, {} edges",
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

    // Fallback builders (tags) only count where the dedicated builders came
    // back empty: an emission survives only if at least one endpoint file has
    // no dedicated semantic edge. Dual coverage was measured as pure noise —
    // a tags edge duplicating a real import/call edge adds mass, not reach —
    // while a parser-degraded file genuinely has nothing else (#131).
    // A per-pair cross-language escape was tried and measured net-negative
    // (#217, 2026-08-19: −4/+0 corpus — `.ts`/`.tsx` count as different
    // languages, so same-project pairs slipped back in); a retry needs
    // language-FAMILY granularity first.
    let mut per_builder_log = per_builder_log;
    if fallback_flags.iter().any(|&f| f) {
        let mut dedicated_files: FxHashSet<&str> = FxHashSet::default();
        for (builder_idx, log) in per_builder_log.iter().enumerate() {
            if fallback_flags[builder_idx] || builder_meta[builder_idx].0 != EdgeCategory::Semantic
            {
                continue;
            }
            for e in log {
                dedicated_files.insert(idx_to_node[e.src as usize].path.as_ref());
                dedicated_files.insert(idx_to_node[e.dst as usize].path.as_ref());
            }
        }
        for (builder_idx, log) in per_builder_log.iter_mut().enumerate() {
            if !fallback_flags[builder_idx] {
                continue;
            }
            log.retain(|e| {
                !dedicated_files.contains(idx_to_node[e.src as usize].path.as_ref())
                    || !dedicated_files.contains(idx_to_node[e.dst as usize].path.as_ref())
            });
        }
    }

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
                // Naming classification uses the RAW builder weight: the
                // channel constants are quantized (0.05 markers, 0.30 tags,
                // >=0.35 symbol/file-naming channels), so the floor selects a
                // fixed channel set rather than trading a scalar band; raw
                // rather than damped so a hub-suppressed import keeps its
                // naming status.
                // TestEdge included (#217): a `widget_test.go` in the same
                // package, or a `widget.test.ts` with no import, is related to
                // its source purely by naming convention — the exact relation
                // the admission gate reads — and no Semantic channel carries it.
                //
                // Classified under THIS emission's builder, not the pair's
                // canonical category: the weight is this builder's, so the
                // category the floor is read against must be too. Reading it
                // against the first builder's let a 0.05 semantic marker plus
                // a 0.8 similarity emission on the same pair pass as a naming
                // edge — the similarity weight cleared the floor wearing the
                // semantic label — and the merge ORs naming across builders,
                // so the admission gate then read a text-similarity link as a
                // naming relation.
                let naming = (builder_category == EdgeCategory::Semantic
                    || builder_category == EdgeCategory::Config
                    || builder_category == EdgeCategory::TestEdge)
                    && e.weight > NAMING_WEIGHT_FLOOR;
                push_bounded_top_k(
                    per_source.entry(e.src).or_default(),
                    RankedCandidate {
                        weight: damped,
                        dst: e.dst,
                        category,
                        naming,
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
                        naming: c.naming,
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

/// Files reachable from the core set through naming-class edges only, within
/// `max_depth` hops (#65 per-file admission). Diffuse channels (markers,
/// tags, similarity, structural stars) do not open a file; a file whose only
/// connection is proximity never enters the admissible set.
pub fn naming_reachable_files(
    capped: &CappedEdges,
    core_ids: &rustc_hash::FxHashSet<crate::types::FragmentId>,
    max_depth: usize,
) -> rustc_hash::FxHashSet<std::sync::Arc<str>> {
    reachable_files(capped, core_ids, max_depth, |e| e.naming)
}

/// Files reachable through DECLARED-relation edges — any Semantic, Config,
/// ConfigGeneric, Document or TestEdge channel — within `max_depth` hops.
/// Wider than `naming_reachable_files`: it admits the low-weight declared
/// channels (config keys, tags at 0.30, latex/terraform references) while
/// still refusing proximity, lexical similarity and co-change. This is the
/// admission bar for the singleton comparators (#211): they recover
/// weak-but-declared relations, which the strict naming floor was measured
/// to starve (8 recall regressions: config→code, terraform data sources,
/// latex packages).
///
/// The `> 0.05` floor is on the DAMPED weight (what CompactEdge carries):
/// same-package/namespace markers emit at exactly 0.05 raw and damping only
/// lowers, so they can never clear it — without the floor they made every
/// same-package sibling "declared" and the gate admitted the same junk it
/// exists to block (measured: 143 of 146 corpus improvements lost).
pub fn declared_reachable_files(
    capped: &CappedEdges,
    core_ids: &rustc_hash::FxHashSet<crate::types::FragmentId>,
    max_depth: usize,
) -> rustc_hash::FxHashSet<std::sync::Arc<str>> {
    reachable_files(capped, core_ids, max_depth, |e| {
        e.weight > 0.05
            && matches!(
                e.category,
                EdgeCategory::Semantic
                    | EdgeCategory::Config
                    | EdgeCategory::ConfigGeneric
                    | EdgeCategory::Document
                    | EdgeCategory::TestEdge
            )
    })
}

fn reachable_files(
    capped: &CappedEdges,
    core_ids: &rustc_hash::FxHashSet<crate::types::FragmentId>,
    max_depth: usize,
    keep: impl Fn(&CompactEdge) -> bool,
) -> rustc_hash::FxHashSet<std::sync::Arc<str>> {
    let n = capped.idx_to_node.len();
    // Undirected on purpose: a naming edge relates the PAIR of files. For
    // several channels the reverse emission (weight*reverse_factor) lands
    // below the naming floor, so a directed walk would reach the changed
    // set's dependencies but not all of its consumers — measured as 24
    // broken consumer-pull corpus cases (php one-hop, terraform dependents,
    // DI).
    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); n];
    for e in &capped.edges {
        if keep(e) {
            adj[e.src as usize].push(e.dst);
            adj[e.dst as usize].push(e.src);
        }
    }
    let mut seen = vec![false; n];
    let mut frontier: Vec<u32> = Vec::new();
    for (i, id) in capped.idx_to_node.iter().enumerate() {
        if core_ids.contains(id) {
            seen[i] = true;
            frontier.push(i as u32);
        }
    }
    let mut files: rustc_hash::FxHashSet<std::sync::Arc<str>> = frontier
        .iter()
        .map(|&i| capped.idx_to_node[i as usize].path.clone())
        .collect();
    for _ in 0..max_depth {
        let mut next = Vec::new();
        for &u in &frontier {
            for &v in &adj[u as usize] {
                if !seen[v as usize] {
                    seen[v as usize] = true;
                    files.insert(capped.idx_to_node[v as usize].path.clone());
                    next.push(v);
                }
            }
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }
    files
}

#[cfg(test)]
mod fallback_gate_tests {
    use super::*;
    use rustc_hash::FxHashSet as Set;
    use std::sync::Arc;

    fn frag(path: &str, content: &str, idents: &[&str]) -> Fragment {
        Fragment {
            id: crate::types::FragmentId::new(Arc::from(path), 1, 10),
            kind: crate::types::FragmentKind::Function,
            content: Arc::from(content),
            identifiers: idents.iter().map(|s| s.to_string()).collect::<Set<_>>(),
            token_count: 10,
            symbol_name: None,
        }
    }

    #[test]
    fn tags_edges_survive_only_where_dedicated_builders_came_back_empty() {
        // a.py <-> b.py carry a dedicated import edge; a.py and c.py share an
        // identifier but nothing imports between them. Two .xyz files have no
        // dedicated builder at all and share the same identifier.
        let fragments = vec![
            frag(
                "proj/a.c",
                "#include \"bdep.h\"\nint zzcommonzz;\n",
                &["zzcommonzz"],
            ),
            frag("proj/bdep.h", "int bdecl(void);\n", &["bdecl"]),
            frag(
                "proj/c.c",
                "#include \"ddep.h\"\nint zzcommonzz;\n",
                &["zzcommonzz"],
            ),
            frag("proj/ddep.h", "int ddecl(void);\n", &["ddecl"]),
            frag("proj/u1.xyz", "zzcommonzz here\n", &["zzcommonzz"]),
            frag("proj/u2.xyz", "zzcommonzz there\n", &["zzcommonzz"]),
        ];
        let capped =
            collect_capped_edges(&fragments, None, false, crate::deadline::Deadline::none());
        let node_path = |idx: u32| capped.idx_to_node[idx as usize].path.clone();
        // Category matters: a.py and c.py legitimately share a structural
        // sibling edge; the class under test is the SEMANTIC tags link.
        let has = |a: &str, b: &str| {
            capped.edges.iter().any(|e| {
                if e.category != EdgeCategory::Semantic {
                    return false;
                }
                let s = node_path(e.src);
                let d = node_path(e.dst);
                (s.ends_with(a) && d.ends_with(b)) || (s.ends_with(b) && d.ends_with(a))
            })
        };
        assert!(
            has("u1.xyz", "u2.xyz"),
            "fallback must still connect files no dedicated builder covers"
        );
        assert!(
            !has("a.c", "c.c"),
            "a tags-only link between two dedicated-covered files is the measured noise class (#131)"
        );
    }
}

#[cfg(test)]
mod naming_merge_tests {
    use super::*;
    use rustc_hash::FxHashSet as Set;
    use std::sync::Arc;

    struct FixedPair {
        label: &'static str,
        weight: f64,
    }

    impl EdgeBuilder for FixedPair {
        fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
            let mut edges = EdgeDict::default();
            edges.insert(
                (fragments[0].id.clone(), fragments[1].id.clone()),
                self.weight,
            );
            edges
        }
        fn category_label(&self) -> Option<&str> {
            Some(self.label)
        }
    }

    fn frag(path: &str) -> Fragment {
        Fragment {
            id: crate::types::FragmentId::new(Arc::from(path), 1, 10),
            kind: crate::types::FragmentKind::Function,
            content: Arc::from("x"),
            identifiers: Set::default(),
            token_count: 10,
            symbol_name: None,
        }
    }

    fn naming_of(builders: Vec<(&'static str, Box<dyn EdgeBuilder>)>) -> bool {
        let fragments = vec![frag("proj/a.py"), frag("proj/b.py")];
        let capped = collect_capped_edges_from(
            builders,
            &fragments,
            None,
            crate::deadline::Deadline::none(),
        );
        assert_eq!(capped.edges.len(), 1, "one merged pair expected");
        capped.edges[0].naming
    }

    /// A weak semantic marker and a strong similarity emission on one pair.
    /// The pair's canonical category is Semantic (first builder), and the
    /// merge takes the max weight — 0.8 — so reading the floor against the
    /// canonical category made this a naming edge. Neither emission is one on
    /// its own terms, so the merge must not be either.
    #[test]
    fn a_strong_similarity_emission_does_not_inherit_naming_from_a_weak_semantic_one() {
        let naming = naming_of(vec![
            (
                "semantic",
                Box::new(FixedPair {
                    label: "semantic",
                    weight: 0.05,
                }),
            ),
            (
                "similarity",
                Box::new(FixedPair {
                    label: "similarity",
                    weight: 0.8,
                }),
            ),
        ]);
        assert!(
            !naming,
            "similarity weight cleared the floor under the semantic label"
        );
    }

    #[test]
    fn a_semantic_emission_above_the_floor_is_a_naming_edge() {
        let naming = naming_of(vec![(
            "semantic",
            Box::new(FixedPair {
                label: "semantic",
                weight: 0.5,
            }),
        )]);
        assert!(naming);
    }
}
