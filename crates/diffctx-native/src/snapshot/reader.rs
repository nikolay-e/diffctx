//! The reader: one snapshot of one repository, read through one door. A path
//! is answered only if the snapshot lists it and admission admits it; there is
//! no fallback to the disk for a tree side, and a failed read is an `Err`,
//! never an absent file (m3, #408).

use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::SystemTime;

use rustc_hash::FxHashMap;

use crate::snapshot::admission::{
    Admission, Decision, IgnoreSource, Meta, NotAnalysedReason, floor,
};
use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::git::{CatBatch, Exec, Reply, RunBudget, Scratch, git_command};
use crate::snapshot::ignore::{self, Basis, Hit};
use crate::snapshot::loss::{Loss, SideTag};
use crate::snapshot::repo::{RepoRoot, index_tree_with, rev_to_oid, tree_of};
use crate::snapshot::types::{Bytes, ContentId, Mode, Oid, RepoPath, digest};

/// Which state of the repository a reader serves.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Side {
    /// A commit's tree, or a tree.
    Tree(Oid),
    /// The index, captured once as a tree on a copy.
    Index,
    /// The checkout's tracked files and its admitted untracked ones.
    WorkingTree,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    pub path: RepoPath,
    pub mode: Mode,
    /// The blob. A working-tree listing leaves it `None` (hashing every file
    /// is not free); `tree_entries` fills it.
    pub oid: Option<Oid>,
    pub size: u64,
    pub admitted: Decision,
}

/// What a snapshot holds that may be shown. A withheld path is counted here
/// and named nowhere; a path that is listed but not read as source is named
/// with the reason.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub withheld: usize,
    pub not_analysed: Vec<(RepoPath, NotAnalysedReason)>,
}

impl Listing {
    /// What the listing left out of the answer: the withheld paths as a
    /// count, the listed paths that are not read as source by name.
    pub fn losses(&self) -> Vec<Loss> {
        let mut losses = Vec::new();
        if self.withheld > 0 {
            losses.push(Loss::Withheld {
                count: self.withheld,
            });
        }
        losses.extend(
            self.not_analysed
                .iter()
                .map(|(path, reason)| Loss::NotAnalysed {
                    path: path.clone(),
                    reason: *reason,
                }),
        );
        losses
    }
}

/// What the listing saw of a working-tree file: the pair a later read must
/// still find, or the bytes it returns are not the snapshot's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Stamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

fn stamp_of(meta: &std::fs::Metadata) -> Stamp {
    Stamp {
        len: meta.len(),
        #[allow(
            clippy::disallowed_methods,
            reason = "a platform without mtimes makes the stamp length only: it cannot see a same-size rewrite, and says so by comparing None to None"
        )]
        modified: meta.modified().ok(),
    }
}

pub(crate) struct Row {
    pub path: RepoPath,
    pub mode: Mode,
    pub oid: Option<Oid>,
    pub size: u64,
    /// A working-tree row's stamp at the listing; `None` for a tree's.
    pub stamp: Option<Stamp>,
    pub tracked: bool,
    pub decision: Decision,
}

/// The working-tree bytes already read, kept so that a second read answers
/// what the first did; past the cap a file is read again and checked against
/// its stamp instead.
#[derive(Default)]
struct Remembered {
    bytes: usize,
    files: FxHashMap<String, Bytes>,
}

const REMEMBER_BYTES: usize = 64 * 1024 * 1024;

pub(crate) struct Table {
    pub rows: Vec<Row>,
    index: FxHashMap<String, usize>,
}

impl Table {
    fn new(mut rows: Vec<Row>) -> Self {
        rows.sort_by(|a, b| a.path.cmp(&b.path));
        rows.dedup_by(|a, b| a.path == b.path);
        let index = rows
            .iter()
            .enumerate()
            .map(|(i, row)| (row.path.as_str().to_string(), i))
            .collect();
        Self { rows, index }
    }

    pub(crate) fn get(&self, path: &RepoPath) -> Option<&Row> {
        self.index.get(path.as_str()).map(|i| &self.rows[*i])
    }
}

enum Backing {
    Tree(Oid),
    WorkingTree,
}

