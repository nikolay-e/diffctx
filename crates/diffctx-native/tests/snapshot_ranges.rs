//! Ranges are resolved once, to object ids: a base that is not an ancestor of
//! the head resolves to `merge-base..head`, a push and a force-push are tree
//! deltas, one snapshot is `At`.

mod snapshot_fixture;

use std::fmt::Write as _;

use _diffctx::snapshot::{
    AnalysisContext, ChangeKind, ChangeSet, Decision, RangeSpec, RunBudget, Side, SnapshotError,
    WithholdReason, commits, resolve,
};
use snapshot_fixture::Repo;

fn changed(set: &ChangeSet) -> Vec<(ChangeKind, String)> {
    set.changes
        .iter()
        .map(|c| (c.kind, c.path.as_str().to_string()))
        .collect()
}

fn context(repo: &Repo, range: RangeSpec) -> AnalysisContext {
    AnalysisContext::new(repo.root.clone(), range, RunBudget::default()).expect("context")
}

/// main: base - m1 (adds main.txt); topic: base - t1 (adds topic.txt).
fn forked() -> Repo {
    let repo = Repo::init();
    repo.write("shared.txt", "shared\n");
    repo.commit("base");
    repo.git(&["switch", "-q", "-c", "topic"]);
    repo.write("topic.txt", "topic\n");
    repo.commit("t1");
    repo.git(&["switch", "-q", "main"]);
    repo.write("main.txt", "main\n");
    repo.commit("m1");
    repo
}

#[test]
fn a_base_that_is_not_an_ancestor_resolves_to_the_fork_point() {
    let repo = forked();
    let fork = repo.rev("main~1");
    let topic = repo.rev("topic");

    for spec in ["main..topic", "main...topic"] {
        let range = resolve(&repo.root, spec).expect("resolve");
        assert_eq!(
            range,
            RangeSpec::Review {
                base: fork.clone(),
                head: topic.clone()
            },
            "{spec}"
        );
    }
    let ctx = context(&repo, resolve(&repo.root, "main..topic").expect("resolve"));
    // Not "topic.txt added and main.txt deleted": what topic changed since it
    // forked, as a forge's "files changed" shows it.
    assert_eq!(
        changed(&ctx.changes),
        [(ChangeKind::Added, "topic.txt".to_string())]
    );
    assert_eq!(
        commits(&repo.root, &ctx.range).expect("commits"),
        std::slice::from_ref(&topic)
    );
}

#[test]
fn an_ancestor_base_stays_as_written_and_an_open_head_is_head() {
    let repo = forked();
    let fork = repo.rev("main~1");
    assert_eq!(
        resolve(&repo.root, &format!("{fork}..main")).expect("resolve"),
        RangeSpec::Review {
            base: fork.clone(),
            head: repo.rev("main")
        }
    );
    assert_eq!(
        resolve(&repo.root, &format!("{fork}..")).expect("resolve"),
        RangeSpec::Review {
            base: fork,
            head: repo.head()
        }
    );
}

#[test]
fn unknown_and_malformed_ranges_are_usage_errors() {
    let repo = forked();
    for spec in ["nope..main", "--ext-diff", "..main", "1.5h", "a b"] {
        let err = resolve(&repo.root, spec).expect_err(spec);
        assert!(err.is_usage(), "{spec}: {err}");
    }
}

#[test]
fn a_push_delta_runs_from_an_ancestor_and_a_rewritten_ref_is_a_tree_delta() {
    let repo = forked();
    let (base, m1, t1) = (repo.rev("main~1"), repo.rev("main"), repo.rev("topic"));

    let pushed = context(
        &repo,
        RangeSpec::Pushed {
            old: base,
            new: m1.clone(),
        },
    );
    assert_eq!(
        changed(&pushed.changes),
        [(ChangeKind::Added, "main.txt".to_string())]
    );
    assert_eq!(
        commits(&repo.root, &pushed.range).expect("commits"),
        std::slice::from_ref(&m1)
    );

    // main moved from t1's line to m1's line: not a fast-forward.
    let refused = AnalysisContext::new(
        repo.root.clone(),
        RangeSpec::Pushed {
            old: t1.clone(),
            new: m1.clone(),
        },
        RunBudget::default(),
    );
    assert!(matches!(refused, Err(SnapshotError::Range(_))));

    let rewritten = context(&repo, RangeSpec::Rewritten { old: t1, new: m1 });
    assert_eq!(
        changed(&rewritten.changes),
        [
            (ChangeKind::Added, "main.txt".to_string()),
            (ChangeKind::Deleted, "topic.txt".to_string()),
        ]
    );
}

#[test]
fn one_snapshot_has_no_delta() {
    let repo = forked();
    let ctx = context(&repo, RangeSpec::At(Side::Tree(repo.head())));
    assert_eq!(ctx.changes.changes.len(), 0);
    assert_eq!(ctx.changes.withheld, 0);
    assert_eq!(commits(&repo.root, &ctx.range).expect("commits").len(), 0);
}

