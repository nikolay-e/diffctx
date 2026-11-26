//! The corpus harness: an in-memory repository run through the engine's own
//! pipeline phases, so a case scores the system that ships (#149, #232).

use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use similar::{ChangeTag, TextDiff};

use diffctx_answer::render::{DiffContextOutput, build_diff_context_output};
use diffctx_base::types::{DiffHunk, Fragment, FragmentId};
use diffctx_config::config::budget::BUDGET;
use diffctx_config::config::tokenization::TOKENIZATION;
use diffctx_config::mode::{PipelineConfig, ScoringMode};
use diffctx_facts::parsers::fragment_file;

pub(crate) struct MemoryRepo {
    pub name: String,
    pub initial_files: FxHashMap<String, String>,
    pub changed_files: FxHashMap<String, String>,
}

pub(crate) struct FragmentRow {
    pub kind: &'static str,
    pub symbol: Option<String>,
    pub start_line: u32,
    pub end_line: u32,
}

pub(crate) fn fragment_rows(path: &str, content: &str) -> Vec<FragmentRow> {
    let mut rows: Vec<FragmentRow> = fragment_file(Arc::from(path), content)
        .into_iter()
        .map(|f| FragmentRow {
            kind: f.kind.as_str(),
            symbol: f.symbol_name,
            start_line: f.id.start_line,
            end_line: f.id.end_line,
        })
        .collect();
    rows.sort_by(|a, b| {
        (a.start_line, a.end_line, a.kind, &a.symbol).cmp(&(
            b.start_line,
            b.end_line,
            b.kind,
            &b.symbol,
        ))
    });
    rows
}

/// A cross-file edge of the graph the engine builds over a repository's
/// final state, as the file pair it relates and the category that survived
/// the merge.
pub(crate) struct FileEdge {
    pub category: &'static str,
    pub src: String,
    pub dst: String,
    pub weight: f64,
}

/// The fragments of every file of the tree after the change.
fn final_state_fragments(repo: &MemoryRepo) -> Vec<Fragment> {
    let all_files = merge_file_contents(&repo.initial_files, &repo.changed_files);
    let mut paths: Vec<&String> = all_files.keys().collect();
    paths.sort();
    let mut fragments: Vec<Fragment> = Vec::new();
    let mut seen: FxHashSet<FragmentId> = FxHashSet::default();
    for path in paths {
        for f in fragment_file(Arc::from(path.as_str()), &all_files[path]) {
            if seen.insert(f.id.clone()) {
                fragments.push(f);
            }
        }
    }
    fragments
}

/// The final tree written to a temporary repository, so the builders and
/// discovery that read a snapshot have one to read. Removed on drop. A
/// repository that cannot be made leaves the harness as it was: no root.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn write(files: &FxHashMap<String, String>) -> Option<Self> {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "diffctx-memory-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).ok()?;
        let tree = Self { root };
        for (path, content) in files {
            let at = tree.root.join(path);
            std::fs::create_dir_all(at.parent()?).ok()?;
            std::fs::write(at, content).ok()?;
        }
        let git = |args: &[&str]| {
            diffctx_snapshot::snapshot::git_command(&tree.root)
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .output()
                .is_ok_and(|out| out.status.success())
        };
        (git(&["init", "-q"])
            && git(&["add", "-A"])
            && git(&[
                "-c",
                "user.name=memory",
                "-c",
                "user.email=memory@localhost",
                "commit",
                "-q",
                "-m",
                "tree",
            ]))
        .then_some(tree)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every cross-file edge `collect_capped_edges` keeps over the files of the
/// repository after the change — the whole final tree, not the discovery
/// universe of one run, so a link is measured whether or not a given diff
/// would have admitted its endpoint. Selection is untouched.
pub(crate) fn final_state_edges(repo: &MemoryRepo) -> Vec<FileEdge> {
    let fragments = final_state_fragments(repo);
    let tree = TempTree::write(&merge_file_contents(
        &repo.initial_files,
        &repo.changed_files,
    ));
    let capped = diffctx_graph::edges::collect_capped_edges(
        &fragments,
        tree.as_ref().map(|t| t.root.as_path()),
        false,
        &diffctx_snapshot::resource::RunContext::unbounded(),
    );
    let mut out: Vec<FileEdge> = capped
        .edges
        .iter()
        .filter_map(|e| {
            let src = &capped.idx_to_node[e.src as usize].path;
            let dst = &capped.idx_to_node[e.dst as usize].path;
            (src != dst).then(|| FileEdge {
                category: e.category.as_str(),
                src: src.to_string(),
                dst: dst.to_string(),
                weight: e.weight,
            })
        })
        .collect();
    out.sort_by(|a, b| (&a.src, &a.dst, a.category).cmp(&(&b.src, &b.dst, b.category)));
    out
}

