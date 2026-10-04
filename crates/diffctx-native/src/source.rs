//! Where a run's current-side bytes come from. A range whose head is a
//! revision or a tree (`A..B`, the staged index captured as a tree) is
//! analysed from that object alone: its file list, the content discovery
//! reads, the fragments, the stale-reference search. The disk is the source
//! only when the head side is the working tree; supplying disk bytes for a
//! snapshot answers for a state nobody asked about (#354).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;

use crate::git::{self, CatFileBatch};

pub struct Snapshot {
    root: PathBuf,
    rev: String,
    batch: Mutex<CatFileBatch>,
}

#[derive(Clone, Default)]
pub enum Source {
    #[default]
    WorkingTree,
    Snapshot(Arc<Snapshot>),
}

/// One blob of a snapshot's listing, by absolute path.
pub struct ListedBlob {
    pub path: PathBuf,
    pub oid: String,
    pub size: u64,
}

impl Source {
    /// The source for a range: its explicit head, else the working tree.
    pub fn for_range(root: &Path, diff_range: Option<&str>) -> crate::git::Result<Self> {
        let head = diff_range
            .map(git::split_diff_range)
            .and_then(|(_, head)| head);
        match head {
            None => Ok(Self::WorkingTree),
            Some(rev) => Ok(Self::Snapshot(Arc::new(Snapshot {
                root: root.to_path_buf(),
                rev,
                batch: Mutex::new(CatFileBatch::new(root)?),
            }))),
        }
    }

    /// The revision the current side is read at, `None` for the working tree.
    pub fn rev(&self) -> Option<&str> {
        match self {
            Self::WorkingTree => None,
            Self::Snapshot(s) => Some(&s.rev),
        }
    }

    pub fn read_bytes(&self, abs_path: &Path) -> Option<Vec<u8>> {
        match self {
            Self::WorkingTree => std::fs::read(abs_path).ok(),
            Self::Snapshot(s) => {
                let rel = abs_path.strip_prefix(&s.root).ok()?;
                s.batch.lock().ok()?.get_bytes(&s.rev, rel).ok()
            }
        }
    }

    pub fn read_to_string(&self, abs_path: &Path) -> Option<String> {
        let bytes = self.read_bytes(abs_path)?;
        String::from_utf8(bytes).ok()
    }

    /// Every file of the source, repo-relative: the snapshot's blobs, or the
    /// working tree's tracked and unignored untracked files.
    pub fn list_paths(&self, root: &Path) -> Vec<String> {
        if let Some(blobs) = self.list_blobs(&[]) {
            return blobs
                .into_iter()
                .filter_map(|b| crate::pipeline::rel_path_string(root, &b.path))
                .collect();
        }
        git::run_git_z(root, &["ls-files", "-co", "--exclude-standard", "-z"]).unwrap_or_default()
    }

    /// Every blob of the snapshot under `scope`, symlinks and submodules
    /// excluded: a link's target is not this tree's content.
    pub fn list_blobs(&self, scope: &[String]) -> Option<Vec<ListedBlob>> {
        let Self::Snapshot(s) = self else {
            return None;
        };
        let mut args: Vec<&str> = vec!["ls-tree", "-r", "-z", "-l", &s.rev];
        if !scope.is_empty() {
            args.push("--");
            args.extend(scope.iter().map(String::as_str));
        }
        let records = git::run_git_z(&s.root, &args).ok()?;
        Some(
            records
                .iter()
                .filter_map(|record| {
                    let (meta, path) = record.split_once('\t')?;
                    let mut fields = meta.split_whitespace();
                    let mode = fields.next()?;
                    let kind = fields.next()?;
                    let oid = fields.next()?;
                    let size = fields.next()?.parse().ok()?;
                    (kind == "blob" && mode != "120000").then(|| ListedBlob {
                        path: crate::paths::repo_join(&s.root, path),
                        oid: oid.to_string(),
                        size,
                    })
                })
                .collect(),
        )
    }
}

thread_local! {
    static SCOPED: std::cell::RefCell<Option<Source>> = const { std::cell::RefCell::new(None) };
}

/// Runs `f` with `source` as this thread's source: for rayon workers the
/// run's context is not published to.
pub fn with_source<T>(source: &Source, f: impl FnOnce() -> T) -> T {
    let prev = SCOPED.with(|c| c.replace(Some(source.clone())));
    let out = f();
    SCOPED.with(|c| *c.borrow_mut() = prev);
    out
}

/// This thread's source: a scoped one, else the current run's, else the
/// working tree.
pub fn current() -> Source {
    SCOPED
        .with(|c| c.borrow().clone())
        .or_else(crate::resource::current_source)
        .unwrap_or_default()
}

pub fn read_to_string(abs_path: &Path) -> Option<String> {
    current().read_to_string(abs_path)
}

/// `cache` first, then the current run's source.
pub fn read_cached(abs_path: &Path, cache: Option<&FxHashMap<PathBuf, String>>) -> Option<String> {
    cache
        .and_then(|c| c.get(abs_path).cloned())
        .or_else(|| read_to_string(abs_path))
}
