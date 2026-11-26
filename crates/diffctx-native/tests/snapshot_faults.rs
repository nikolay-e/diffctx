//! A reader's failure is an `Err`, never an absent file. A mutation that
//! swallowed a blob-read error survived the facade's sweep: a read that
//! failed came back as `None`, indistinguishable from a file that was never
//! there.

mod snapshot_fixture;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use diffctx_snapshot::snapshot::{
    Admission, RepoPath, RunBudget, Side, SnapshotError, SnapshotReader,
};
use snapshot_fixture::Repo;

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).expect("a path")
}

fn loose_object(repo: &Repo, spec: &str) -> std::path::PathBuf {
    let oid = repo.git(&["rev-parse", spec]);
    let oid = oid.trim();
    repo.path()
        .join(".git/objects")
        .join(&oid[..2])
        .join(&oid[2..])
}

#[test]
fn a_blob_the_tree_lists_but_the_repository_lost_is_an_error_not_an_absent_file() {
    let repo = Repo::init();
    repo.write("src/kept.py", "KEPT = 1\n");
    repo.write("src/lost.py", "LOST = 1\n");
    repo.commit("two files");
    let reader = SnapshotReader::new(
        &repo.root,
        Side::Tree(repo.head()),
        Arc::new(Admission::default()),
    )
    .expect("reader");
    // The listing is built while the object is still there.
    assert_eq!(reader.list().expect("list").entries.len(), 2);

    let lost = loose_object(&repo, "HEAD:src/lost.py");
    let mut perms = std::fs::metadata(&lost)
        .expect("a loose object")
        .permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "git writes objects read-only"
    )]
    perms.set_readonly(false);
    std::fs::set_permissions(&lost, perms).expect("chmod");
    std::fs::remove_file(&lost).expect("remove the object");

    let err = reader
        .read(&path("src/lost.py"))
        .expect_err("a failed read is an Err");
    assert!(matches!(err, SnapshotError::MissingObject { .. }), "{err}");
    // The reader is still good for what it can read, and a path it does not
    // list is still absent, not failed.
    let kept = reader
        .read(&path("src/kept.py"))
        .expect("read")
        .expect("present");
    assert_eq!(&*kept, b"KEPT = 1\n");
    assert_eq!(reader.read(&path("src/nowhere.py")).expect("read"), None);
}

#[test]
fn a_cancelled_run_fails_reads_and_listings() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.commit("one file");
    let cancel = Arc::new(AtomicBool::new(false));
    let reader = SnapshotReader::with_budget(
        &repo.root,
        Side::Tree(repo.head()),
        Arc::new(Admission::default()),
        RunBudget {
            cancel: Some(Arc::clone(&cancel)),
            ..RunBudget::default()
        },
    )
    .expect("reader");
    assert!(reader.read(&path("a.py")).expect("read").is_some());

    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        reader.read(&path("a.py")),
        Err(SnapshotError::Cancelled)
    ));
    assert!(matches!(reader.list(), Err(SnapshotError::Cancelled)));
}

#[test]
fn a_snapshot_that_names_no_object_cannot_be_opened() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.commit("one file");
    let nowhere = diffctx_snapshot::snapshot::Oid::parse(&"1".repeat(40)).expect("an oid");
    let result = SnapshotReader::new(
        &repo.root,
        Side::Tree(nowhere),
        Arc::new(Admission::default()),
    );
    assert!(result.is_err());
}

#[test]
fn an_unreadable_policy_file_fails_the_listing_instead_of_publishing_what_it_withheld() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    let policy = repo.path().join(".diffctx/ignore");
    std::fs::create_dir_all(policy.parent().expect("a parent")).expect("mkdir");
    std::fs::write(&policy, [0xff_u8, 0xfe, b'\n']).expect("write");
    repo.commit("a policy that is not text");
    let reader = SnapshotReader::new(
        &repo.root,
        Side::Tree(repo.head()),
        Arc::new(Admission::default()),
    )
    .expect("reader");
    assert!(reader.list().is_err());
    assert!(reader.read(&path("a.py")).is_err());
}

#[test]
fn an_injected_fault_fails_exactly_the_read_it_names_and_the_loss_is_recorded() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.write("b.py", "B = 1\n");
    repo.commit("two files");
    let reader = SnapshotReader::with_budget(
        &repo.root,
        Side::Tree(repo.head()),
        Arc::new(Admission::default()),
        RunBudget {
            read_fault: Some(diffctx_snapshot::snapshot::ReadFault::at(1)),
            ..RunBudget::default()
        },
    )
    .expect("reader");
    assert!(reader.read(&path("a.py")).expect("read").is_some());
    assert!(reader.read(&path("b.py")).is_err());
    assert!(reader.read(&path("a.py")).expect("read").is_some());
    assert_eq!(reader.losses().len(), 1);
}

