//! What a run costs in git processes. Each answer starts git children (reads of
//! the repository, one persistent object reader per snapshot), and a later
//! change that asks the same question again, or one more per file, is paid in
//! processes before it is paid in anything a profile shows. The count is read
//! where a git child starts (`git_spawns`), never timed, and a repository of
//! three files and one of twenty must cost the same: a spawn per item shows as
//! a difference, whatever the ceiling.
//!
//! Needles searched together share the search: one `git grep` for every consumer
//! that planned, and each gets the rows its own needles select.
//!
//! One test: the counter is the process's, and a second test would count into it.

use std::path::Path;

use _diffctx::pipeline::{
    build_diff_context_impact, build_diff_context_in, build_diff_context_locate,
    build_symbol_impact,
};
use diffctx_config::config::limits::DEFAULT_PPR_ALPHA;
use diffctx_config::mode::ScoringMode;
use diffctx_snapshot::resource::RunContext;
use diffctx_snapshot::snapshot::{
    Admission, Oid, RepoPath, Side, SnapshotReader, git_spawns, needle_lines, open, rev_oid,
};
use tempfile::TempDir;

mod support;
use support::seeds::{Edit, Knobs, Lang, RANGE, seed};

/// A `--timeout` no run reaches.
const TIMEOUT_SECS: u64 = 315_360_000;

type Cell = (&'static str, usize, fn(&Path));

const CELLS: [Cell; 4] = [
    ("impact", IMPACT_CEILING, |repo| {
        build_diff_context_impact(
            repo,
            Some(RANGE),
            &[],
            DEFAULT_PPR_ALPHA,
            ScoringMode::Ppr,
            TIMEOUT_SECS,
        )
        .expect("impact");
    }),
    ("pack", PACK_CEILING, |repo| {
        build_diff_context_in(
            repo,
            Some(RANGE),
            &[],
            Some(8000),
            DEFAULT_PPR_ALPHA,
            None,
            false,
            false,
            ScoringMode::Ego,
            TIMEOUT_SECS,
        )
        .expect("pack");
    }),
    ("locate", LOCATE_CEILING, |repo| {
        build_diff_context_locate(
            repo,
            Some(RANGE),
            &[],
            Some(8000),
            DEFAULT_PPR_ALPHA,
            None,
            ScoringMode::Ppr,
            TIMEOUT_SECS,
            false,
        )
        .expect("locate");
    }),
    ("symbol", SYMBOL_CEILING, |repo| {
        build_symbol_impact(
            repo,
            "total",
            &[],
            DEFAULT_PPR_ALPHA,
            ScoringMode::Ppr,
            TIMEOUT_SECS,
        )
        .expect("symbol");
    }),
];

const IMPACT_CEILING: usize = 26;
const PACK_CEILING: usize = 15;
const LOCATE_CEILING: usize = 15;
const SYMBOL_CEILING: usize = 12;

fn repository(extra_files: usize) -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let knobs = Knobs {
        extra_files,
        ..Knobs::default()
    };
    seed(Lang::Python, Edit::Signature, &knobs).commit_into(dir.path());
    dir
}

/// The second run of a cell: the first fills the caches on disk, and a run that
/// reads them starts the processes of every later one.
fn processes(repo: &Path, run: fn(&Path)) -> usize {
    run(repo);
    let before = git_spawns();
    run(repo);
    git_spawns() - before
}

fn needles_searched_together_are_one_search(repo: &Path) {
    let (total, charge) = (vec!["total".to_string()], vec!["charge".to_string()]);
    let more = || true;
    let alone = (
        needle_lines(repo, &total, None, &more).expect("search"),
        needle_lines(repo, &charge, None, &more).expect("search"),
    );
    assert!(
        !alone.0.is_empty() && !alone.1.is_empty(),
        "the seed spells both"
    );

    let run = RunContext::unbounded();
    let _in_run = run.enter();
    let before = git_spawns();
    let together_needles: Vec<String> = total.iter().chain(&charge).cloned().collect();
    needle_lines(repo, &together_needles, None, &more).expect("search");
    let together = (
        needle_lines(repo, &total, None, &more).expect("search"),
        needle_lines(repo, &charge, None, &more).expect("search"),
    );
    assert_eq!(
        git_spawns() - before,
        1,
        "two needles searched together, one search"
    );
    assert_eq!(together, alone, "each reads the rows of its own needles");
    needle_lines(repo, &total, None, &more).expect("search");
    assert_eq!(
        git_spawns() - before,
        1,
        "a needle searched is not searched again"
    );
}

fn readers_of_a_run_share_one_object_reader(repo: &Path) {
    let run = RunContext::unbounded();
    let _in_run = run.enter();
    let root = open(repo).expect("open");
    let commit = rev_oid(repo, "HEAD").expect("rev").expect("a commit");
    let side = || Side::Tree(Oid::parse(&commit).expect("oid"));
    let admission = std::sync::Arc::new(Admission::default());
    let first = SnapshotReader::new(&root, side(), admission.clone()).expect("reader");
    let second = SnapshotReader::new(&root, side(), admission).expect("reader");
    let path = RepoPath::new("shop/pricing.py").expect("path");
    assert!(
        first.read(&path).expect("read").is_some(),
        "the seed has it"
    );
    let before = git_spawns();
    assert!(
        second.read(&path).expect("read").is_some(),
        "the seed has it"
    );
    assert_eq!(
        git_spawns() - before,
        0,
        "a second reader of the run asks the first one's object reader"
    );
}

fn a_cut_search_gives_no_row_of_other_needles(repo: &Path) {
    let covered = vec!["total".to_string(), "charge".to_string()];
    let asked = vec!["return sum".to_string(), "sum(items".to_string()];
    let run = RunContext::unbounded();
    let _in_run = run.enter();
    let kept = needle_lines(repo, &covered, None, &|| true).expect("search");
    assert!(
        kept.iter()
            .any(|(_, _, text)| !text.contains("return sum") && !text.contains("sum(items")),
        "the seed has covered rows that the asked needles do not select"
    );
    let polls = std::cell::Cell::new(0);
    let one_poll = || {
        polls.set(polls.get() + 1);
        polls.get() == 1
    };
    let cut = needle_lines(repo, &asked, None, &one_poll).expect("search");
    assert!(!cut.is_empty(), "the cut search found its rows");
    assert!(
        cut.iter()
            .all(|(_, _, text)| asked.iter().any(|n| text.contains(n.as_str()))),
        "a cut search returned a row of the needles covered before it: {cut:?}"
    );
}

#[test]
fn a_run_starts_no_more_git_processes_than_its_cell_allows_whatever_the_repository_size() {
    let small = repository(0);
    let large = repository(12);
    let mut report = Vec::new();
    let mut failures = Vec::new();
    for (name, ceiling, run) in CELLS {
        let (few, many) = (processes(small.path(), run), processes(large.path(), run));
        report.push(format!(
            "{name}: {few} git processes on the small repository, {many} on the large (ceiling {ceiling})"
        ));
        if few != many {
            failures.push(format!("{name}: the count grew with the repository"));
        }
        if many > ceiling {
            failures.push(format!("{name}: over its ceiling"));
        }
    }
    needles_searched_together_are_one_search(small.path());
    a_cut_search_gives_no_row_of_other_needles(small.path());
    readers_of_a_run_share_one_object_reader(small.path());
    let report = report.join("\n");
    assert!(failures.is_empty(), "{}\n{report}", failures.join("; "));
    eprintln!("{report}");
}
