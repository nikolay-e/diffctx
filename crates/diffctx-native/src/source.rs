//! The engine's one door onto the snapshot layer. A run analyses an
//! [`Analysis`] — the `AnalysisContext` its door built once from the range —
//! and every byte of content it reads, on the current side or the base one,
//! comes out of that context's readers. There is no ambient default: a source
//! that was never given a context (the in-memory corpus harness) is
//! [`Source::empty`] and finds no file, so a read can never answer for a state
//! nobody asked about (#354, #511).
//!
//! This is the only module that names `crate::snapshot`: the engine speaks
//! repo-relative strings and absolute paths, and the snapshot module's types stop here.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use crate::snapshot as snap;

use crate::git::{self, GitError};
use crate::resource::{LimitReason, RunNotes};

/// The failure of a snapshot call as the engine already names git failures,
/// so an interruption stays an interruption and a usage error keeps its exit
/// code.
fn lift(e: snap::SnapshotError) -> GitError {
    use snap::SnapshotError as E;
    match e {
        E::Cancelled => GitError::Cancelled,
        E::Timeout(secs) => GitError::Timeout(secs),
        E::NotARepo(path) => GitError::NotARepo(path),
        E::InvalidRange(m) => GitError::InvalidRange(m),
        E::InvalidDuration(m) => GitError::InvalidDuration(m),
        E::InvalidPath(m) => GitError::InvalidPath(m),
        E::Io(e) => GitError::Io(e),
        E::TooLarge { path, size } => GitError::BlobTooLarge {
            size: usize::try_from(size).unwrap_or(usize::MAX),
            spec: path,
        },
        other => GitError::CommandFailed(other.to_string()),
    }
}

/// Why a path is not shown, as far as the engine tells the reasons apart.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Why {
    /// Secret by name, or inside git's own directory: counted, never named.
    Secret,
    /// `.diffctx/ignore`, a declared confidentiality policy: counted, never named.
    Policy,
    /// `.gitignore`: mundane, listed by path (#188).
    Gitignore,
}

/// The secret-path floor, which needs no repository: private-key, keystore
/// and credential files never reach LLM-bound output on any surface (#227).
pub fn is_secret_path(path: &Path) -> bool {
    snap::is_secret_path(path)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Admitted {
    Yes,
    Withheld(Why),
}

/// One blob of a listing, by absolute path.
pub struct ListedBlob {
    pub path: PathBuf,
    pub oid: Option<String>,
    pub size: u64,
}

/// What a side holds: the admitted blobs, the files it holds but did not
/// analyse for being over the size cap, and how many it withheld.
///
/// The snapshot crate's `Listing` seen the way the engine addresses files, by
/// absolute path, with the over-size and symlink entries split out; it holds
/// nothing the reader's does not. It goes when the engine addresses files by
/// `RepoPath` (the facts layer, waves 3-5).
pub struct Listing {
    pub blobs: Vec<ListedBlob>,
    pub too_large: Vec<PathBuf>,
    /// Symlinks, which hold a target's name and are followed only inside the
    /// snapshot (`Source::link_target`): files an import may name.
    pub links: Vec<PathBuf>,
    pub withheld: usize,
}

impl Listing {
    /// The blobs under `scope` (repo-relative pathspecs; empty is the whole
    /// repository).
    pub fn within<'a>(
        &'a self,
        root: &'a Path,
        scope: &'a [String],
    ) -> impl Iterator<Item = &'a ListedBlob> + 'a {
        self.blobs
            .iter()
            .filter(move |b| in_scope(root, &b.path, scope))
    }
}

pub(crate) fn in_scope(root: &Path, path: &Path, scope: &[String]) -> bool {
    scope.is_empty()
        || crate::paths::display_rel(root, path).is_some_and(|rel| {
            scope.iter().any(|s| {
                let s = s.trim_end_matches('/');
                rel == s || rel.strip_prefix(s).is_some_and(|r| r.starts_with('/'))
            })
        })
}

