//! What the range says as a whole: contracts that moved, stale references, the files that name a changed
//! one, the files nothing traced, the limits the run met; and the answer assembled from them.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::{self};

use super::super::mentions::{named_by_path, stale_references, twin_references};

use super::super::IMPACT_TOKEN_CAP;
use super::super::MAX_CONTRACTS;
use super::super::callers::is_code_file;
use super::super::changes::{changed_rels, is_schema_file, layout_files, rel, unread_namers};
use super::super::deliver::proves_empty;
use super::super::key_search::{key_references, read_by_collection};
use super::super::output::{Contract, ImpactOutput, UnanalysedFile};
use super::super::removed::dead_modules;
use super::super::symbol::directory_name;
use super::super::unanalysed::{
    MAX_OUTSIDE_RUNS, OUTSIDE_DEFINITIONS, nameless_changes, outside_definitions, unanalysed_files,
};

use super::context::{Findings, Impact};
use super::targets::Targets;

pub(super) fn findings(cx: &Impact<'_>, t: &Targets<'_>, found: Findings) -> ImpactOutput {
    let (state, query, cells, tests, analysed) =
        (cx.state, cx.query, cx.cells, &cx.tests, cx.analysed);
    let (cores, held, untraced, diff_range) = (&t.cores, &t.held, &t.untraced, cx.diff_range);
    let Findings {
        mut changed,
        mut contracts,
        mut public_api,
        untested_definitions,
        untested_unlisted,
        untested_called_in_diff,
        entry_unlisted,
        layout_only,
        traced_definitions,
        traced_names,
    } = found;
    // What the cap costs the reader: callers outside the diff, fewer changed users, commit overlap.
    changed.sort_by(|a, b| {
        b.callers
            .len()
            .cmp(&a.callers.len())
            .then_with(|| a.also_changed.len().cmp(&b.also_changed.len()))
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| a.path.cmp(&b.path))
            .then_with(|| a.lines.cmp(&b.lines))
    });

    let changed_files: Vec<String> = changed_rels(state).collect();
    let layout_files = layout_files(state);
    // A queried symbol changes nothing: the file holding its definition is
    // no contract that moved.
    for path in changed_files.iter().filter(|_| query.is_none()) {
        if is_schema_file(path) && !layout_files.contains(path.as_str()) {
            contracts.push(Contract {
                path: path.clone(),
                symbol: None,
                kind: "schema",
            });
        }
    }

    public_api.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)));
    contracts.extend(public_api.into_iter().map(|(_, c)| c));
    let contracts_omitted = contracts.len().saturating_sub(MAX_CONTRACTS);
    contracts.truncate(MAX_CONTRACTS);
    // Pack's rule, not a list of our own: a run-wide stop, or a per-file
    // limit on a changed file. The graph's per-node cap is routine and stays
    // out; the callers it cut an edge from are recovered per definition.
    let changed_set: FxHashSet<&str> = changed_files.iter().map(String::as_str).collect();
    let untracked: FxHashSet<String> = state
        .untracked_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .collect();
    let (stale, stale_omitted) = if query.is_none() {
        let _stale = diffctx_base::profile::phase("impact.stale_paths");
        stale_references(state, cells, analysed)
    } else {
        (Vec::new(), 0)
    };
    let (deleted_modules, changed) = if query.is_none() {
        dead_modules(state, cells, changed, &stale)
    } else {
        (Vec::new(), changed)
    };
    let (mut named_by, named_by_path_omitted) = {
        let _named = diffctx_base::profile::phase("impact.named_by");
        let (mut named, omitted) = named_by_path(state, cells, analysed);
        if query.is_none() {
            let by_module = untraced.module_refs(state, cores, &named);
            named.extend(by_module);
            named.extend(twin_references(state, cores));
        }
        (named, omitted)
    };
    let (collection_reads, collection_reads_omitted) = if query.is_none() {
        read_by_collection(state, analysed)
    } else {
        (Vec::new(), 0)
    };
    let nameless = if query.is_none() {
        nameless_changes(state, cells, held)
    } else {
        Vec::new()
    };
    let (mut unanalysed, cell_read_configs, consumed_outside, lost_configs) = if query.is_none() {
        // A file the run did not analyse whole has its own reason, which already
        // says its effect is unknown: it is never listed twice. The rest come
        // first, so that the cap on the list keeps them.
        let mut others = unanalysed_files(state, &changed_files);
        // A base side over the size cap is never parsed: its removals go unread,
        // and the file is named at the path the answer reports (the head's, for
        // a rename), whether it changed, was renamed or was deleted.
        let old_path_of: FxHashMap<&str, &str> = state
            .renamed_files
            .iter()
            .map(|(old, new)| (new.as_str(), old.as_str()))
            .collect();
        let sides = changed_files
            .iter()
            .map(|p| {
                (
                    old_path_of.get(p.as_str()).copied().unwrap_or(p.as_str()),
                    p.as_str(),
                )
            })
            .chain(state.deleted_files.iter().map(|p| (p.as_str(), p.as_str())));
        for (old, new) in sides {
            let over = |path: &str| {
                cells.reading(path).and_then(|cell| cell.loss_about(path))
                    == Some(decide::gap::Reason::TooLarge)
            };
            if (over(old) || over(new)) && !others.iter().any(|f| f.path == new) {
                others.push(UnanalysedFile::new(
                    new.to_string(),
                    "too_large",
                    Vec::new(),
                ));
            }
        }
        // A changed file a cell read whole is traced: what it holds outside every
        // definition is `outside_definitions`, and nothing else of it is untraced.
        // What the code reads of its keys is no cell's answer yet: the key search
        // still runs for it, as it did before the cell read it.
        let cell_read: Vec<String> = others
            .iter()
            .filter(|f| {
                f.reason == "content_untraced"
                    && cells.of(&f.path).is_some_and(|cell| cell.is_bound(&f.path))
            })
            .map(|f| f.path.clone())
            .collect();
        // The cell traces what is inside the files; what builds or applies them
        // lies outside, and a mention or a reader in another file is all that
        // reaches it. Such a file is a gap again unless one is found below.
        let consumed_outside: Vec<UnanalysedFile> = others
            .iter()
            .filter(|f| {
                cell_read.contains(&f.path)
                    && cells
                        .of(&f.path)
                        .is_some_and(decide::cell::Cell::consumed_outside)
            })
            .cloned()
            .collect();
        others.retain(|f| !cell_read.contains(&f.path));
        // A file its cell lost, in part or whole, is not made traced by the key
        // search that still runs for it: its loss is what the answer says of it.
        let mut lost_configs: Vec<String> = Vec::new();
        for f in &mut others {
            let lost = (f.reason == "content_untraced")
                .then(|| {
                    cells
                        .reading(&f.path)
                        .and_then(|cell| cell.loss_about(&f.path))
                })
                .flatten();
            if let Some(reason) = lost {
                f.reason = reason.row_reason();
                lost_configs.push(f.path.clone());
            }
        }
        let mut all: Vec<UnanalysedFile> = outside_definitions(cells)
            .into_iter()
            .filter(|o| !others.iter().any(|f| f.path == o.path))
            .collect();
        all.extend(others);
        for n in &nameless {
            if !all.iter().any(|f| f.path == n.path) {
                all.push(UnanalysedFile {
                    language: diffctx_base::languages::get_language_for_file(&n.path),
                    path: n.path.clone(),
                    reason: OUTSIDE_DEFINITIONS,
                    lines: n
                        .runs
                        .iter()
                        .take(MAX_OUTSIDE_RUNS)
                        .flat_map(|&(first, last)| [first, last])
                        .collect(),
                });
            }
        }
        (all, cell_read, consumed_outside, lost_configs)
    } else {
        (Vec::new(), Vec::new(), Vec::new(), Vec::new())
    };
    let (key_searches, key_files_unsearched, named_by_key_omitted) = {
        let untraced: Vec<String> = unanalysed
            .iter()
            .filter(|f| f.reason == "content_untraced")
            .map(|f| f.path.clone())
            .chain(cell_read_configs)
            .chain(lost_configs.iter().cloned())
            .chain(nameless.iter().map(|n| n.path.clone()))
            .chain(changed_files.iter().filter(|p| is_schema_file(p)).cloned())
            .collect();
        let (refs, omitted, searches, unsearched, unread) =
            key_references(state, cells, &untraced, &nameless);
        unanalysed.extend(unread);
        // A search of the names a nameless change touches is no trace of it.
        unanalysed.retain(|f| {
            lost_configs.contains(&f.path)
                || nameless.iter().any(|n| n.path == f.path)
                || !searches.iter().any(|k| k.path == f.path)
        });
        named_by.extend(refs);
        (searches, unsearched, omitted)
    };
    // A config or a Dockerfile nothing names stays a gap: its builds are not
    // traced, and a clean zero would claim they are.
    for file in consumed_outside {
        let read = named_by.iter().any(|r| r.target == file.path)
            || changed
                .iter()
                .any(|c| c.path == file.path && c.callers.iter().any(|k| k.path != file.path));
        if !read {
            unanalysed.push(file);
        }
    }
    // Read after the last phase that can stop: a deadline the stale-reference
    // search hit was missing from an answer that had dropped its lines. A
    // changed file outside code holds no definition and calls nothing: one
    // over the cap is named in `unanalysed`, and no caller is missing for it
    // (#626).
    let (mut limits, _, _) = diffctx_snapshot::resource::classify_limits(&state.run, &[], &|p| {
        changed_set.contains(p) && (is_code_file(p) || Path::new(p).extension().is_none())
    });
    // A changed file cut to the fragment cap keeps every fragment the diff
    // touched: pack loses context there, impact loses no changed definition,
    // and the cap fires on most large files (#306).
    limits.retain(|r| *r != diffctx_snapshot::resource::LimitReason::FragmentLimit);
    // A symbol lookup reads the files naming the symbol itself.
    let outside_scope_files = if query.is_none() {
        unread_namers(state, analysed, traced_names)
    } else {
        0
    };
    let mut output = ImpactOutput {
        name: directory_name(&state.root_dir),
        range: diff_range.map(str::to_string),
        index_tree: state.index_tree.clone(),
        symbol: query.map(str::to_string),
        untracked_files: changed_files
            .iter()
            .filter(|f| untracked.contains(f.as_str()))
            .cloned()
            .collect(),
        changed_files: changed_files
            .into_iter()
            .filter(|f| !untracked.contains(f.as_str()))
            .collect(),
        deleted_files: state.deleted_files.clone(),
        stale_references: stale,
        stale_references_omitted: stale_omitted,
        deleted_modules,
        named_by,
        named_by_path_omitted,
        named_by_key_omitted,
        key_searches,
        key_files_unsearched,
        collection_reads,
        collection_reads_omitted,
        commit_count: state.commit_count,
        changed,
        contracts,
        contracts_omitted,
        limits,
        tests_known: tests.any(),
        untested_definitions,
        untested_unlisted,
        untested_called_in_diff,
        entry_unlisted,
        layout_only,
        lockfile_changes: state.lockfile_changes.clone(),
        traced_definitions,
        withheld_files: state.policy_excluded_count + state.run.withheld_count(),
        outside_scope_files,
        unread_directories: state.run.source().unread_directories(),
        unanalysed_files: unanalysed,
        run: Some(state.run.clone()),
        cap: IMPACT_TOKEN_CAP,
        ..ImpactOutput::default()
    };
    output.empty = proves_empty(&output);
    output
}
