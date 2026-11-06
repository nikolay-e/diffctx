//! Parallel contexts: readers of different snapshots, and one reader shared
//! across threads, answer for their own snapshot only.

mod snapshot_fixture;

use std::sync::Arc;

use _diffctx::snapshot::{
    Admission, AnalysisContext, Oid, RangeSpec, RepoPath, RunBudget, Side, SnapshotReader,
};
use snapshot_fixture::Repo;

const SNAPSHOTS: usize = 6;
const FILES: usize = 12;

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).expect("a path")
}

fn body(snapshot: usize, file: usize) -> String {
    format!("SNAPSHOT = {snapshot}\nFILE = {file}\n")
}

/// Commit `i` holds every file with `i` in its body, and a file only it has.
fn history() -> (Repo, Vec<Oid>) {
    let repo = Repo::init();
    let mut commits = Vec::new();
    for snapshot in 0..SNAPSHOTS {
        for file in 0..FILES {
            repo.write(&format!("pkg/m{file}.py"), &body(snapshot, file));
        }
        repo.write(&format!("only/{snapshot}.txt"), "marker\n");
        commits.push(repo.commit(&format!("snapshot {snapshot}")));
    }
    (repo, commits)
}

#[test]
fn readers_are_send_and_sync() {
    fn assert_threadsafe<T: Send + Sync>() {}
    assert_threadsafe::<SnapshotReader>();
}

#[test]
fn contexts_of_different_snapshots_run_in_threads_without_crosstalk() {
    let (repo, commits) = history();
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for (snapshot, commit) in commits.iter().enumerate() {
            {
                let root = repo.root.clone();
                let commit = commit.clone();
                workers.push(scope.spawn(move || {
                    let ctx = AnalysisContext::new(
                        root,
                        RangeSpec::At(Side::Tree(commit)),
                        RunBudget::default(),
                    )
                    .expect("context");
                    for round in 0..4 {
                        for file in 0..FILES {
                            let read = ctx
                                .head
                                .read(&path(&format!("pkg/m{file}.py")))
                                .expect("read")
                                .expect("present");
                            assert_eq!(
                                &*read,
                                body(snapshot, file).as_bytes(),
                                "snapshot {snapshot} round {round} file {file}"
                            );
                        }
                    }
                    // Only this snapshot and the ones before it hold their
                    // marker files.
                    for other in 0..SNAPSHOTS {
                        let marker = ctx
                            .head
                            .read(&path(&format!("only/{other}.txt")))
                            .expect("read");
                        assert_eq!(marker.is_some(), other <= snapshot);
                    }
                    ctx.identity()
                }));
            }
        }
        let identities: Vec<_> = workers
            .into_iter()
            .map(|w| w.join().expect("worker"))
            .collect();
        for (i, a) in identities.iter().enumerate() {
            for b in &identities[i + 1..] {
                assert_ne!(a, b, "two snapshots share an identity");
            }
        }
    });
}

#[test]
fn one_reader_shared_by_threads_serves_every_thread_its_own_path() {
    let (repo, commits) = history();
    let last = SNAPSHOTS - 1;
    let reader = SnapshotReader::new(
        &repo.root,
        Side::Tree(commits[last].clone()),
        Arc::new(Admission::default()),
    )
    .expect("reader");
    std::thread::scope(|scope| {
        for file in 0..FILES {
            let reader = reader.clone();
            scope.spawn(move || {
                for _ in 0..10 {
                    let read = reader
                        .read(&path(&format!("pkg/m{file}.py")))
                        .expect("read")
                        .expect("present");
                    assert_eq!(&*read, body(last, file).as_bytes());
                }
            });
        }
    });
    let listing = reader.list().expect("list");
    assert_eq!(listing.entries.len(), FILES + SNAPSHOTS);
}