/// A link of the typed key table: two files that name the same environment
/// variable or image, with the key and how each file spells it.
pub(crate) struct TypedKeyLink {
    pub kind: &'static str,
    pub strength: &'static str,
    pub key: String,
    pub a: String,
    pub b: String,
    pub a_mechanism: &'static str,
    pub b_mechanism: &'static str,
}

/// The typed key links that count for the change, before they become edges:
/// what the `key_links` gate scores. A changed file's untouched key is not
/// part of the change.
pub(crate) fn typed_key_links(repo: &MemoryRepo) -> Vec<TypedKeyLink> {
    let fragments = final_state_fragments(repo);
    let run = diffctx_snapshot::resource::RunContext::unbounded();
    run.set_changed_lines(diffctx_base::changed_lines::ChangedLines::from_diff(
        &compute_memory_diff_text(&repo.initial_files, &repo.changed_files),
    ));
    let _in_run = run.enter();
    let mut links: Vec<TypedKeyLink> = diffctx_graph::keytable::typed_links(&fragments, None)
        .into_iter()
        .map(|l| TypedKeyLink {
            kind: l.kind,
            strength: l.strength,
            key: l.key,
            a: l.a_path,
            b: l.b_path,
            a_mechanism: l.a_mechanism,
            b_mechanism: l.b_mechanism,
        })
        .collect();
    links.sort_by(|x, y| (&x.kind, &x.key, &x.a, &x.b).cmp(&(&y.kind, &y.key, &y.a, &y.b)));
    links
}