/// The staged index is a tree like any other once captured: the range runs
/// from `base` to that tree and nothing reads the index again.
fn index_as_tree(repo: &snap::RepoRoot, range: snap::RangeSpec) -> git::Result<snap::RangeSpec> {
    match range {
        snap::RangeSpec::Staged { base } => Ok(snap::RangeSpec::Review {
            base,
            head: snap::index_tree(repo).map_err(lift)?,
        }),
        other => Ok(other),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Head,
    Base,
}

/// One analysis, built once by the door that received the question.
pub struct Analysis {
    ctx: snap::AnalysisContext,
    root: PathBuf,
    /// The range as `git diff` is asked it: both ends as object ids for a
    /// two-sided range, the base for a window or a revision against the
    /// working tree, `None` for no range at all.
    range: Option<String>,
    /// A tree-ended range (the staged index, or `A..<tree>`): content and no
    /// history.
    staged: Option<git::StagedSnapshot>,
    working_tree_diff: bool,
    /// The revision `git grep` reads for the head side; `None` for the
    /// working tree.
    head_revision: Option<String>,
    listings: [OnceLock<Arc<Listing>>; 2],
    notes: OnceLock<RunNotes>,
}

impl Analysis {
    /// The analysis of a diff query: `spec` is the caller's range, read once
    /// by the snapshot layer. The staged index is captured here, once, as a
    /// tree, and every current-side byte of the run comes from that tree
    /// whatever the disk or a later `git add` says.
    pub fn open(
        root_dir: &Path,
        spec: Option<&str>,
        run: &crate::resource::RunContext,
    ) -> git::Result<Arc<Self>> {
        let repo = snap::open(root_dir).map_err(lift)?;
        let spec = spec.map(str::trim);
        let range = snap::resolve(&repo, spec.unwrap_or("HEAD")).map_err(lift)?;
        let range = index_as_tree(&repo, range)?;
        Self::build(root_dir, repo, range, spec, run)
    }

    /// The analysis of a range its caller already holds as a value: a push
    /// or a force-push the hook read off git's record reaches the engine
    /// as the two ids it measured, never as text to be read again.
    pub fn open_range(
        root_dir: &Path,
        range: snap::RangeSpec,
        run: &crate::resource::RunContext,
    ) -> git::Result<Arc<Self>> {
        let repo = snap::open(root_dir).map_err(lift)?;
        let base_text = match &range {
            snap::RangeSpec::WorkingTree { base } => Some(base.to_string()),
            _ => None,
        };
        let range = index_as_tree(&repo, range)?;
        Self::build(root_dir, repo, range, base_text.as_deref(), run)
    }

    /// The analysis of one snapshot, not of a change: `--symbol`, the project
    /// graph, a question about the checkout as it is.
    pub fn at_working_tree(
        root_dir: &Path,
        run: &crate::resource::RunContext,
    ) -> git::Result<Arc<Self>> {
        let repo = snap::open(root_dir).map_err(lift)?;
        let range = snap::RangeSpec::At(snap::Side::WorkingTree);
        Self::build(root_dir, repo, range, None, run)
    }

    fn build(
        root_dir: &Path,
        repo: snap::RepoRoot,
        range: snap::RangeSpec,
        spec: Option<&str>,
        run: &crate::resource::RunContext,
    ) -> git::Result<Arc<Self>> {
        let (range_text, staged, working_tree_diff, head_revision) = match &range {
            snap::RangeSpec::Rewritten { old, new } => {
                // A force-push changed the remote by the delta between two
                // tips that share no history: the new tip stands as its
                // tree, and a range name would read `merge-base..new`
                // (gh#596).
                let tree = git::run_git(
                    root_dir,
                    &["rev-parse", "--verify", &format!("{new}^{{tree}}")],
                )?
                .trim()
                .to_string();
                let base = old.to_string();
                let staged = git::StagedSnapshot {
                    head: git::rev_oid(root_dir, &base),
                    tree: tree.clone(),
                    base: base.clone(),
                };
                (
                    Some(format!("{base}..{tree}")),
                    Some(staged),
                    false,
                    Some(tree),
                )
            }
            snap::RangeSpec::Review { base, head }
            | snap::RangeSpec::Pushed {
                old: base,
                new: head,
            } => {
                let (base, head) = (base.to_string(), head.to_string());
                let text = format!("{base}..{head}");
                if git::rev_oid(root_dir, &head).is_some() {
                    (Some(text), None, false, Some(head))
                } else {
                    let staged = git::StagedSnapshot {
                        head: git::rev_oid(root_dir, &base),
                        base,
                        tree: head.clone(),
                    };
                    (Some(text), Some(staged), false, Some(head))
                }
            }
            snap::RangeSpec::WorkingTree { .. } => {
                let text = match spec {
                    Some(spec) => Some(
                        git::duration_base(root_dir, spec)?.unwrap_or_else(|| spec.to_string()),
                    ),
                    None => None,
                };
                (text, None, true, None)
            }
            snap::RangeSpec::Staged { .. } | snap::RangeSpec::At(_) => (None, None, true, None),
        };
        let (deadline, cancel) = run.io_limits();
        let budget = snap::RunBudget {
            deadline,
            cancel: Some(cancel),
            read_fault: crate::resource::test_read_fault(),
        };
        let ctx = snap::AnalysisContext::new(repo, range, budget).map_err(lift)?;
        Ok(Arc::new(Self {
            ctx,
            root: root_dir.to_path_buf(),
            range: range_text,
            staged,
            working_tree_diff,
            head_revision,
            listings: [OnceLock::new(), OnceLock::new()],
            notes: OnceLock::new(),
        }))
    }

    pub fn range(&self) -> Option<&str> {
        self.range.as_deref()
    }

    pub fn staged(&self) -> Option<&git::StagedSnapshot> {
        self.staged.as_ref()
    }

    /// Whether the change's head is the checkout itself, which is where
    /// untracked files belong in the change set.
    pub fn is_working_tree_diff(&self) -> bool {
        self.working_tree_diff
    }

    /// The current side.
    pub fn head(self: &Arc<Self>) -> Source {
        Source::over(self, End::Head)
    }

    pub(crate) fn report_to(&self, notes: RunNotes) {
        let _ = self.notes.set(notes);
    }

    fn reader(&self, end: End) -> &snap::SnapshotReader {
        match end {
            End::Head => &self.ctx.head,
            End::Base => &self.ctx.base,
        }
    }

    fn repo_path(&self, abs: &Path) -> Option<snap::RepoPath> {
        let rel = crate::paths::display_rel(&self.root, abs)?;
        snap::RepoPath::new(&rel).ok()
    }

    fn note(&self, reason: LimitReason) {
        if let Some(notes) = self.notes.get() {
            notes.note(reason);
        }
    }

    fn listing(&self, end: End) -> git::Result<Arc<Listing>> {
        let cell = &self.listings[usize::from(end == End::Base)];
        if let Some(listing) = cell.get() {
            return Ok(listing.clone());
        }
        let listed = self.reader(end).list().map_err(lift)?;
        let blob = |p: &snap::RepoPath| crate::paths::repo_join(&self.root, &p.to_string());
        let listing = Arc::new(Listing {
            blobs: listed
                .entries
                .iter()
                .map(|e| ListedBlob {
                    path: blob(&e.path),
                    oid: e.oid.as_ref().map(ToString::to_string),
                    size: e.size,
                })
                .collect(),
            too_large: listed
                .not_analysed
                .iter()
                .filter(|(_, why)| matches!(why, snap::NotAnalysedReason::Oversize))
                .map(|(p, _)| blob(p))
                .collect(),
            links: listed
                .not_analysed
                .iter()
                .filter(|(_, why)| matches!(why, snap::NotAnalysedReason::Symlink))
                .map(|(p, _)| blob(p))
                .collect(),
            withheld: listed.withheld,
        });
        let _ = cell.set(listing.clone());
        Ok(listing)
    }
}

/// A view of one side of an [`Analysis`], addressed by absolute path as the
/// engine addresses files. Cheap to clone.
#[derive(Clone)]
pub struct Source {
    view: Option<(Arc<Analysis>, End)>,
}

impl Source {
    /// A source of nothing: every read finds no file. For runs that analyse
    /// content already in memory.
    pub fn empty() -> Self {
        Self { view: None }
    }

    fn over(analysis: &Arc<Analysis>, end: End) -> Self {
        Self {
            view: Some((analysis.clone(), end)),
        }
    }

    /// Whether a read of either side failed, however the reader's caller took
    /// the error: a failure swallowed on the way is still a loss of the run.
    pub fn lost_a_read(&self) -> bool {
        self.view.as_ref().is_some_and(|(analysis, _)| {
            analysis.ctx.head.has_losses() || analysis.ctx.base.has_losses()
        })
    }

    /// The side the change started from.
    pub fn base(&self) -> Self {
        match &self.view {
            Some((analysis, _)) => Self::over(analysis, End::Base),
            None => Self::empty(),
        }
    }

    /// Where a file the head side lacks is looked for next: the base side of
    /// a two-sided range. A working-tree head has none, and a snapshot's file
    /// absent from it is not in the snapshot.
    pub fn fallback(&self) -> Option<Self> {
        match &self.view {
            Some((analysis, End::Head))
                if !analysis.working_tree_diff && analysis.range.is_some() =>
            {
                Some(self.base())
            }
            _ => None,
        }
    }

    /// The revision `git grep` reads for this side, `None` for the working
    /// tree.
    pub fn revision(&self) -> Option<&str> {
        match &self.view {
            Some((analysis, End::Head)) => analysis.head_revision.as_deref(),
            _ => None,
        }
    }

    pub(crate) fn report_to(&self, notes: RunNotes) {
        if let Some((analysis, _)) = &self.view {
            analysis.report_to(notes);
        }
    }

    fn note(&self, reason: LimitReason) {
        if let Some((analysis, _)) = &self.view {
            analysis.note(reason);
        }
    }

    /// The file's bytes: `Ok(None)` when the side has no such file, an error
    /// when the read failed — out of time, cancelled, too large, a broken
    /// pipe. Taken for an absent file, a failed read dropped imports and
    /// callers with no word in the answer (#408).
    pub fn read(&self, abs_path: &Path) -> git::Result<Option<Vec<u8>>> {
        let Some((analysis, end)) = &self.view else {
            return Ok(None);
        };
        let Some(path) = analysis.repo_path(abs_path) else {
            return Ok(None);
        };
        analysis
            .reader(*end)
            .read(&path)
            .map(|bytes| bytes.map(|b| b[..].to_vec()))
            .map_err(lift)
    }

    /// `read`, with a failure noted on the run and read as no file.
    pub fn read_bytes(&self, abs_path: &Path) -> Option<Vec<u8>> {
        match self.read(abs_path) {
            Ok(bytes) => bytes,
            Err(e) => {
                self.note(crate::resource::reason_for(&e));
                None
            }
        }
    }

    /// The file as text with invalid sequences replaced, for the readers
    /// that parse a side and would rather see mojibake than nothing:
    /// `Ok(None)` when the side has no such file.
    pub fn read_lossy(&self, abs_path: &Path) -> git::Result<Option<String>> {
        Ok(self
            .read(abs_path)?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    pub fn read_to_string(&self, abs_path: &Path) -> Option<String> {
        String::from_utf8(self.read_bytes(abs_path)?).ok()
    }

    /// Where an import of `rel` lands when `rel` is a symlink of this side:
    /// Node and Vite load a module by its real path (#372). The snapshot
    /// layer follows the link inside the snapshot only; `None` when `rel` is
    /// no link, or the chain leaves the snapshot or ends at no file.
    pub fn link_target(&self, rel: &str) -> Option<String> {
        let (analysis, end) = self.view.as_ref()?;
        let path = snap::RepoPath::new(rel).ok()?;
        match analysis.reader(*end).resolve_link(&path) {
            Ok(target) => target.map(|t| t.to_string()),
            Err(e) => {
                self.note(crate::resource::reason_for(&lift(e)));
                None
            }
        }
    }

    /// Every admitted blob of this side, once per run.
    pub fn list(&self) -> git::Result<Arc<Listing>> {
        match &self.view {
            Some((analysis, end)) => analysis.listing(*end),
            None => Ok(Arc::new(Listing {
                blobs: Vec::new(),
                too_large: Vec::new(),
                links: Vec::new(),
                withheld: 0,
            })),
        }
    }

    /// Every file of this side, repo-relative, the over-sized ones included:
    /// a failed listing is an error, never an empty side (#443).
    pub fn list_paths(&self, root: &Path) -> git::Result<Vec<String>> {
        let listing = self.list()?;
        Ok(listing
            .blobs
            .iter()
            .map(|b| &b.path)
            .chain(&listing.too_large)
            .chain(&listing.links)
            .filter_map(|p| crate::paths::display_rel(root, p))
            .collect())
    }

    /// What the snapshot layer decided for `rel` on this side, `None` when
    /// the side does not hold the path.
    fn decision(&self, rel: &str) -> Option<snap::Decision> {
        let (analysis, end) = self.view.as_ref()?;
        let path = snap::RepoPath::new(rel).ok()?;
        match analysis.reader(*end).decision(&path) {
            Ok(decision) => decision,
            // A decision that could not be made is a withheld path, and the
            // run says it could not read.
            Err(e) => {
                self.note(crate::resource::reason_for(&lift(e)));
                Some(snap::Decision::Withhold(
                    snap::WithholdReason::DiffctxPolicy,
                ))
            }
        }
    }

    /// Whether the path may be shown, and when not why: secret by name, git's
    /// own directory, `.gitignore` and `.diffctx/ignore`, as the snapshot
    /// layer's admission decided them for this side, else for the base side
    /// (a deleted path), else by name alone.
    pub fn admitted(&self, rel: &str) -> Admitted {
        let Some((analysis, _)) = &self.view else {
            return Admitted::Yes;
        };
        let held = self
            .decision(rel)
            .or_else(|| Self::over(analysis, End::Base).decision(rel));
        match held {
            Some(snap::Decision::Withhold(why)) => Admitted::Withheld(match why {
                snap::WithholdReason::Secret | snap::WithholdReason::RepoInternal => Why::Secret,
                snap::WithholdReason::DiffctxPolicy => Why::Policy,
                snap::WithholdReason::Gitignore => Why::Gitignore,
            }),
            Some(snap::Decision::Admit | snap::Decision::NotAnalysed(_)) => Admitted::Yes,
            None if is_secret_path(Path::new(rel)) || rel.split('/').any(|p| p == ".git") => {
                Admitted::Withheld(Why::Secret)
            }
            None => Admitted::Yes,
        }
    }

    /// Which of `rels` this side withholds whether it lists them or not: the
    /// question of a surface that serves paths of its own choosing (the MCP
    /// fetch). A path that cannot be asked about is withheld.
    pub fn withheld_among(&self, rels: &[String]) -> Vec<String> {
        let Some((analysis, end)) = &self.view else {
            return Vec::new();
        };
        let paths: Vec<snap::RepoPath> = rels
            .iter()
            .filter_map(|rel| snap::RepoPath::new(rel).ok())
            .collect();
        // A name that is no repository path is not one to serve.
        let mut withheld: Vec<String> = rels
            .iter()
            .filter(|rel| snap::RepoPath::new(rel).is_err())
            .cloned()
            .collect();
        match analysis.reader(*end).withheld_among(&paths) {
            Ok(found) => withheld.extend(found.iter().map(ToString::to_string)),
            Err(e) => {
                self.note(crate::resource::reason_for(&lift(e)));
                return rels.to_vec();
            }
        }
        withheld
    }

    fn workspace(&self) -> Option<&snap::WorkspaceModel> {
        self.view
            .as_ref()
            .map(|(analysis, _)| &analysis.ctx.workspace)
    }

    /// Whether code at `user` can reference code at `used` (repo-relative):
    /// the same project, one holding the other, or a declared dependency. A
    /// source with no workspace model rules nothing out.
    pub fn may_reference(&self, user: &str, used: &str) -> bool {
        self.workspace().is_none_or(|w| w.may_reference(user, used))
    }

    /// Whether the two files sit in one package of the workspace.
    pub fn same_package(&self, a: &str, b: &str) -> bool {
        self.workspace().is_some_and(|w| w.same_package(a, b))
    }

    /// Which of `rels` this side withholds.
    pub fn withheld(&self, rels: &[String]) -> Vec<String> {
        rels.iter()
            .filter(|rel| self.admitted(rel) != Admitted::Yes)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = git::git_command(dir).args(args).status().expect("run git");
        assert!(status.success(), "git {args:?} failed");
    }

    fn repo_with_dirty_file() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        git(root, &["config", "user.email", "t@example.com"]);
        git(root, &["config", "user.name", "T"]);
        git(root, &["config", "commit.gpgsign", "false"]);
        std::fs::write(root.join("a.py"), "x = 1\n").expect("write");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "one"]);
        std::fs::write(root.join("a.py"), "x = 2\n").expect("write");
        tmp
    }

    /// No range at all is what it always was: the checkout against `HEAD`,
    /// asked of git as a bare `git diff` (range `None`), untracked files in
    /// the change, the head read from the disk and the base from `HEAD`.
    #[test]
    fn no_range_keeps_its_old_meaning() {
        let tmp = repo_with_dirty_file();
        let root = dunce::canonicalize(tmp.path()).expect("canonical");
        let run = crate::resource::RunContext::unbounded();
        let bare = Analysis::open(&root, None, &run).expect("open");
        let head = Analysis::open(&root, Some("HEAD"), &run).expect("open");

        assert_eq!(bare.range(), None);
        assert_eq!(head.range(), Some("HEAD"));
        for analysis in [&bare, &head] {
            assert!(analysis.is_working_tree_diff());
            assert!(analysis.staged().is_none());
            let file = root.join("a.py");
            assert_eq!(
                analysis.head().read_to_string(&file).as_deref(),
                Some("x = 2\n")
            );
            assert_eq!(
                analysis.head().base().read_to_string(&file).as_deref(),
                Some("x = 1\n")
            );
            assert!(analysis.head().fallback().is_none());
        }
    }

    fn oid_of(root: &Path, rev: &str) -> snap::Oid {
        let out = git::run_git(root, &["rev-parse", rev]).expect("rev-parse");
        snap::Oid::parse(out.trim()).expect("object id")
    }

    fn commit(root: &Path, text: &str, message: &str) {
        std::fs::write(root.join("a.py"), text).expect("write");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", message]);
    }

    /// A push the caller measured reaches the engine as its two ids and is
    /// read exactly as the same range written as text is.
    #[test]
    fn a_pushed_range_opens_as_a_value() {
        let tmp = repo_with_dirty_file();
        let root = dunce::canonicalize(tmp.path()).expect("canonical");
        commit(&root, "x = 2\n", "two");
        let (old, new) = (oid_of(&root, "HEAD~1"), oid_of(&root, "HEAD"));
        let run = crate::resource::RunContext::unbounded();
        let text = format!("{old}..{new}");
        let by_text = Analysis::open(&root, Some(&text), &run).expect("open");
        let by_value = Analysis::open_range(
            &root,
            snap::RangeSpec::Pushed {
                old: old.clone(),
                new: new.clone(),
            },
            &run,
        )
        .expect("open_range");

        assert_eq!(by_value.range(), Some(text.as_str()));
        assert_eq!(by_value.range(), by_text.range());
        assert!(by_value.staged().is_none());
        assert!(!by_value.is_working_tree_diff());
        let file = root.join("a.py");
        assert_eq!(
            by_value.head().read_to_string(&file).as_deref(),
            Some("x = 2\n")
        );
        assert_eq!(
            by_value.head().base().read_to_string(&file).as_deref(),
            Some("x = 1\n")
        );
    }

    /// A force-push is the tree delta between two tips with no history in
    /// common: the new tip stands as its tree, whatever its commit's
    /// ancestry, and a push value that does not descend is refused.
    #[test]
    fn a_rewritten_range_opens_as_the_tree_delta_between_the_tips() {
        let tmp = repo_with_dirty_file();
        let root = dunce::canonicalize(tmp.path()).expect("canonical");
        commit(&root, "x = 2\n", "two");
        let old = oid_of(&root, "HEAD");
        git(&root, &["checkout", "-q", "-b", "rewritten", "HEAD~1"]);
        commit(&root, "x = 3\n", "three");
        let new = oid_of(&root, "HEAD");
        let tree = oid_of(&root, "HEAD^{tree}");
        let run = crate::resource::RunContext::unbounded();

        let analysis = Analysis::open_range(
            &root,
            snap::RangeSpec::Rewritten {
                old: old.clone(),
                new: new.clone(),
            },
            &run,
        )
        .expect("open_range");
        let named = format!("{old}..{tree}");
        assert_eq!(analysis.range(), Some(named.as_str()));
        let staged = analysis.staged().expect("a tree-ended range");
        assert_eq!(staged.tree, tree.to_string());
        assert_eq!(staged.base, old.to_string());
        assert_eq!(staged.head.as_deref(), Some(old.to_string().as_str()));
        let file = root.join("a.py");
        assert_eq!(
            analysis.head().read_to_string(&file).as_deref(),
            Some("x = 3\n")
        );
        assert_eq!(
            analysis.head().base().read_to_string(&file).as_deref(),
            Some("x = 2\n")
        );

        let refused = Analysis::open_range(&root, snap::RangeSpec::Pushed { old, new }, &run);
        assert!(
            refused.is_err(),
            "the tips do not descend one from the other"
        );
    }
}