struct Inner {
    /// Which end of the range this reader serves, once the context says.
    role: OnceLock<SideTag>,
    /// Paths whose bytes the reader failed to produce, in order, and whether
    /// the cause was the file changing under it.
    failures: Mutex<Vec<(RepoPath, bool)>>,
    remembered: Mutex<Remembered>,
    root: RepoRoot,
    basis: Backing,
    id: ContentId,
    admission: Arc<Admission>,
    exec: Exec,
    batch: Mutex<CatBatch>,
    table: Mutex<Option<Arc<Table>>>,
}

/// `Send + Sync` and cheap to clone: clones share the connection, the
/// listing and the admission verdicts.
#[derive(Clone)]
pub struct SnapshotReader {
    inner: Arc<Inner>,
}

impl fmt::Debug for SnapshotReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SnapshotReader")
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| SnapshotError::Protocol("a reader panicked".into()))
}

impl SnapshotReader {
    pub fn new(root: &RepoRoot, side: Side, admission: Arc<Admission>) -> Result<Self> {
        Self::with_budget(root, side, admission, RunBudget::default())
    }

    /// A reader whose every git call and disk read is held to the run's
    /// deadline and cancellation.
    pub fn with_budget(
        root: &RepoRoot,
        side: Side,
        admission: Arc<Admission>,
        budget: RunBudget,
    ) -> Result<Self> {
        let exec = Exec::with_budget(budget);
        let work = &root.work_tree;
        let (basis, id) = match side {
            Side::Tree(oid) => {
                let tree = tree_of(&exec, work, &oid)?;
                let id = ContentId(tree.to_string());
                (Backing::Tree(tree), id)
            }
            Side::Index => {
                let tree = index_tree_with(&exec, work)?;
                let id = ContentId(tree.to_string());
                (Backing::Tree(tree), id)
            }
            Side::WorkingTree => (Backing::WorkingTree, working_tree_id(&exec, root)?),
        };
        let cap = usize::try_from(admission.policy().size_cap).unwrap_or(usize::MAX);
        Ok(Self {
            inner: Arc::new(Inner {
                role: OnceLock::new(),
                failures: Mutex::new(Vec::new()),
                remembered: Mutex::new(Remembered::default()),
                root: root.clone(),
                basis,
                id,
                batch: Mutex::new(CatBatch::new(work, cap, exec.clone())),
                admission,
                exec,
                table: Mutex::new(None),
            }),
        })
    }

    pub fn root(&self) -> &RepoRoot {
        &self.inner.root
    }

    /// Names the end of the range this reader serves; the first call wins.
    pub(crate) fn set_role(&self, role: SideTag) {
        let _ = self.inner.role.set(role);
    }

    fn role(&self) -> SideTag {
        self.inner.role.get().copied().unwrap_or(SideTag::Head)
    }

    /// Whether a read of this snapshot has failed, however its caller took it.
    pub fn has_losses(&self) -> bool {
        self.inner
            .failures
            .lock()
            .map_or(true, |failures| !failures.is_empty())
    }

    /// What reads of this snapshot lost so far: every path whose bytes were
    /// wanted and not produced. A caller that swallowed the `Err` has not
    /// made the loss go away.
    pub fn losses(&self) -> Vec<Loss> {
        let side = self.role();
        let failures = self
            .inner
            .failures
            .lock()
            .map_or_else(|poisoned| poisoned.into_inner().clone(), |f| f.clone());
        failures
            .into_iter()
            .map(|(path, changed)| {
                if changed {
                    Loss::SnapshotChanged { path }
                } else {
                    Loss::ReadFailed { path, side }
                }
            })
            .fold(Vec::new(), |mut losses, loss| {
                if !losses.contains(&loss) {
                    losses.push(loss);
                }
                losses
            })
    }

    fn record_failure(&self, path: &RepoPath, error: &SnapshotError) {
        if error.is_interruption() || matches!(error, SnapshotError::TooLarge { .. }) {
            return;
        }
        if let Ok(mut failures) = self.inner.failures.lock() {
            failures.push((path.clone(), matches!(error, SnapshotError::Changed { .. })));
        }
    }

    /// The tree oid, or `wt:` and a digest of what the working tree holds
    /// beyond `HEAD`.
    pub fn id(&self) -> &ContentId {
        &self.inner.id
    }

    pub fn admission(&self) -> &Arc<Admission> {
        &self.inner.admission
    }

