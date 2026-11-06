//! A reader's failure is an `Err`, never an absent file. The facade's sweep
//! missed a swallowed blob-read error (mutation m3, relay arch-migration):
//! a read that failed came back as `None`, indistinguishable from a file that
//! was never there.

mod snapshot_fixture;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use _diffctx::snapshot::{Admission, RepoPath, RunBudget, Side, SnapshotError, SnapshotReader};
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
    let nowhere = _diffctx::snapshot::Oid::parse(&"1".repeat(40)).expect("an oid");
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
            read_fault: Some(_diffctx::snapshot::ReadFault::at(1)),
            ..RunBudget::default()
        },
    )
    .expect("reader");
    assert!(reader.read(&path("a.py")).expect("read").is_some());
    assert!(reader.read(&path("b.py")).is_err());
    assert!(reader.read(&path("a.py")).expect("read").is_some());
    assert_eq!(reader.losses().len(), 1);
}
