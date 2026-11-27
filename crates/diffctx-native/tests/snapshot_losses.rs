//! A gap is a `Loss`: the manifests the workspace model could not read, a read
//! that failed whatever its caller did with the error, and the paths a
//! listing leaves out.

mod snapshot_fixture;

use std::sync::Arc;

use diffctx_snapshot::snapshot::{
    Admission, AnalysisContext, Loss, RangeSpec, RepoPath, RunBudget, Side, SideTag, SnapshotReader,
};
use snapshot_fixture::Repo;

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).expect("a path")
}

#[test]
fn an_unreadable_manifest_is_a_named_loss_with_a_real_path() {
    let repo = Repo::init();
    repo.write("svc/pyproject.toml", "[project\nname = \n");
    repo.write("svc/app.py", "X = 1\n");
    repo.commit("a manifest that is not TOML");
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        RangeSpec::At(Side::Tree(repo.head())),
        RunBudget::default(),
    )
    .expect("context");
    let unread: Vec<_> = ctx
        .losses()
        .into_iter()
        .filter(|loss| loss.kind() == "manifest_unread")
        .collect();
    assert_eq!(unread.len(), 1, "{unread:?}");
    assert_eq!(unread[0].path(), Some(&path("svc/pyproject.toml")));
}

#[test]
fn a_failed_read_stays_a_loss_after_its_caller_dropped_the_error() {
    let repo = Repo::init();
    repo.write("kept.py", "KEPT = 1\n");
    repo.write("lost.py", "LOST = 1\n");
    repo.commit("two files");
    let reader = SnapshotReader::new(
        &repo.root,
        Side::Tree(repo.head()),
        Arc::new(Admission::default()),
    )
    .expect("reader");
    assert!(!reader.has_losses());
    assert_eq!(reader.list().expect("list").entries.len(), 2);

    let oid = repo.git(&["rev-parse", "HEAD:lost.py"]);
    let oid = oid.trim();
    let object = repo
        .path()
        .join(".git/objects")
        .join(&oid[..2])
        .join(&oid[2..]);
    let mut perms = std::fs::metadata(&object)
        .expect("a loose object")
        .permissions();
    #[allow(
        clippy::permissions_set_readonly_false,
        reason = "git writes objects read-only"
    )]
    perms.set_readonly(false);
    std::fs::set_permissions(&object, perms).expect("chmod");
    std::fs::remove_file(&object).expect("remove the object");

    let dropped = reader.read(&path("lost.py")).is_ok();
    assert!(!dropped);
    assert!(reader.read(&path("kept.py")).expect("read").is_some());
    assert!(reader.has_losses());
    assert_eq!(
        reader.losses(),
        vec![Loss::ReadFailed {
            path: path("lost.py"),
            side: SideTag::Head
        }]
    );
}

#[cfg(unix)]
#[test]
fn a_file_git_cannot_read_is_a_loss_of_the_working_tree_not_the_end_of_its_read() {
    use std::os::unix::fs::PermissionsExt as _;
    let repo = Repo::init();
    repo.write("kept.py", "KEPT = 1\n");
    repo.write("locked.py", "LOCKED = 1\n");
    repo.commit("two files");
    let locked = repo.path().join("locked.py");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    if std::fs::read(&locked).is_ok() {
        return;
    }
    let attempt = SnapshotReader::new(
        &repo.root,
        Side::WorkingTree,
        Arc::new(Admission::default()),
    )
    .map(|reader| {
        let entries = reader.tree_entries();
        (reader, entries)
    });
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    let (reader, entries) = attempt.expect("a reader over the checkout");
    let entries = entries.expect("entries");
    let oid_of = |name: &str| {
        entries
            .entries
            .iter()
            .find(|entry| entry.path == path(name))
            .map(|entry| entry.oid.clone())
    };
    assert!(matches!(oid_of("kept.py"), Some(Some(_))), "{entries:?}");
    assert_eq!(oid_of("locked.py"), Some(None), "{entries:?}");
    assert!(
        reader.losses().iter().any(|loss| matches!(
            loss,
            Loss::ReadFailed { path: lost, .. } if *lost == path("locked.py")
        )),
        "{:?}",
        reader.losses()
    );
}

#[test]
fn a_listing_counts_what_it_withholds_and_names_none_of_it() {
    let repo = Repo::init();
    repo.write("a.py", "A = 1\n");
    repo.write("id_rsa", "secret\n");
    repo.commit("a key");
    let reader = SnapshotReader::new(
        &repo.root,
        Side::Tree(repo.head()),
        Arc::new(Admission::default()),
    )
    .expect("reader");
    let losses = reader.list().expect("list").losses();
    assert_eq!(losses, vec![Loss::Withheld { count: 1 }], "never a path");
}