    /// The file's bytes. `Ok(None)` means the snapshot has no such file, or
    /// has it and withholds it. A file the snapshot lists but the repository
    /// cannot produce, a read cut short, a file over the size cap: `Err`.
    /// A symlink reads as its target's name, never the target's content.
    pub fn read(&self, path: &RepoPath) -> Result<Option<Bytes>> {
        self.inner.exec.check()?;
        let table = self.table()?;
        let Some(row) = table.get(path) else {
            return Ok(None);
        };
        match row.decision {
            Decision::Admit | Decision::NotAnalysed(NotAnalysedReason::Symlink) => self.fetch(row),
            Decision::NotAnalysed(NotAnalysedReason::Oversize) => Err(SnapshotError::TooLarge {
                path: path.to_string(),
                size: row.size,
            }),
            Decision::Withhold(_) | Decision::NotAnalysed(_) => Ok(None),
        }
    }

    /// What may be shown: the admitted entries, sorted by path.
    pub fn list(&self) -> Result<Listing> {
        self.inner.exec.check()?;
        let table = self.table()?;
        let mut listing = Listing {
            entries: Vec::new(),
            withheld: 0,
            not_analysed: Vec::new(),
        };
        for row in &table.rows {
            match row.decision {
                Decision::Admit => listing.entries.push(Entry {
                    path: row.path.clone(),
                    mode: row.mode,
                    oid: row.oid.clone(),
                    size: row.size,
                    admitted: Decision::Admit,
                }),
                Decision::Withhold(_) => listing.withheld += 1,
                Decision::NotAnalysed(reason) => {
                    listing.not_analysed.push((row.path.clone(), reason));
                }
            }
        }
        Ok(listing)
    }

    /// The same set as `list`, every entry carrying its blob id: content
    /// identity without content. A working tree's files are hashed as `git
    /// add` would store them, filters applied, nothing written.
    pub fn tree_entries(&self) -> Result<Listing> {
        let mut listing = self.list()?;
        if matches!(self.inner.basis, Backing::WorkingTree) {
            let paths: Vec<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
            let oids = hash_objects(&self.inner.exec, &self.inner.root.work_tree, &paths)?;
            for (entry, oid) in listing.entries.iter_mut().zip(oids) {
                entry.oid = Some(oid);
            }
        }
        Ok(listing)
    }

    /// What admission says about one path of this snapshot; `None` when the
    /// snapshot does not hold it.
    pub fn decision(&self, path: &RepoPath) -> Result<Option<Decision>> {
        Ok(self.table()?.get(path).map(|row| row.decision))
    }

    /// The path a symlink names, followed inside this snapshot only: a link
    /// that leaves the repository, dangles, loops, or ends at a path that is
    /// not admitted resolves to `None`.
    pub fn resolve_link(&self, path: &RepoPath) -> Result<Option<RepoPath>> {
        let table = self.table()?;
        let mut current = path.clone();
        for _ in 0..8 {
            let Some(row) = table.get(&current) else {
                return Ok(None);
            };
            if row.mode != Mode::Symlink {
                return Ok((row.decision == Decision::Admit).then_some(current));
            }
            let Some(text) = self.read(&current)? else {
                return Ok(None);
            };
            let Ok(text) = std::str::from_utf8(&text) else {
                return Ok(None);
            };
            let Some(target) = join_inside(current.parent().unwrap_or(""), text) else {
                return Ok(None);
            };
            current = target;
        }
        Ok(None)
    }

    /// Which of `paths` this snapshot withholds, listed or not. A path the
    /// snapshot lists is answered by its admission; one it does not (a file
    /// the working tree's ignore rules exclude, a directory, a path that does
    /// not exist) by the name floor and, for a working tree, the ignore rules
    /// asked directly, so a surface that serves paths of its own choosing
    /// refuses exactly what a listing would have withheld.
    pub fn withheld_among(&self, paths: &[RepoPath]) -> Result<Vec<RepoPath>> {
        let table = self.table()?;
        let mut withheld = Vec::new();
        let mut unlisted: Vec<&RepoPath> = Vec::new();
        for path in paths {
            match table.get(path) {
                Some(row) => {
                    if matches!(row.decision, Decision::Withhold(_)) {
                        withheld.push(path.clone());
                    }
                }
                None if floor(path).is_some() => withheld.push(path.clone()),
                None => unlisted.push(path),
            }
        }
        if matches!(self.inner.basis, Backing::WorkingTree) && !unlisted.is_empty() {
            let names: Vec<&str> = unlisted.iter().map(|p| p.as_str()).collect();
            let hits = ignore::check(
                &self.inner.exec,
                &Basis::WorkingTree {
                    root: &self.inner.root.work_tree,
                },
                &names,
            )?;
            withheld.extend(
                unlisted
                    .into_iter()
                    .filter(|p| hits.contains_key(p.as_str()))
                    .cloned(),
            );
        }
        Ok(withheld)
    }

