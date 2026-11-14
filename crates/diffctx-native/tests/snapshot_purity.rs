//! Snapshot purity: a context on a tree range reads the tree and nothing
//! else. The working tree is mutated and junk added between two builds; the
//! identity, the listing, every read and the change set must not move.

mod snapshot_fixture;

use _diffctx::snapshot::{
    AnalysisContext, Decision, RangeSpec, RepoPath, RunBudget, Side, WithholdReason, resolve,
};
use snapshot_fixture::{Repo, names};

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).expect("a path")
}

fn seeded() -> (Repo, String) {
    let repo = Repo::init();
    repo.write(".gitignore", "*.log\nbuild/\n");
    repo.write("src/a.py", "def a():\n    return 1\n");
    repo.write("src/b.py", "from a import a\n");
    repo.write("docs/readme.md", "# readme\n");
    repo.write("trace.log", "tracked but matched by a rule\n");
    repo.write("build/out.py", "tracked under an ignored directory\n");
    repo.git(&["add", "-f", "--", "trace.log", "build/out.py"]);
    repo.commit("base");
    repo.write("src/a.py", "def a():\n    return 2\n");
    repo.write("src/new.py", "NEW = 1\n");
    repo.commit("head");
    let range = format!("{}..{}", repo.rev("HEAD~1"), repo.head());
    (repo, range)
}

fn mutate_working_tree(repo: &Repo) {
    repo.write("src/a.py", "def a():\n    return 'edited on disk'\n");
    repo.write("src/junk.py", "JUNK = 1\n");
    repo.write("untracked.txt", "junk\n");
    repo.write("junk.log", "junk\n");
    repo.write(".gitignore", "*\n");
    repo.remove("docs/readme.md");
}

#[test]
fn a_review_context_is_the_same_before_and_after_the_working_tree_moves() {
    let (repo, range) = seeded();
    let range = resolve(&repo.root, &range).expect("resolve");

    let first = AnalysisContext::new(repo.root.clone(), range.clone(), RunBudget::default())
        .expect("first context");
    let first_listing = first.head.list().expect("list");
    let first_reads: Vec<_> = first_listing
        .entries
        .iter()
        .map(|e| first.head.read(&e.path).expect("read"))
        .collect();

    mutate_working_tree(&repo);

    let second = AnalysisContext::new(repo.root, range, RunBudget::default()).expect("second");
    assert_eq!(first.identity(), second.identity());
    assert_eq!(first_listing, second.head.list().expect("list"));
    assert_eq!(first.changes, second.changes);
    let second_reads: Vec<_> = first_listing
        .entries
        .iter()
        .map(|e| second.head.read(&e.path).expect("read"))
        .collect();
    assert_eq!(first_reads, second_reads);

    let a = second
        .head
        .read(&path("src/a.py"))
        .expect("read")
        .expect("present");
    assert_eq!(&*a, b"def a():\n    return 2\n");
    let before = second
        .base
        .read(&path("src/a.py"))
        .expect("read")
        .expect("present");
    assert_eq!(&*before, b"def a():\n    return 1\n");
}

#[test]
fn a_tree_side_never_falls_back_to_the_disk() {
    let (repo, range) = seeded();
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        resolve(&repo.root, &range).expect("resolve"),
        RunBudget::default(),
    )
    .expect("context");

    repo.write("only_on_disk.py", "X = 1\n");
    assert_eq!(ctx.head.read(&path("only_on_disk.py")).expect("read"), None);

    repo.remove("src/b.py");
    let kept = ctx
        .head
        .read(&path("src/b.py"))
        .expect("read")
        .expect("present");
    assert_eq!(&*kept, b"from a import a\n");
}

#[test]
fn ignore_rules_are_those_of_the_tree_not_of_the_checkout() {
    let (repo, range) = seeded();
    // The checkout ignores everything before the context is even built; the
    // tree's own .gitignore still withholds the tracked log it matches
    // directly, and still admits the tracked file that is under an ignored
    // directory only transitively.
    repo.write(".gitignore", "*\n");
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        resolve(&repo.root, &range).expect("resolve"),
        RunBudget::default(),
    )
    .expect("context");
    let listing = ctx.head.list().expect("list");
    let admitted = names(&listing);
    assert!(admitted.contains(&"src/a.py".to_string()));
    assert!(
        admitted.contains(&"build/out.py".to_string()),
        "{admitted:?}"
    );
    assert!(!admitted.contains(&"trace.log".to_string()));
    assert_eq!(listing.withheld, 1);
    assert_eq!(
        ctx.head.decision(&path("trace.log")).expect("decision"),
        Some(Decision::Withhold(WithholdReason::Gitignore))
    );
}

#[test]
fn one_snapshot_context_has_no_change_and_one_identity() {
    let (repo, _) = seeded();
    let side = Side::Tree(repo.head());
    let at = || {
        AnalysisContext::new(
            repo.root.clone(),
            RangeSpec::At(side.clone()),
            RunBudget::default(),
        )
        .expect("context")
    };
    let first = at();
    assert_eq!(first.changes.changes.len(), 0);
    assert_eq!(first.base.id(), first.head.id());
    mutate_working_tree(&repo);
    assert_eq!(first.identity(), at().identity());
}

#[test]
fn a_staged_context_does_not_follow_the_working_tree() {
    let (repo, _) = seeded();
    repo.write("src/staged.py", "STAGED = 1\n");
    repo.git(&["add", "src/staged.py"]);
    let range = _diffctx::snapshot::staged(&repo.root).expect("range");
    let first =
        AnalysisContext::new(repo.root.clone(), range, RunBudget::default()).expect("context");
    repo.write("src/staged.py", "STAGED = 'changed after capture'\n");
    repo.write("src/late.py", "LATE = 1\n");
    let read = first
        .head
        .read(&path("src/staged.py"))
        .expect("read")
        .expect("present");
    assert_eq!(&*read, b"STAGED = 1\n");
    assert_eq!(first.head.read(&path("src/late.py")).expect("read"), None);
    assert_eq!(
        first
            .changes
            .changes
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["src/staged.py"]
    );
}
