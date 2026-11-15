//! The working tree is read as it was listed: a file rewritten between the
//! listing and its read is a named loss, never its new content; what was read
//! once is answered again as read.

mod snapshot_fixture;

use std::sync::Arc;

use _diffctx::snapshot::{Admission, Loss, RepoPath, Side, SnapshotError, SnapshotReader};
use snapshot_fixture::Repo;

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).expect("a path")
}

fn working(repo: &Repo) -> SnapshotReader {
    SnapshotReader::new(
        &repo.root,
        Side::WorkingTree,
        Arc::new(Admission::default()),
    )
    .expect("reader")
}

#[test]
fn a_file_rewritten_between_the_listing_and_its_read_is_a_named_loss() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.write("b.py", "B = 1\n");
    repo.write("c.py", "C = 1\n");
    repo.commit("three files");
    let reader = working(&repo);
    assert_eq!(reader.list().expect("list").entries.len(), 3);

    repo.write("a.py", "A = 'rewritten after the listing'\n");
    repo.remove("c.py");

    let err = reader
        .read(&path("a.py"))
        .expect_err("a changed file is not the snapshot's");
    assert!(matches!(err, SnapshotError::Changed { .. }), "{err}");
    let err = reader
        .read(&path("c.py"))
        .expect_err("a vanished file is not an absent one");
    assert!(matches!(err, SnapshotError::Changed { .. }), "{err}");
    assert_eq!(
        &*reader.read(&path("b.py")).expect("read").expect("present"),
        b"B = 1\n"
    );
    assert_eq!(
        reader.losses(),
        vec![
            Loss::SnapshotChanged { path: path("a.py") },
            Loss::SnapshotChanged { path: path("c.py") },
        ]
    );
}

#[test]
fn what_was_read_once_is_answered_again_as_read() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.commit("one file");
    let reader = working(&repo);
    reader.list().expect("list");
    let first = reader.read(&path("a.py")).expect("read").expect("present");
    repo.write("a.py", "A = 'changed after it was read, and longer'\n");
    let again = reader.read(&path("a.py")).expect("read").expect("present");
    assert_eq!(&*first, b"A = 1\n");
    assert_eq!(&*again, &*first);
    assert!(!reader.has_losses());
}
