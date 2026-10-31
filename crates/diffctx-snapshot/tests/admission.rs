//! Admission parity: the hook's `tree_entries` and the engine's `list` are
//! one set; what admission withholds is counted and never named.

mod common;

use std::sync::Arc;

use common::{Repo, names};
use diffctx_snapshot::{
    Admission, AdmissionPolicy, Decision, NotAnalysedReason, Side, SnapshotError, SnapshotReader,
    UntrackedPolicy, WithholdReason,
};

/// The withheld names as they would print inside a `Debug` of a listing.
const CANARIES: [&str; 4] = [
    "deploy/id_rsa\"",
    "keys/server.pem",
    "hush.txt",
    "debug.log",
];

fn path(p: &str) -> diffctx_snapshot::RepoPath {
    diffctx_snapshot::RepoPath::new(p).expect("a path")
}

fn policy_repo() -> Repo {
    let repo = Repo::init();
    repo.write(".gitignore", "*.log\n");
    repo.write(".diffctx/ignore", "hush.txt\n");
    repo.write(".env", "MODE=dev\n");
    repo.write("deploy/id_rsa", "-----BEGIN-----\n");
    repo.write("deploy/id_rsa.pub", "ssh-rsa AAAA\n");
    repo.write("keys/server.pem", "-----BEGIN-----\n");
    repo.write("hush.txt", "declared private\n");
    repo.write("debug.log", "noise\n");
    repo.write("src/ok.py", "OK = 1\n");
    repo.git(&["add", "-f", "--", "debug.log"]);
    repo.commit("policy");
    repo
}

fn reader(repo: &Repo, side: Side) -> SnapshotReader {
    SnapshotReader::new(&repo.root, side, Arc::new(Admission::default())).expect("reader")
}

#[test]
fn withheld_paths_are_counted_and_never_named() {
    let repo = policy_repo();
    let reader = reader(&repo, Side::Tree(repo.head()));
    let listing = reader.list().expect("list");

    assert_eq!(
        names(&listing),
        [
            ".diffctx/ignore",
            ".env",
            ".gitignore",
            "deploy/id_rsa.pub",
            "src/ok.py"
        ]
    );
    assert_eq!(listing.withheld, 4);
    assert_eq!(listing.not_analysed.len(), 0);

    let shown = format!("{listing:?} {:?}", reader.tree_entries().expect("entries"));
    for canary in CANARIES {
        assert!(!shown.contains(canary), "{canary} leaked into {shown}");
    }
    for withheld in ["deploy/id_rsa", "keys/server.pem", "hush.txt", "debug.log"] {
        assert_eq!(
            reader.read(&path(withheld)).expect("read"),
            None,
            "{withheld}"
        );
    }
    let env = reader.read(&path(".env")).expect("read").expect("kept");
    assert_eq!(&*env, b"MODE=dev\n");
}

#[test]
fn the_hooks_entries_are_the_engines_admitted_list_on_every_side() {
    let repo = policy_repo();
    repo.write("scratch.tmp", "untracked\n");
    repo.write("extra.pem", "untracked secret\n");
    repo.write("noise.log", "untracked and ignored\n");
    for side in [Side::Tree(repo.head()), Side::WorkingTree] {
        let reader = reader(&repo, side.clone());
        let list = reader.list().expect("list");
        let entries = reader.tree_entries().expect("entries");
        assert_eq!(names(&list), names(&entries), "{side:?}");
        assert_eq!(list.withheld, entries.withheld, "{side:?}");
        assert!(entries.entries.iter().all(|e| e.oid.is_some()), "{side:?}");
    }
    let working = reader(&repo, Side::WorkingTree).list().expect("list");
    assert!(names(&working).contains(&"scratch.tmp".to_string()));
    // The four withheld in the tree, plus the untracked secret. The untracked
    // log is excluded by git's own listing and is not counted.
    assert_eq!(working.withheld, 5);
}

#[test]
fn working_tree_blob_ids_are_the_ones_git_stores() {
    let repo = policy_repo();
    repo.write("src/ok.py", "OK = 2\n");
    let entries = reader(&repo, Side::WorkingTree)
        .tree_entries()
        .expect("entries");
    let stored = repo.git(&["hash-object", "src/ok.py"]);
    let entry = entries
        .entries
        .iter()
        .find(|e| e.path.as_str() == "src/ok.py")
        .expect("listed");
    assert_eq!(
        entry.oid.as_ref().map(ToString::to_string),
        Some(stored.trim().to_string())
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_is_its_target_name_and_is_followed_only_inside_the_snapshot() {
    let repo = Repo::init();
    repo.write("src/ok.py", "OK = 1\n");
    repo.symlink("alias.py", "src/ok.py");
    repo.symlink("up", "../outside");
    repo.symlink("chain", "alias.py");
    repo.commit("links");
    let reader = reader(&repo, Side::Tree(repo.head()));

    let listing = reader.list().expect("list");
    assert_eq!(names(&listing), ["src/ok.py"]);
    let mut links: Vec<_> = listing
        .not_analysed
        .iter()
        .map(|(p, r)| (p.as_str().to_string(), *r))
        .collect();
    links.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        links,
        [
            ("alias.py".to_string(), NotAnalysedReason::Symlink),
            ("chain".to_string(), NotAnalysedReason::Symlink),
            ("up".to_string(), NotAnalysedReason::Symlink),
        ]
    );

    let text = reader
        .read(&path("alias.py"))
        .expect("read")
        .expect("present");
    assert_eq!(&*text, b"src/ok.py", "the target's name, not its content");
    assert_eq!(
        reader.resolve_link(&path("alias.py")).expect("link"),
        Some(path("src/ok.py"))
    );
    assert_eq!(
        reader.resolve_link(&path("chain")).expect("link"),
        Some(path("src/ok.py"))
    );
    assert_eq!(reader.resolve_link(&path("up")).expect("link"), None);
}

#[test]
fn a_file_over_the_cap_is_listed_not_read() {
    let repo = Repo::init();
    repo.write("big.txt", &"x".repeat(100));
    repo.write("small.txt", "x\n");
    repo.commit("sizes");
    let admission = Admission::new(AdmissionPolicy {
        size_cap: 10,
        ..AdmissionPolicy::default()
    });
    let reader = SnapshotReader::new(&repo.root, Side::Tree(repo.head()), Arc::new(admission))
        .expect("reader");
    let listing = reader.list().expect("list");
    assert_eq!(names(&listing), ["small.txt"]);
    assert_eq!(
        listing.not_analysed,
        [(path("big.txt"), NotAnalysedReason::Oversize)]
    );
    assert!(matches!(
        reader.read(&path("big.txt")),
        Err(SnapshotError::TooLarge { size: 100, .. })
    ));
}

#[test]
fn the_untracked_policy_can_close_the_working_tree_to_new_files() {
    let repo = policy_repo();
    repo.write("scratch.tmp", "untracked\n");
    let admission = Admission::new(AdmissionPolicy {
        untracked: UntrackedPolicy::Exclude,
        ..AdmissionPolicy::default()
    });
    let reader =
        SnapshotReader::new(&repo.root, Side::WorkingTree, Arc::new(admission)).expect("reader");
    let listing = reader.list().expect("list");
    assert!(!names(&listing).contains(&"scratch.tmp".to_string()));
    assert_eq!(
        listing.not_analysed,
        [(path("scratch.tmp"), NotAnalysedReason::Untracked)]
    );
    assert_eq!(
        reader.decision(&path("deploy/id_rsa")).expect("decision"),
        Some(Decision::Withhold(WithholdReason::Secret))
    );
}