    /// The tree this reader serves; `None` for the working tree.
    pub(crate) fn tree(&self) -> Option<&Oid> {
        match &self.inner.basis {
            Backing::Tree(tree) => Some(tree),
            Backing::WorkingTree => None,
        }
    }

    pub(crate) fn exec(&self) -> &Exec {
        &self.inner.exec
    }

    pub(crate) fn table(&self) -> Result<Arc<Table>> {
        let mut slot = lock(&self.inner.table)?;
        if let Some(table) = slot.as_ref() {
            return Ok(Arc::clone(table));
        }
        let rows = match &self.inner.basis {
            Backing::Tree(tree) => self.tree_rows(tree)?,
            Backing::WorkingTree => self.working_rows()?,
        };
        let table = Arc::new(Table::new(rows));
        *slot = Some(Arc::clone(&table));
        Ok(table)
    }

    fn fetch(&self, row: &Row) -> Result<Option<Bytes>> {
        let fetched = self.fetch_row(row);
        if let Err(error) = &fetched {
            self.record_failure(&row.path, error);
        }
        fetched
    }

    fn fetch_row(&self, row: &Row) -> Result<Option<Bytes>> {
        match (&self.inner.basis, &row.oid) {
            (Backing::Tree(_), Some(oid)) => self.read_object(&row.path, oid).map(Some),
            (Backing::Tree(_), None) => Err(SnapshotError::Git(format!(
                "the tree lists {} without an object",
                row.path
            ))),
            (Backing::WorkingTree, _) => self.read_disk(row),
        }
    }

    fn read_object(&self, path: &RepoPath, oid: &Oid) -> Result<Bytes> {
        self.inner.exec.inject_read_fault()?;
        // A stream that went wrong twice on this object is an error: it is
        // never answered another way, so no read is quietly a different one.
        match lock(&self.inner.batch)?.get(oid.as_str())? {
            Reply::Blob(bytes) => Ok(Bytes::from(bytes)),
            // The tree names this object: git not having it is a broken or
            // partial repository, not an absent file.
            Reply::Missing => Err(SnapshotError::MissingObject {
                path: path.to_string(),
                oid: oid.to_string(),
            }),
            Reply::NotBlob => Err(SnapshotError::Git(format!(
                "{path} is listed with object {oid}, which is not a blob"
            ))),
            Reply::TooLarge(size) => Err(SnapshotError::TooLarge {
                path: path.to_string(),
                size: size as u64,
            }),
        }
    }