pub(crate) fn build_diff_context_in_memory(
    repo: &MemoryRepo,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: Option<f64>,
    no_content: bool,
    scoring_mode: ScoringMode,
) -> DiffContextOutput {
    let hunks = compute_memory_hunks(&repo.initial_files, &repo.changed_files);
    if hunks.is_empty() {
        return DiffContextOutput::empty(&repo.name);
    }

    let diff_text = compute_memory_diff_text(&repo.initial_files, &repo.changed_files);
    let all_files = merge_file_contents(&repo.initial_files, &repo.changed_files);

    let changed_paths: FxHashSet<String> =
        hunks.iter().map(|h| h.path.as_ref().to_string()).collect();

    let changed_file_paths: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    let all_file_paths: Vec<PathBuf> = all_files.keys().map(PathBuf::from).collect();
    let file_cache: FxHashMap<PathBuf, String> = all_files
        .iter()
        .map(|(k, v)| (PathBuf::from(k), v.clone()))
        .collect();

    let mut config = PipelineConfig::from_mode(scoring_mode);
    config.ppr_alpha = alpha;
    let run = diffctx_snapshot::resource::RunContext::unbounded();
    run.set_changed_lines(diffctx_base::changed_lines::ChangedLines::from_diff(
        &diff_text,
    ));
    let _in_run = run.enter();

    // The product's discovery ensemble — structural, test-file, BM25 top-k —
    // over the in-memory files. The harness used to run the structural
    // strategy alone, so the corpus measured a narrower universe than the
    // one shipped (#232): a test file or a lexical neighbour the product
    // would have offered the selector never reached the oracle here.
    let expansion_concepts: FxHashSet<String> = diffctx_base::types::extract_identifiers(
        &diff_text,
        TOKENIZATION.query_min_identifier_length,
    )
    .into_iter()
    .collect();
    let tree = TempTree::write(&all_files);
    let tree_root = tree.as_ref().map(|t| t.root.clone());
    let discovery_ctx = diffctx_graph::discovery::DiscoveryContext {
        root_dir: tree_root.clone().unwrap_or_else(|| PathBuf::from(".")),
        changed_files: changed_file_paths.clone(),
        all_candidates: all_file_paths.clone(),
        diff_text: diff_text.clone(),
        expansion_concepts,
        file_cache: file_cache.clone(),
        token_corpus: std::sync::OnceLock::new(),
        source: diffctx_snapshot::source::Source::empty(),
        run: run.clone(),
    };
    let (discovered, _attribution) =
        _diffctx::pipeline::create_discovery(&config, false).discover_attributed(&discovery_ctx);
    let discovered_paths: FxHashSet<String> = discovered
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect();

    let allowed_paths: FxHashSet<&str> = changed_paths
        .iter()
        .chain(discovered_paths.iter())
        .map(|s| s.as_str())
        .collect();

    let mut all_fragments: Vec<Fragment> = Vec::new();
    let mut seen: FxHashSet<FragmentId> = FxHashSet::default();
    for (path, content) in &all_files {
        if !allowed_paths.contains(path.as_str()) {
            continue;
        }
        let path_arc: Arc<str> = Arc::from(path.as_str());
        let frags = fragment_file(path_arc, content);
        for f in frags {
            if seen.insert(f.id.clone()) {
                all_fragments.push(f);
            }
        }
    }

    let effective_budget = budget_tokens.unwrap_or(BUDGET.unlimited);

    let discovered_arc: FxHashSet<Arc<str>> = discovered_paths
        .iter()
        .map(|s| Arc::from(s.as_str()))
        .collect();

    // The corpus harness has no timeout contract; before #210 it inherited
    // whatever ceiling the last in-process run left behind.
    let _diffctx::pipeline::ScoredFragments {
        all_fragments,
        core_ids,
        core_excerpts,
        scoring_result,
        needs,
        changed_lines,
        ..
    } = _diffctx::pipeline::score_from_fragments(
        all_fragments,
        &hunks,
        &diff_text,
        &config,
        tree_root.as_deref(),
        &discovered_arc,
        &run,
    );

    // The same envelope charge the product pipeline applies (#241): a harness
    // that spends the budget differently scores a system nobody runs (#149).
    let mut listed: Vec<String> = changed_paths.iter().cloned().collect();
    listed.sort();
    let selection_budget =
        effective_budget.saturating_sub(_diffctx::pipeline::envelope_token_cost(None, &listed));

    let dummy_root = Path::new(".");
    let mut changed_files: Vec<PathBuf> = changed_paths.iter().map(PathBuf::from).collect();
    changed_files.sort();
    let mut hunk_texts = diffctx_base::changed_lines::hunk_texts(&diff_text);
    for runs in hunk_texts.chunk_by_mut(|a, b| a.path == b.path) {
        let text = all_files.get(runs[0].path.as_str()).map(String::as_str);
        diffctx_facts::change_class::settle_layout(runs, text, &|| run.check());
    }
    let change_classes = _diffctx::pipeline::classify_changes(
        dummy_root,
        &changed_files,
        &hunks,
        &hunk_texts,
        &all_fragments,
    );
    let fragmentless = diffctx_answer::run_provenance::state::fragmentless_changed_files(
        dummy_root,
        &changed_files,
        &all_fragments,
    );

    let _diffctx::pipeline::PostpassOutcome {
        mut selected,
        stand_in_ids,
        ..
    } = _diffctx::pipeline::select_and_postpass(
        &scoring_result,
        &all_fragments,
        &core_ids,
        &needs,
        &core_excerpts,
        config.objective,
        selection_budget,
        tau,
        &_diffctx::pipeline::evidence_priority_of(&changed_files, &change_classes),
        &changed_lines,
    );

    let used: u32 = selected.iter().map(|f| f.token_count).sum();
    let remaining = selection_budget.saturating_sub(used);
    diffctx_select::postpass::ensure_changed_files_represented(
        &mut selected,
        &all_fragments,
        &changed_files,
        remaining,
        Path::new("."),
        &diffctx_snapshot::source::Source::empty(),
        &core_ids,
        &FxHashMap::default(),
        &stand_in_ids,
    );

    let mut changed_list: Vec<String> = changed_paths.iter().cloned().collect();
    changed_list.sort();
    let change = diffctx_answer::render::ChangeSummary {
        range_spec: None,
        index_tree: None,
        lockfile_changes: Vec::new(),
        ignored_changes: Vec::new(),
        policy_excluded_count: 0,
        commit_message: None,
        commit_messages: Vec::new(),
        commit_count: 0,
        changes: change_classes,
        fragmentless,
        changed_files: changed_list,
        deleted_files: Vec::new(),
        renamed_files: Vec::new(),
    };
    build_diff_context_output(
        dummy_root,
        &selected,
        no_content,
        &core_ids,
        &stand_in_ids,
        &scoring_result.rel_scores,
        change,
    )
}