/// Every `.git/config` line git cannot parse is a `fatal:` on every call: the
/// same failure whichever question is asked, with no stand-in for the answer.
fn break_git(repo: &Repo) {
    let config = repo.path().join(".git/config");
    let mut text = std::fs::read_to_string(&config).expect("config");
    text.push_str("[core\n");
    std::fs::write(&config, text).expect("write config");
}

fn two_commits() -> Repo {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.commit("first");
    repo.write("a.py", "A = 2\n");
    repo.commit("second\n\nwhy it changed");
    repo
}

/// git ran and refused: its `fatal` exit status, whatever language it speaks.
fn fatal(err: SnapshotError) -> bool {
    err.git_status() == Some(128)
}

/// A revision that names no commit is `None`; a read git refused is an `Err`:
/// the two never share an answer.
#[test]
fn rev_oid_tells_an_unknown_revision_from_a_failed_read() {
    let repo = two_commits();
    let head = diffctx_snapshot::snapshot::rev_oid(repo.path(), "HEAD").expect("read");
    assert_eq!(head.as_deref(), Some(repo.head().as_str()));
    assert_eq!(
        diffctx_snapshot::snapshot::rev_oid(repo.path(), "no-such-ref").expect("read"),
        None
    );
    assert_eq!(
        diffctx_snapshot::snapshot::rev_oid(repo.path(), "--option-like").expect("read"),
        None
    );
    break_git(&repo);
    assert!(fatal(
        diffctx_snapshot::snapshot::rev_oid(repo.path(), "HEAD")
            .expect_err("a failed read is an Err")
    ));
}

#[test]
fn find_toplevel_tells_no_work_tree_from_a_failed_read() {
    let repo = two_commits();
    let top = diffctx_snapshot::snapshot::find_toplevel(repo.path()).expect("read");
    assert!(top.is_some());
    let outside = tempfile::TempDir::new().expect("tempdir");
    assert_eq!(
        diffctx_snapshot::snapshot::find_toplevel(outside.path())
            .expect("not a repository is an answer"),
        None
    );
    break_git(&repo);
    assert!(fatal(
        diffctx_snapshot::snapshot::find_toplevel(repo.path())
            .expect_err("a failed read is an Err")
    ));
}

/// A bare repository and a `.git` directory are repositories with no work
/// tree: an answer, not a refusal. git's exit status is the same 128 for them
/// as for a config it cannot parse, so the two are told apart by asking git
/// again, never by its words.
#[test]
fn find_toplevel_answers_none_where_a_repository_has_no_work_tree() {
    let repo = two_commits();
    assert_eq!(
        diffctx_snapshot::snapshot::find_toplevel(&repo.path().join(".git"))
            .expect("a git directory"),
        None
    );
    let bare = tempfile::TempDir::new().expect("tempdir");
    let target = bare.path().join("bare.git");
    repo.git(&[
        "clone",
        "-q",
        "--bare",
        ".",
        target.to_str().expect("utf-8"),
    ]);
    assert_eq!(
        diffctx_snapshot::snapshot::find_toplevel(&target).expect("a bare repository"),
        None
    );
}

/// No repository at or above a directory is an answer (`false`, `NotARepo`);
/// a repository git refuses to read, and a path that is not there, are not.
#[test]
fn no_repository_is_told_from_a_repository_git_refuses() {
    let outside = tempfile::TempDir::new().expect("tempdir");
    assert!(
        !diffctx_snapshot::snapshot::is_git_repo(outside.path())
            .expect("no repository is an answer")
    );
    assert!(matches!(
        diffctx_snapshot::snapshot::open(outside.path()),
        Err(SnapshotError::NotARepo(_))
    ));
    let missing = outside.path().join("not-there");
    assert!(diffctx_snapshot::snapshot::is_git_repo(&missing).is_err());
    let repo = two_commits();
    assert!(diffctx_snapshot::snapshot::is_git_repo(repo.path()).expect("a repository"));
    break_git(&repo);
    assert!(diffctx_snapshot::snapshot::is_git_repo(repo.path()).is_err());
    assert!(matches!(
        diffctx_snapshot::snapshot::open(repo.path()),
        Err(e) if !matches!(e, SnapshotError::NotARepo(_))
    ));
}

#[test]
fn empty_tree_id_is_an_error_when_git_cannot_name_the_format() {
    let repo = two_commits();
    let id = diffctx_snapshot::snapshot::empty_tree_id(repo.path()).expect("read");
    assert_eq!(id, "4b825dc642cb6eb9a060e54bf8d69288fbee4904"); // pragma: allowlist secret
    break_git(&repo);
    assert!(fatal(
        diffctx_snapshot::snapshot::empty_tree_id(repo.path()).expect_err("no sha1 stands in")
    ));
}

