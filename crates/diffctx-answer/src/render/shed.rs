//! What goes next when a pack document is over its budget: one fragment fewer, or a changed one shortened,
//! the lowest evidence tier first. The document's own trait impl lives here because the trait is the base's and
//! the document is this crate's.

use std::path::Path;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::render::{DiffContextOutput, FragmentEntry};
use diffctx_facts::change_class::ChangeClass;
use diffctx_snapshot::resource::{CoverageReport, CoverageStatus, LimitReason, ResourceUsage};

/// A pack document, shed a fragment at a time.
impl diffctx_base::fit::Shed for DiffContextOutput {
    fn shed_one(&mut self) -> bool {
        drop_one_fragment(self)
    }
}

/// One fragment fewer, or a changed one shortened; `false` once nothing is
/// left to drop.
fn drop_one_fragment(pack: &mut DiffContextOutput) -> bool {
    let classes: FxHashMap<&str, ChangeClass> = pack
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.class))
        .collect();
    let Some(index) = drop_index(&pack.fragments, &classes) else {
        return false;
    };
    // A changed fragment is shortened from its tail before it goes: a larger
    // budget must never show less of the change. Dropped whole, the rewrite
    // of `plan` vanished at the budget where a smaller one showed its
    // signature, while the one-line bumps above it stayed (#438). Context
    // has no such floor and drops whole.
    let changed = pack.fragments[index].role.as_deref() == Some("changed");
    if !(changed && halve_witness(&mut pack.fragments[index])) {
        pack.fragments.remove(index);
    }
    let represented: FxHashSet<&str> = pack.fragments.iter().map(|f| f.path.as_str()).collect();
    let mut unrepresented = false;
    for change in &mut pack.changes {
        let shown = represented.contains(change.path.as_str());
        unrepresented |= !shown && !change.no_fragments;
        change.represented = shown;
    }
    record_cut(pack, unrepresented);
    true
}

fn record_cut(pack: &mut DiffContextOutput, unrepresented: bool) {
    let coverage = pack.coverage.get_or_insert_with(|| CoverageReport {
        status: CoverageStatus::Partial,
        limits: Vec::new(),
        capped: Vec::new(),
        limited_files: Vec::new(),
        resources: ResourceUsage::default(),
        unknown: vec!["resources".to_string()],
        lossy_files: Vec::new(),
    });
    let cut = LimitReason::SelectionBudgetExceeded;
    if !coverage.limits.contains(&cut) {
        coverage.limits.push(cut);
    }
    if unrepresented {
        coverage.status = CoverageStatus::Degraded;
    } else if coverage.status == CoverageStatus::Complete {
        coverage.status = CoverageStatus::Partial;
    }
}

const CLIPPED_MARKER_PREFIX: &str = "… [";

const CLIPPED_MARKER_SUFFIX: &str = " more lines of this change]";

fn halve_witness(fragment: &mut FragmentEntry) -> bool {
    let Some(content) = fragment.content.as_deref() else {
        return false;
    };
    let mut lines: Vec<&str> = content.lines().collect();
    let mut hidden = 0usize;
    if let Some(n) = lines
        .last()
        .and_then(|l| l.strip_prefix(CLIPPED_MARKER_PREFIX))
        .and_then(|l| l.strip_suffix(CLIPPED_MARKER_SUFFIX))
        .and_then(|n| n.parse::<usize>().ok())
    {
        hidden = n;
        lines.pop();
    }
    if lines.len() < 2 {
        return false;
    }
    let keep = (lines.len() * 3 / 4).clamp(1, lines.len() - 1);
    hidden += lines.len() - keep;
    let clipped = format!(
        "{}\n{CLIPPED_MARKER_PREFIX}{hidden}{CLIPPED_MARKER_SUFFIX}\n",
        lines[..keep].join("\n")
    );
    let start: usize = fragment
        .lines
        .split('-')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    fragment.lines = format!("{start}-{}", start + keep - 1);
    fragment.content = Some(Arc::from(clipped.as_str()));
    true
}

/// What goes next when the document is over its budget: context from the
/// tail; then a changed file's second fragment, before any file loses its
/// last; then those last witnesses. Changed fragments go lowest evidence
/// tier first — generated files, mechanical bumps, config and docs, tests,
/// production last — the reverse of the order the selection placed them
/// in, and from the tail within a tier. By position alone the production
/// file went first because `zsrc/` sorts after `README.md` and `tests/`
/// (#411, #438).
fn drop_index(
    fragments: &[FragmentEntry],
    classes: &FxHashMap<&str, ChangeClass>,
) -> Option<usize> {
    if let Some(i) = fragments
        .iter()
        .rposition(|f| f.role.as_deref() != Some("changed"))
    {
        return Some(i);
    }
    let tier = |f: &FragmentEntry| {
        let class = classes
            .get(f.path.as_str())
            .copied()
            .unwrap_or(ChangeClass::Content);
        evidence_tier(&f.path, class)
    };
    let mut per_file: FxHashMap<&str, usize> = FxHashMap::default();
    for f in fragments {
        *per_file.entry(f.path.as_str()).or_default() += 1;
    }
    let lowest_from_the_tail = |second: bool| {
        (0..fragments.len())
            .filter(|&i| !second || per_file[fragments[i].path.as_str()] > 1)
            .max_by_key(|&i| (tier(&fragments[i]), i))
    };
    lowest_from_the_tail(true).or_else(|| lowest_from_the_tail(false))
}

/// The order changed files earn budget in when not all of them fit, and the
/// renderer cuts them in reverse: production code (0), then its tests,
/// then config and docs, then mechanical or layout-only edits, then
/// generated files (4). A test stub printed whole while the migration it
/// exercises was omitted is the inversion (#311). Read on the repository
/// path: an absolute one under a `tests/` directory made every file a test.
pub fn evidence_tier(repo_path: &str, class: diffctx_facts::change_class::ChangeClass) -> u8 {
    use diffctx_facts::change_class::ChangeClass;
    let path = Path::new(repo_path);
    let ext = diffctx_base::languages::file_ext(path);
    match class {
        ChangeClass::Generated => 4,
        ChangeClass::Mechanical | ChangeClass::Layout => 3,
        _ if diffctx_base::testfiles::is_test_path(path) => 1,
        _ if diffctx_config::config::extensions::CONFIG_EXTENSIONS.contains(ext.as_str())
            || matches!(ext.as_str(), ".md" | ".rst" | ".txt" | ".adoc") =>
        {
            2
        }
        _ => 0,
    }
}