    /// The bytes of a working-tree file the listing saw. They are the
    /// snapshot's only while the file still has the stamp the listing
    /// recorded: a file that vanished, was replaced or was rewritten since is
    /// a named loss (`Loss::SnapshotChanged`), never its new content. What
    /// was read once is answered again as read.
    #[allow(
        clippy::disallowed_methods,
        reason = "the working-tree door: the one place that reads a tracked file from the disk"
    )]
    fn read_disk(&self, row: &Row) -> Result<Option<Bytes>> {
        let path = &row.path;
        let known = lock(&self.inner.remembered)?
            .files
            .get(path.as_str())
            .cloned();
        if known.is_some() {
            return Ok(known);
        }
        self.inner.exec.inject_read_fault()?;
        let changed = || SnapshotError::Changed {
            path: path.to_string(),
        };
        let at = path.under(&self.inner.root.work_tree);
        let meta = match std::fs::symlink_metadata(&at) {
            Ok(meta) => meta,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Err(changed());
            }
            Err(e) => return Err(SnapshotError::Io(e)),
        };
        let stamp = stamp_of(&meta);
        if row.stamp.is_some_and(|listed| listed != stamp) {
            return Err(changed());
        }
        let bytes = if meta.file_type().is_symlink() {
            Bytes::from(link_text(&at)?)
        } else if meta.is_file() {
            let cap = self.inner.admission.policy().size_cap;
            if meta.len() > cap {
                return Err(SnapshotError::TooLarge {
                    path: path.to_string(),
                    size: meta.len(),
                });
            }
            match std::fs::read(&at) {
                Ok(bytes) => Bytes::from(bytes),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::IsADirectory
                    ) =>
                {
                    return Err(changed());
                }
                Err(e) => return Err(SnapshotError::Io(e)),
            }
        } else {
            return Err(changed());
        };
        // Rewritten while it was being read.
        match std::fs::symlink_metadata(&at) {
            Ok(after) if stamp_of(&after) == stamp => {}
            Ok(_) => return Err(changed()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(changed()),
            Err(e) => return Err(SnapshotError::Io(e)),
        }
        let mut remembered = lock(&self.inner.remembered)?;
        if remembered.bytes.saturating_add(bytes.len()) <= REMEMBER_BYTES {
            remembered.bytes += bytes.len();
            remembered
                .files
                .insert(path.as_str().to_string(), Arc::clone(&bytes));
        }
        Ok(Some(bytes))
    }

    fn tree_rows(&self, tree: &Oid) -> Result<Vec<Row>> {
        let exec = &self.inner.exec;
        let work = &self.inner.root.work_tree;
        let records = exec.records(work, &["ls-tree", "-r", "-z", "-l", tree.as_str()])?;
        let mut rows: Vec<Row> = Vec::with_capacity(records.len());
        for record in &records {
            rows.push(parse_tree_record(record)?);
        }
        apply_floor(&mut rows);
        // The rule files of the tree, read through this reader's own door.
        let mut rule_files: Vec<(RepoPath, Vec<u8>)> = Vec::new();
        for row in &rows {
            let readable = row.decision == Decision::Admit
                && matches!(row.mode, Mode::Regular | Mode::Executable)
                && ignore::is_rule_file(&row.path);
            if let (true, Some(oid)) = (readable, &row.oid) {
                let bytes = self.read_object(&row.path, oid).inspect_err(|error| {
                    self.record_failure(&row.path, error);
                })?;
                rule_files.push((row.path.clone(), bytes.to_vec()));
            }
        }
        let paths: Vec<&str> = rows
            .iter()
            .filter(|row| row.decision == Decision::Admit)
            .map(|row| row.path.as_str())
            .collect();
        let hits = ignore::check(exec, &Basis::Tree { files: &rule_files }, &paths)?;
        self.decide_rows(&mut rows, &hits);
        Ok(rows)
    }

    fn working_rows(&self) -> Result<Vec<Row>> {
        let exec = &self.inner.exec;
        let work = &self.inner.root.work_tree;
        let records = exec.records(
            work,
            &[
                "ls-files",
                "-z",
                "-t",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
        )?;
        let mut rows: Vec<Row> = Vec::with_capacity(records.len());
        for record in &records {
            if record.len() < 3 {
                return Err(SnapshotError::Git(format!(
                    "git ls-files answered '{}'",
                    String::from_utf8_lossy(record)
                )));
            }
            let (tag, name) = (record[0], &record[2..]);
            // An untracked nested repository is one directory entry, not a
            // file of this repository.
            if name.ends_with(b"/") {
                continue;
            }
            let (path, addressable) = path_of(name);
            if !addressable {
                rows.push(Row {
                    path,
                    mode: Mode::Regular,
                    oid: None,
                    size: 0,
                    stamp: None,
                    tracked: tag != b'?',
                    decision: Decision::NotAnalysed(NotAnalysedReason::Unaddressable),
                });
                continue;
            }
            let Some((mode, stamp)) = disk_meta(&path.under(work))? else {
                continue;
            };
            rows.push(Row {
                path,
                mode,
                oid: None,
                size: stamp.len,
                stamp: Some(stamp),
                tracked: tag != b'?',
                decision: if addressable {
                    Decision::Admit
                } else {
                    Decision::NotAnalysed(NotAnalysedReason::Unaddressable)
                },
            });
        }
        apply_floor(&mut rows);
        let paths: Vec<&str> = rows
            .iter()
            .filter(|row| row.decision == Decision::Admit)
            .map(|row| row.path.as_str())
            .collect();
        let hits = ignore::check(exec, &Basis::WorkingTree { root: work }, &paths)?;
        self.decide_rows(&mut rows, &hits);
        Ok(rows)
    }

    /// Runs `decide` for every row that is still undecided (`Admit` is the
    /// placeholder the row builders leave).
    fn decide_rows(&self, rows: &mut [Row], hits: &FxHashMap<String, Hit>) {
        for row in &mut *rows {
            if row.decision != Decision::Admit {
                continue;
            }
            let ignored = hits.get(row.path.as_str()).and_then(|hit| {
                // A tracked file under a directory a rule excludes is not
                // ignored (#153); `.diffctx/ignore` always counts.
                (hit.source == IgnoreSource::DiffctxPolicy || !row.tracked || !hit.inherited)
                    .then_some(hit.source)
            });
            row.decision = self.inner.admission.decide(
                &row.path,
                &Meta {
                    mode: row.mode,
                    size: row.size,
                    tracked: row.tracked,
                    ignored,
                },
            );
        }
    }
}

