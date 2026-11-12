//! The scored state of a run and what a selection over it returns: read by every
//! renderer and answer, below the pipeline that builds them.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::mode::PipelineConfig;
use crate::scoring::ScoringResult;
use crate::types::{Fragment, FragmentId};
use crate::utility::InformationNeed;

/// Per-instance heavy-phase outputs cached for reuse across many
/// (`tau`, `core_budget_fraction`) selection cells. The selection /
/// post-pass / render pipeline then runs against this state cheaply.
///
/// All fields are owned, no shared external lifetimes; safe to move
/// into a `pyclass` and hand back to Python.
pub struct ScoredState {
    pub root_dir: PathBuf,
    pub config: PipelineConfig,
    pub all_fragments: Vec<Fragment>,
    pub core_ids: FxHashSet<FragmentId>,
    /// Narrow stand-ins for the core fragments that have no signature variant,
    /// keyed by the core they replace. Deliberately outside `all_fragments`:
    /// they are budget fallbacks, not graph nodes or context candidates.
    pub core_excerpts: FxHashMap<FragmentId, Fragment>,
    pub scoring_result: ScoringResult,
    pub needs: Vec<InformationNeed>,
    pub changed_files: Vec<PathBuf>,
    /// The `changed_files` git does not track yet: new work in a working
    /// tree, named apart so a count of changed files is what git would
    /// commit (#418).
    pub untracked_files: Vec<PathBuf>,
    pub deleted_files: Vec<String>,
    pub renamed_files: Vec<(String, String)>,
    /// Lock files touched by the range, paths only: the dependency bump is
    /// signal, the checksum churn is not (#112).
    pub lockfile_changes: Vec<String>,
    /// Changed files withheld by ignore rules (.diffctx/ignore, gitignore).
    /// Listed so the omission is visible: a reader of the output cannot
    /// otherwise tell a file the diff never touched from one the tool
    /// filtered, and #188 documents a reviewer concluding "no tests" from
    /// exactly that silence.
    pub ignored_changes: Vec<String>,
    /// Changed files withheld by `.diffctx/ignore` or the secret-path
    /// heuristics. Count only: both policies' point is that these paths stay
    /// out of the artifact (#85, #214), but a bare number still tells the
    /// reader the output is deliberately incomplete.
    pub policy_excluded_count: usize,
    /// Which discovery strategy first surfaced each discovered path.
    ///
    /// Read-only telemetry for the universe ceiling (#130): without it, a gold
    /// file that never reaches the output is indistinguishable between "no
    /// strategy found it" and "found but outranked", and those need different
    /// fixes. Empty for the changed files themselves, which are not discovered.
    pub discovery_source: FxHashMap<Arc<str>, &'static str>,
    pub commit_message: Option<String>,
    /// Every subject of a multi-commit range, newest first (empty otherwise).
    pub commit_messages: Vec<String>,
    /// Commits in the range; `commit_messages` holds at most the newest twenty.
    pub commit_count: usize,
    /// `(display path, class, reason)` per changed file — the priority the
    /// evidence floor ranks by and the inventory row the artifact prints.
    pub change_classes: Vec<(String, crate::change_class::ChangeClass, &'static str)>,
    pub heavy_latency_ms: HeavyLatencyMs,
    /// Everything a reader needs to reproduce this state: the resolved
    /// configuration and its hash, the input revisions, the limits in force.
    pub provenance: crate::run_provenance::RunProvenance,
    /// The deadline, the resource caps and the log of what limited the run —
    /// read by every renderer for the coverage block.
    pub run: crate::resource::RunContext,
    /// The change summary's token cost (#241), computed once here: it depends
    /// on nothing a sweep cell changes, and `select_with_params` re-runs
    /// selection per (tau, cbf) cell against this one state.
    pub envelope_tokens: u32,
    /// The captured index tree when the run analyses staged changes: the
    /// identity of the snapshot every current-side byte came from.
    pub index_tree: Option<String>,
    /// The range as resolved — a duration's base commit, the staged
    /// snapshot's `base..tree` — which every git question about the change
    /// must use; the caller's spelling is for display.
    pub analysed_range: Option<String>,
    /// The diff's hunks with their removed and added lines: whether a change
    /// is layout only, what it removed, where its lines are.
    pub hunk_texts: Vec<crate::change_class::HunkText>,
    /// See `ScoredFragments::changed_lines`.
    pub changed_lines: FxHashMap<FragmentId, f64>,
    /// The pathspecs that narrow the run (`paths`, or the subtree it was asked
    /// about); empty for the whole repository. What answers from a snapshot
    /// reads the files inside it alone.
    pub scope: Vec<String>,
}

#[derive(Default, Clone, Copy)]
pub struct HeavyLatencyMs {
    /// Everything before the heavy phase begins: hunk parse, untracked scan,
    /// ignore resolution, and the `git diff` / `--name-only` / rename calls.
    /// Outside every timer until #183 — which is why a 182s run reported 5.3s of
    /// instrumented work with nothing to say where the rest went.
    pub pre_phase: f64,
    pub parse_changed: f64,
    pub universe_walk: f64,
    pub discovery: f64,
    pub parse_discovered: f64,
    pub tokenization: f64,
    pub graph_build: f64,
    pub scoring: f64,
}

pub struct SelectionOutcome {
    pub selected: Vec<Fragment>,
    /// The budget the caller asked for (or auto-sizing produced). Reported as
    /// the contract; it is NOT what the selection had to spend.
    pub effective_budget: u32,
    /// What was actually left for fragments after the change summary was
    /// charged (#241). Anything reasoning about headroom — "did the budget
    /// stop us, or did tau?" — has to use this one, or it reads the envelope
    /// as free space and concludes the budget was never binding.
    pub selection_budget: u32,
    pub selection_iters: usize,
    pub stopping_certificate: f64,
    pub select_ms: f64,
    /// See `SelectionResult::stand_in_ids` — carried to the renderers so both
    /// surfaces read one recorded fact instead of re-deriving it (#209).
    pub stand_in_ids: FxHashSet<FragmentId>,
    /// What was actually asked for, so provenance says the requested cap and
    /// the auto-sized one apart.
    pub budget_requested: Option<u32>,
    pub tau: f64,
    pub gated: bool,
    /// What this selection could not honour — per outcome, not per state,
    /// because one state serves many budgets in a sweep.
    pub limit_reasons: Vec<crate::resource::LimitReason>,
    /// The range's commit messages as this budget affords them.
    pub commit_messages: Vec<String>,
}

impl SelectionOutcome {
    pub fn selection_provenance(&self) -> crate::run_provenance::Selection {
        crate::run_provenance::Selection {
            budget_tokens: self.effective_budget,
            budget_requested: self.budget_requested,
            tau: self.tau,
            gate: if self.gated { "admission" } else { "none" },
        }
    }
}

/// Changed files the fragmenter gave nothing to: no budget can represent
/// them, so they must not be reported as dropped by one.
pub(crate) fn fragmentless_changed_files(
    root_dir: &Path,
    changed_files: &[PathBuf],
    fragments: &[Fragment],
) -> FxHashSet<String> {
    let fragmented: FxHashSet<&str> = fragments.iter().map(|f| f.path()).collect();
    changed_files
        .iter()
        .filter(|p| !fragmented.contains(p.to_string_lossy().as_ref()))
        .map(|p| crate::paths::display_rel_or_abs(root_dir, p))
        .collect()
}

/// The files an answer is about — the changed ones and those it selected
/// from — by display path: a per-file limit elsewhere cost it nothing.
/// A code file the run could not read is about the answer too when it
/// imports a changed module: a caller dropped for its size is lost context.
pub(crate) struct AnswerScope {
    root: PathBuf,
    paths: FxHashSet<String>,
    stems: Vec<String>,
    source: crate::source::Source,
}

impl AnswerScope {
    pub(crate) fn contains(&self, display_path: &str) -> bool {
        if self.paths.contains(display_path) {
            return true;
        }
        let ext = crate::edges::base::file_ext(Path::new(display_path));
        if self.stems.is_empty()
            || !crate::config::extensions::CODE_EXTENSIONS.contains(ext.as_str())
        {
            return false;
        }
        let Some(content) = self.source.read_bytes(&self.root.join(display_path)) else {
            return false;
        };
        String::from_utf8_lossy(&content).lines().any(|line| {
            let head = line.trim_start();
            ["import ", "from ", "use ", "#include", "require", "mod "]
                .iter()
                .any(|k| head.starts_with(k))
                && head
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| self.stems.iter().any(|s| s == w))
        })
    }
}