/// A side that names no commit holds none (the empty tree a window older
/// than the repository starts from); a log git refused is an `Err`, not a
/// range with no commits.
#[test]
fn the_commit_lookups_tell_no_commits_from_a_failed_read() {
    let repo = two_commits();
    let root = repo.path();
    let log =
        diffctx_snapshot::snapshot::commit_log(root, "HEAD~1", "HEAD", 10, 1000).expect("read");
    assert_eq!(log.messages, ["second\n\nwhy it changed"]);
    assert_eq!(log.count, 1);
    assert_eq!(log.head.as_deref(), Some("second\nwhy it changed"));
    assert_eq!(
        diffctx_snapshot::snapshot::commit_message(root, "HEAD").expect("read"),
        "second\nwhy it changed"
    );
    let empty = diffctx_snapshot::snapshot::empty_tree_id(root).expect("read");
    let none = diffctx_snapshot::snapshot::commit_log(root, &empty, "HEAD", 10, 1000)
        .expect("a tree holds no commits");
    assert_eq!((none.count, none.messages.len(), none.head), (0, 0, None));
    assert_eq!(
        diffctx_snapshot::snapshot::commit_message(root, &empty).expect("a tree has no message"),
        ""
    );
    repo.write("a.py", "A = 3\n");
    repo.commit("third");
    let capped =
        diffctx_snapshot::snapshot::commit_log(root, "HEAD~2", "HEAD", 1, 1000).expect("read");
    assert_eq!(
        (capped.count, capped.messages, capped.head.as_deref()),
        (2, vec!["third".to_string()], Some("third"))
    );
    break_git(&repo);
    assert!(fatal(
        diffctx_snapshot::snapshot::commit_log(root, "HEAD~1", "HEAD", 10, 1000)
            .expect_err("a failed log is an Err")
    ));
    assert!(fatal(
        diffctx_snapshot::snapshot::commit_message(root, "HEAD")
            .expect_err("a failed log is an Err")
    ));
}

/// One log is what the count, the messages and the head's message were, each
/// from its own command: over a merge, a subject wrapped on two lines, and a
/// range longer than the limit.
#[test]
fn one_log_answers_what_the_count_the_messages_and_the_head_message_were() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    let first = repo.commit("first");
    repo.git(&["checkout", "-q", "-b", "side"]);
    repo.write("b.py", "B = 1\n");
    repo.commit("side one");
    repo.git(&["checkout", "-q", "main"]);
    repo.write("a.py", "A = 2\n");
    repo.commit("wrapped subject\nsecond line\n\nthe body\nof it");
    repo.git(&["merge", "-q", "--no-ff", "-m", "merge side", "side"]);
    let (root, range) = (repo.path(), format!("{}..HEAD", first.as_str()));
    let counted: usize = repo
        .git(&["rev-list", "--count", &range, "--"])
        .trim()
        .parse()
        .expect("a count");
    for limit in [2, 10] {
        let log = diffctx_snapshot::snapshot::commit_log(root, first.as_str(), "HEAD", limit, 1000)
            .expect("read");
        let max = format!("--max-count={limit}");
        let raw = repo.git(&["log", "--format=%B%x1e", &max, &range, "--"]);
        let messages: Vec<&str> = raw
            .split('\x1e')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .collect();
        assert_eq!(log.messages, messages, "limit {limit}");
        assert_eq!(log.count, counted, "limit {limit}");
        assert_eq!(
            log.head,
            Some(diffctx_snapshot::snapshot::commit_message(root, "HEAD").expect("read")),
            "limit {limit}"
        );
    }
}

/// A repository with no commit has none before a window (the empty tree);
/// one git cannot read has no answer at all.
#[test]
fn duration_base_tells_an_unborn_branch_from_a_failed_read() {
    let fresh = Repo::init();
    assert_eq!(
        diffctx_snapshot::snapshot::duration_base(fresh.path(), "24h").expect("an unborn branch"),
        Some("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string()) // pragma: allowlist secret
    );
    let repo = two_commits();
    break_git(&repo);
    assert!(fatal(
        diffctx_snapshot::snapshot::duration_base(repo.path(), "24h")
            .expect_err("a failed read is an Err")
    ));
}

#[test]
fn capture_planned_is_an_error_when_the_head_cannot_be_read() {
    let repo = two_commits();
    let planned = diffctx_snapshot::snapshot::capture_planned(repo.path(), &[]).expect("plan");
    assert_eq!(planned.head.as_deref(), Some(repo.head().as_str()));
    break_git(&repo);
    assert!(
        diffctx_snapshot::snapshot::capture_planned(repo.path(), &[]).is_err(),
        "no stand-in base"
    );
}