/// A name as a path, and whether it is one: a name that is not UTF-8, or not
/// a path inside the repository, is kept for disclosure under a lossy
/// spelling and never read.
pub(crate) fn path_of(name: &[u8]) -> (RepoPath, bool) {
    match std::str::from_utf8(name).map(RepoPath::new) {
        Ok(Ok(path)) => (path, true),
        _ => (
            RepoPath::unchecked(String::from_utf8_lossy(name).into_owned()),
            false,
        ),
    }
}

/// `<mode> <type> <oid> <size>\t<path>`; the size is `-` for a submodule.
fn parse_tree_record(record: &[u8]) -> Result<Row> {
    let bad = || {
        SnapshotError::Git(format!(
            "git ls-tree answered '{}'",
            String::from_utf8_lossy(record)
        ))
    };
    let tab = record.iter().position(|b| *b == b'\t').ok_or_else(bad)?;
    let meta = String::from_utf8_lossy(&record[..tab]);
    let mut fields = meta.split_whitespace();
    let (Some(mode), Some(_kind), Some(oid)) = (fields.next(), fields.next(), fields.next()) else {
        return Err(bad());
    };
    let mode = Mode::from_git(mode).ok_or_else(bad)?;
    // A blob git cannot size (a damaged repository) reads as 0 here and fails
    // where it is read, by name.
    let size = fields.next().map_or(0, |s| s.parse().unwrap_or(0));
    let (path, addressable) = path_of(&record[tab + 1..]);
    Ok(Row {
        path,
        mode,
        oid: Some(Oid::parse(oid)?),
        size,
        stamp: None,
        tracked: true,
        decision: if addressable {
            Decision::Admit
        } else {
            Decision::NotAnalysed(NotAnalysedReason::Unaddressable)
        },
    })
}

/// The part of the decision that needs no lookup is made before the ignore
/// query, which then skips these paths.
fn apply_floor(rows: &mut [Row]) {
    for row in &mut *rows {
        if row.decision == Decision::Admit {
            if let Some(reason) = floor(&row.path) {
                row.decision = Decision::Withhold(reason);
            }
        }
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "the working-tree door: the one place that stats a tracked file"
)]
fn disk_meta(at: &Path) -> Result<Option<(Mode, Stamp)>> {
    let meta = match std::fs::symlink_metadata(at) {
        Ok(meta) => meta,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(SnapshotError::Io(e)),
    };
    let kind = meta.file_type();
    let stamp = stamp_of(&meta);
    Ok(if kind.is_symlink() {
        Some((Mode::Symlink, stamp))
    } else if kind.is_dir() {
        // A tracked directory is a submodule's checkout.
        Some((Mode::Submodule, Stamp { len: 0, ..stamp }))
    } else if kind.is_file() {
        let mode = if is_executable(&meta) {
            Mode::Executable
        } else {
            Mode::Regular
        };
        Some((mode, stamp))
    } else {
        None
    })
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

#[allow(
    clippy::disallowed_methods,
    reason = "the working-tree door: the one place that reads a link from the disk"
)]
fn link_text(at: &Path) -> Result<Vec<u8>> {
    let target = std::fs::read_link(at)?;
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(target.as_os_str().as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        Ok(target.to_string_lossy().replace('\\', "/").into_bytes())
    }
}

/// `text` resolved against `parent`, lexically; `None` when it is absolute or
/// climbs out of the repository.
fn join_inside(parent: &str, text: &str) -> Option<RepoPath> {
    if text.is_empty() || text.starts_with('/') {
        return None;
    }
    let mut stack: Vec<&str> = if parent.is_empty() {
        Vec::new()
    } else {
        parent.split('/').collect()
    };
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            name => stack.push(name),
        }
    }
    // A joined name that is no path is a link out of the snapshot: absent.
    #[allow(
        clippy::disallowed_methods,
        reason = "a malformed link target resolves to nothing, it is not a failed read"
    )]
    RepoPath::new(&stack.join("/")).ok()
}

