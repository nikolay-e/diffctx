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
    /// The run this source serves, for the reads that fail.
    notes: std::sync::OnceLock<crate::resource::RunNotes>,
    listed: Listed,
}

/// The file list a source gave for a root, kept as long as the source: impact
/// asked for it four times a run, each a walk of the repository.
#[derive(Default)]
pub struct Listed(std::sync::OnceLock<(PathBuf, Vec<String>)>);

#[derive(Clone)]
pub enum Source {
    WorkingTree(Arc<Listed>),
    Snapshot(Arc<Snapshot>),
}

impl Default for Source {
    fn default() -> Self {
        Self::WorkingTree(Arc::default())
    }
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
            None => Ok(Self::default()),
            Some(rev) => Ok(Self::Snapshot(Arc::new(Snapshot {
                root: root.to_path_buf(),
                rev,
                batch: Mutex::new(CatFileBatch::new(root)?),
                notes: std::sync::OnceLock::new(),
                listed: Listed::default(),
            }))),
        }
    }

    /// The revision the current side is read at, `None` for the working tree.
    pub fn rev(&self) -> Option<&str> {
        match self {
            Self::WorkingTree(_) => None,
            Self::Snapshot(s) => Some(&s.rev),
        }
    }

    pub(crate) fn report_to(&self, notes: crate::resource::RunNotes) {
        if let Self::Snapshot(s) = self {
            let _ = s.notes.set(notes);
        }
    }

    /// The file's bytes, `None` when the source has no such file. A read
    /// that failed — out of time, cancelled, too large, a broken pipe — is
    /// `None` too, but noted on the run: taken for an absent file, it
    /// dropped imports and callers with no word in the answer (#408).
    pub fn read_bytes(&self, abs_path: &Path) -> Option<Vec<u8>> {
        match self {
            Self::WorkingTree(_) => match std::fs::read(abs_path) {
                Ok(bytes) => Some(bytes),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::IsADirectory
                            | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    None
                }
                Err(_) => {
                    crate::resource::note_current(crate::resource::LimitReason::ReadFailed);
                    None
                }
            },
            Self::Snapshot(s) => {
                let rel = abs_path.strip_prefix(&s.root).ok()?;
                let read = match s.batch.lock() {
                    Ok(mut batch) => batch.get_bytes(&s.rev, rel),
                    Err(_) => Err(git::GitError::Protocol("a reader panicked".into())),
                };
                match read {
                    Ok(bytes) => Some(bytes),
                    Err(git::GitError::MissingObject(_)) => None,
                    Err(e) => {
                        if let Some(notes) = s.notes.get() {
                            notes.note(crate::resource::reason_for(&e));
                        }
                        None
                    }
                }
            }
        }
    }

    pub fn read_to_string(&self, abs_path: &Path) -> Option<String> {
        let bytes = self.read_bytes(abs_path)?;
        String::from_utf8(bytes).ok()
    }

    /// Every file of the source, repo-relative: the snapshot's blobs, or the
    /// working tree's tracked and unignored untracked files. A failed listing
    /// is an error: read as empty, or a snapshot read as the working tree,
    /// it made every caller in the missing files a clean zero.
    pub fn list_paths(&self, root: &Path) -> git::Result<Vec<String>> {
        let listed = match self {
            Self::WorkingTree(listed) => listed.as_ref(),
            Self::Snapshot(s) => &s.listed,
        };
        if let Some((at, paths)) = listed.0.get() {
            if at == root {
                return Ok(paths.clone());
            }
        }
        let paths = match self.snapshot_blobs(&[]) {
            Some(blobs) => blobs?
                .into_iter()
                .filter_map(|b| crate::pipeline::rel_path_string(root, &b.path))
                .collect(),
            None => git::run_git_z(root, &["ls-files", "-co", "--exclude-standard", "-z"])?,
        };
        let _ = listed.0.set((root.to_path_buf(), paths.clone()));
        Ok(paths)
    }

    /// Every blob of the snapshot under `scope`, symlinks and submodules
    /// excluded: a link's target is not this tree's content. `None` for the
    /// working tree; a snapshot whose listing failed is an error, never the
    /// working tree's files in its place (#443).
    pub fn list_blobs(&self, scope: &[String]) -> Option<git::Result<Vec<ListedBlob>>> {
        self.snapshot_blobs(scope)
    }

    fn snapshot_blobs(&self, scope: &[String]) -> Option<git::Result<Vec<ListedBlob>>> {
        let Self::Snapshot(s) = self else {
            return None;
        };
        let mut args: Vec<&str> = vec!["ls-tree", "-r", "-z", "-l", &s.rev];
        if !scope.is_empty() {
            args.push("--");
            args.extend(scope.iter().map(String::as_str));
        }
        let records = match git::run_git_z(&s.root, &args) {
            Ok(records) => records,
            Err(e) => return Some(Err(e)),
        };
        Some(Ok(records
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
            .collect()))
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