pub(crate) fn answer_paths<'a>(
    state: &ScoredState,
    selected: impl Iterator<Item = &'a Fragment>,
) -> AnswerScope {
    let stems = state
        .changed_files
        .iter()
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()))
        .filter(|s| s.len() >= 3 && !matches!(*s, "mod" | "lib" | "main" | "index" | "__init__"))
        .map(str::to_string)
        .collect();
    AnswerScope {
        source: state.run.source(),
        root: state.root_dir.clone(),
        paths: state
            .changed_files
            .iter()
            .map(|p| crate::paths::display_rel_or_abs(&state.root_dir, p))
            .chain(
                selected.map(|f| {
                    crate::paths::display_rel_or_abs(&state.root_dir, Path::new(f.path()))
                }),
            )
            .collect(),
        stems,
    }
}

pub(crate) fn seed_hops(state: &ScoredState) -> FxHashMap<FragmentId, u32> {
    let graph = &state.scoring_result.graph;
    let mut hops: FxHashMap<FragmentId, u32> = FxHashMap::default();
    let mut queue: VecDeque<FragmentId> = VecDeque::new();
    for core in &state.core_ids {
        hops.insert(core.clone(), 0);
        queue.push_back(core.clone());
    }
    // Relevance flows along edges in both directions (PPR blends forward and
    // backward pushes), so distance is measured on the undirected graph.
    let mut undirected: FxHashMap<FragmentId, Vec<FragmentId>> = FxHashMap::default();
    graph.for_each_categorized_edge(|src, dst, _| {
        undirected.entry(src.clone()).or_default().push(dst.clone());
        undirected.entry(dst.clone()).or_default().push(src.clone());
    });
    while let Some(node) = queue.pop_front() {
        let d = hops[&node];
        if let Some(neighbors) = undirected.get(&node) {
            for n in neighbors {
                if !hops.contains_key(n) {
                    hops.insert(n.clone(), d + 1);
                    queue.push_back(n.clone());
                }
            }
        }
    }
    hops
}