fn compute_memory_hunks(
    initial: &FxHashMap<String, String>,
    changed: &FxHashMap<String, String>,
) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();

    for (path, new_content) in changed {
        let old_content = initial.get(path).map(|s| s.as_str()).unwrap_or("");
        if old_content == new_content {
            continue;
        }
        let path_arc: Arc<str> = Arc::from(path.as_str());
        let file_hunks = diff_to_hunks(&path_arc, old_content, new_content);
        hunks.extend(file_hunks);
    }

    for (path, _old_content) in initial {
        if !changed.contains_key(path) {
            let path_arc: Arc<str> = Arc::from(path.as_str());
            let old_line_count = initial[path].lines().count() as u32;
            if old_line_count > 0 {
                hunks.push(DiffHunk {
                    path: path_arc,
                    new_start: 1,
                    new_len: 0,
                    old_start: 1,
                    old_len: old_line_count,
                });
            }
        }
    }

    hunks
}

pub(crate) fn diff_to_hunks(path: &Arc<str>, old: &str, new: &str) -> Vec<DiffHunk> {
    let diff = TextDiff::from_lines(old, new);
    let mut hunks = Vec::new();

    let mut new_line: u32 = 0;
    let mut old_line: u32 = 0;

    let mut hunk_new_start: Option<u32> = None;
    let mut hunk_new_len: u32 = 0;
    let mut hunk_old_start: u32 = 0;
    let mut hunk_old_len: u32 = 0;

    // git's `--unified=0` reports a pure deletion's new-side start as the
    // line BEFORE the gap (`@@ -5 +4,0 @@`), and `core_selection_range`
    // anchors on that. Starting at `new_line + 1` here put the harness one
    // line below production for every deletion-only hunk, so the corpus
    // scored a selection nobody ships.
    let finish = |start: u32, new_len: u32| {
        if new_len == 0 {
            start.saturating_sub(1)
        } else {
            start
        }
    };
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let Some(start) = hunk_new_start.take() {
                    hunks.push(DiffHunk {
                        path: Arc::clone(path),
                        new_start: finish(start, hunk_new_len),
                        new_len: hunk_new_len,
                        old_start: hunk_old_start,
                        old_len: hunk_old_len,
                    });
                    hunk_new_len = 0;
                    hunk_old_len = 0;
                }
                new_line += 1;
                old_line += 1;
            }
            ChangeTag::Delete => {
                if hunk_new_start.is_none() {
                    hunk_new_start = Some(new_line + 1);
                    hunk_old_start = old_line + 1;
                }
                hunk_old_len += 1;
                old_line += 1;
            }
            ChangeTag::Insert => {
                if hunk_new_start.is_none() {
                    hunk_new_start = Some(new_line + 1);
                    hunk_old_start = old_line + 1;
                }
                hunk_new_len += 1;
                new_line += 1;
            }
        }
    }

    if let Some(start) = hunk_new_start {
        hunks.push(DiffHunk {
            path: Arc::clone(path),
            new_start: finish(start, hunk_new_len),
            new_len: hunk_new_len,
            old_start: hunk_old_start,
            old_len: hunk_old_len,
        });
    }

    hunks
}

fn compute_memory_diff_text(
    initial: &FxHashMap<String, String>,
    changed: &FxHashMap<String, String>,
) -> String {
    let mut result = String::new();

    let mut paths: Vec<&String> = changed.keys().collect();
    paths.sort();

    for path in paths {
        let new_content = &changed[path];
        let old_content = initial.get(path).map(|s| s.as_str()).unwrap_or("");
        if old_content == new_content {
            continue;
        }

        let diff = TextDiff::from_lines(old_content, new_content);
        let mut udiff = diff.unified_diff();
        let formatted = udiff
            .context_radius(TOKENIZATION.diff_context_radius)
            .header(&format!("a/{path}"), &format!("b/{path}"));
        let _ = write!(result, "{formatted}");
    }

    let mut deleted_paths: Vec<&String> = initial
        .keys()
        .filter(|p| !changed.contains_key(*p))
        .collect();
    deleted_paths.sort();

    for path in deleted_paths {
        let old_content = &initial[path];
        let empty = String::new();
        let diff = TextDiff::from_lines(old_content, &empty);
        let mut udiff = diff.unified_diff();
        let formatted = udiff
            .context_radius(TOKENIZATION.diff_context_radius)
            .header(&format!("a/{path}"), "/dev/null");
        let _ = write!(result, "{formatted}");
    }

    result
}

fn merge_file_contents(
    initial: &FxHashMap<String, String>,
    changed: &FxHashMap<String, String>,
) -> FxHashMap<String, String> {
    let mut merged = initial.clone();
    for (path, content) in changed {
        merged.insert(path.clone(), content.clone());
    }
    merged
}