#[test]
fn staged_and_working_tree_ranges_are_deltas_against_head() {
    let repo = forked();
    repo.write("staged.txt", "staged\n");
    repo.git(&["add", "staged.txt"]);
    repo.write("shared.txt", "edited, not staged\n");
    repo.write("fresh.txt", "untracked\n");
    repo.write("leak.pem", "untracked secret\n");

    let staged = context(
        &repo,
        _diffctx::snapshot::staged(&repo.root).expect("staged"),
    );
    assert_eq!(
        changed(&staged.changes),
        [(ChangeKind::Added, "staged.txt".to_string())]
    );

    let worktree = context(
        &repo,
        _diffctx::snapshot::working_tree(&repo.root).expect("working tree"),
    );
    assert_eq!(
        changed(&worktree.changes),
        [
            (ChangeKind::Added, "fresh.txt".to_string()),
            (ChangeKind::Modified, "shared.txt".to_string()),
            (ChangeKind::Added, "staged.txt".to_string()),
        ]
    );
    // The untracked secret took part in the delta and is only counted.
    assert_eq!(worktree.changes.withheld, 1);
    assert!(!format!("{:?}", worktree.changes).contains("leak.pem"));
}

#[test]
fn a_withheld_change_is_counted_and_its_name_stays_inside_the_crate() {
    let repo = Repo::init();
    repo.write("a.txt", "a\n");
    repo.commit("base");
    repo.write("deploy/id_ed25519", "private\n");
    repo.write("b.txt", "b\n");
    repo.commit("head");
    let ctx = context(
        &repo,
        resolve(
            &repo.root,
            &format!("{}..{}", repo.rev("HEAD~1"), repo.head()),
        )
        .expect("resolve"),
    );
    assert_eq!(
        changed(&ctx.changes),
        [(ChangeKind::Added, "b.txt".to_string())]
    );
    assert_eq!(ctx.changes.withheld, 1);
    assert_eq!(
        ctx.head
            .decision(&_diffctx::snapshot::RepoPath::new("deploy/id_ed25519").expect("path"))
            .expect("decision"),
        Some(Decision::Withhold(WithholdReason::Secret))
    );
}

#[test]
fn renames_carry_their_source_and_similarity_and_links_are_reported_as_links() {
    let repo = Repo::init();
    let mut body = String::new();
    for i in 0..40 {
        writeln!(body, "line {i}").expect("a String takes writes");
    }
    repo.write("old_name.txt", &body);
    repo.write("config.txt", "plain file\n");
    repo.commit("base");
    repo.git(&["mv", "old_name.txt", "new_name.txt"]);
    #[cfg(unix)]
    {
        repo.remove("config.txt");
        repo.symlink("config.txt", "new_name.txt");
    }
    repo.commit("rename and retype");
    let ctx = context(
        &repo,
        resolve(
            &repo.root,
            &format!("{}..{}", repo.rev("HEAD~1"), repo.head()),
        )
        .expect("resolve"),
    );
    let rename = ctx
        .changes
        .changes
        .iter()
        .find(|c| c.kind == ChangeKind::Renamed)
        .expect("a rename");
    assert_eq!(rename.path.as_str(), "new_name.txt");
    assert_eq!(
        rename.from.as_ref().map(|p| p.as_str()),
        Some("old_name.txt")
    );
    assert_eq!(rename.similarity, Some(100));
    #[cfg(unix)]
    assert_eq!(
        ctx.changes
            .symlinks()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["config.txt"]
    );
}

#[test]
fn a_change_only_gitignore_excludes_keeps_its_name_and_a_policy_one_does_not() {
    let repo = Repo::init();
    repo.write(".gitignore", "*.log\n");
    repo.write(".diffctx/ignore", "hush.txt\n");
    repo.write("a.log", "one\n");
    repo.write("hush.txt", "one\n");
    repo.write("ok.txt", "one\n");
    repo.git(&["add", "-f", "--", "a.log"]);
    repo.commit("base");
    repo.write("a.log", "two\n");
    repo.write("hush.txt", "two\n");
    repo.write("ok.txt", "two\n");
    repo.commit("head");
    let ctx = context(
        &repo,
        resolve(
            &repo.root,
            &format!("{}..{}", repo.rev("HEAD~1"), repo.head()),
        )
        .expect("resolve"),
    );
    assert_eq!(
        changed(&ctx.changes),
        [(ChangeKind::Modified, "ok.txt".to_string())]
    );
    assert_eq!(
        ctx.changes.gitignored,
        [_diffctx::snapshot::RepoPath::new("a.log").expect("path")]
    );
    assert_eq!(ctx.changes.withheld, 1);
    assert!(!format!("{:?}", ctx.changes).contains("hush.txt"));
}