struct CatMass {
    mass: f64,
    top_source: FragmentId,
    top_contribution: f64,
}

fn per_category_mass(
    state: &ScoredState,
) -> FxHashMap<FragmentId, FxHashMap<&'static str, CatMass>> {
    let graph = &state.scoring_result.graph;
    let rel = &state.scoring_result.rel_scores;
    let mut mass: FxHashMap<FragmentId, FxHashMap<&'static str, CatMass>> = FxHashMap::default();
    graph.for_each_categorized_edge(|src, dst, cat| {
        let src_rel = rel.get(src).copied().unwrap_or(0.0);
        if src_rel <= 0.0 {
            return;
        }
        let w = graph.forward_edge_weight(src, dst).unwrap_or(0.0);
        if w <= 0.0 {
            return;
        }
        let contribution = w * src_rel;
        let entry = mass
            .entry(dst.clone())
            .or_default()
            .entry(cat.as_str())
            .or_insert_with(|| CatMass {
                mass: 0.0,
                top_source: src.clone(),
                top_contribution: 0.0,
            });
        entry.mass += contribution;
        // Strict `>` with deterministic iteration would still tie-break by
        // visit order; prefer the lexically-smaller source on equal
        // contribution so the attribution is order-independent.
        if contribution > entry.top_contribution
            || (contribution == entry.top_contribution && src.path < entry.top_source.path)
        {
            entry.top_source = src.clone();
            entry.top_contribution = contribution;
        }
    });
    mass
}

/// Per-fragment incoming relevance mass grouped by edge category, sorted by
/// mass descending: `(category, strongest_source_path, mass)`. Shared by the
/// provenance dump and the locate renderer (#126) — one attribution pass,
/// two consumers.
pub(crate) fn incoming_attribution(
    state: &ScoredState,
) -> FxHashMap<FragmentId, Vec<(String, String, f64)>> {
    per_category_mass(state)
        .into_iter()
        .map(|(id, cats)| {
            let mut rows: Vec<(String, String, f64)> = cats
                .into_iter()
                .map(|(cat, m)| (cat.to_string(), m.top_source.path.to_string(), m.mass))
                .collect();
            rows.sort_by(|a, b| {
                b.2.partial_cmp(&a.2)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.0.cmp(&b.0))
            });
            (id, rows)
        })
        .collect()
}

pub(crate) fn incoming_mass(
    state: &ScoredState,
) -> FxHashMap<FragmentId, FxHashMap<&'static str, f64>> {
    per_category_mass(state)
        .into_iter()
        .map(|(id, cats)| (id, cats.into_iter().map(|(c, m)| (c, m.mass)).collect()))
        .collect()
}