/// The blob ids `git add` would store for `paths` (repo-relative), in order:
/// the repository's filters (line endings, LFS) applied, nothing written.
#[allow(
    clippy::disallowed_methods,
    reason = "feeds git the list of paths it hashes through a file it just wrote"
)]
fn hash_objects(exec: &Exec, work: &Path, paths: &[&str]) -> Result<Vec<Oid>> {
    let mut oids: Vec<Option<Oid>> = vec![None; paths.len()];
    // `--stdin-paths` is line-delimited: a name holding a newline would split.
    let (plain, odd): (Vec<usize>, Vec<usize>) =
        (0..paths.len()).partition(|i| !paths[*i].chars().any(char::is_control));
    if !plain.is_empty() {
        let scratch = Scratch::new()?;
        for chunk in plain.chunks(5000) {
            let list = scratch.path().join("paths");
            let mut text = String::new();
            for i in chunk {
                text.push_str(paths[*i]);
                text.push('\n');
            }
            std::fs::write(&list, text)?;
            let mut cmd = git_command(work);
            cmd.args(["hash-object", "--stdin-paths"])
                .stdin(std::fs::File::open(&list)?);
            let out = exec.finish(cmd, work, &["hash-object"], &[])?;
            let answer = String::from_utf8_lossy(&out.stdout);
            let lines: Vec<&str> = answer.lines().collect();
            if lines.len() != chunk.len() {
                return Err(SnapshotError::Git("git hash-object: short answer".into()));
            }
            for (i, line) in chunk.iter().zip(lines) {
                oids[*i] = Some(Oid::parse(line)?);
            }
        }
    }
    for i in odd {
        let out = exec.text(work, &["hash-object", "--", paths[i]])?;
        oids[i] = Some(Oid::parse(&out)?);
    }
    oids.into_iter()
        .map(|oid| oid.ok_or_else(|| SnapshotError::Git("git hash-object: short answer".into())))
        .collect()
}

/// The blob ids `git add` would store for these paths of the working tree.
///
/// In order: a dirty set's content identity without reading it back. The paths
/// are those of regular files; a path that is not one is an error.
pub fn hash_working_paths(root: &RepoRoot, paths: &[RepoPath]) -> Result<Vec<Oid>> {
    let names: Vec<&str> = paths.iter().map(RepoPath::as_str).collect();
    hash_objects(&Exec::ambient(), &root.work_tree, &names)
}

/// A digest of what the working tree holds beyond `HEAD`: the tree of `HEAD`
/// and the content of every path `git status` reports. Cheap, and it does not
/// change when nothing the analysis could read changed.
fn working_tree_id(exec: &Exec, root: &RepoRoot) -> Result<ContentId> {
    let work = &root.work_tree;
    let head = rev_to_oid(exec, work, "HEAD^{tree}")?;
    let records = exec.records(
        work,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ],
    )?;
    let mut names: Vec<String> = records
        .iter()
        .filter(|record| record.len() > 3)
        .map(|record| String::from_utf8_lossy(&record[3..]).into_owned())
        .collect();
    names.sort();
    names.dedup();

    let mut marks: Vec<Vec<u8>> = vec![Vec::new(); names.len()];
    let mut regular: Vec<usize> = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let at = work.join(name);
        match disk_meta(&at)? {
            Some((Mode::Regular | Mode::Executable, _)) => regular.push(i),
            Some((Mode::Symlink, _)) => {
                let text = link_text(&at)?;
                marks[i] = digest(&[b"L".as_slice(), text.as_slice()]).into_bytes();
            }
            Some((Mode::Submodule, _)) => marks[i] = b"D".to_vec(),
            None => marks[i] = b"-".to_vec(),
        }
    }
    let hashed = hash_objects(
        exec,
        work,
        &regular
            .iter()
            .map(|i| names[*i].as_str())
            .collect::<Vec<_>>(),
    )?;
    for (i, oid) in regular.iter().zip(hashed) {
        marks[*i] = oid.to_string().into_bytes();
    }
    let head = head.map_or_else(|| "unborn".to_string(), |oid| oid.to_string());
    let mut parts: Vec<&[u8]> = vec![head.as_bytes()];
    for (name, mark) in names.iter().zip(&marks) {
        parts.push(name.as_bytes());
        parts.push(mark);
    }
    Ok(ContentId(format!("wt:{}", digest(&parts))))
}
