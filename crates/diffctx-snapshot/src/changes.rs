//! What a range changes, as git states it between the two snapshots' trees:
//! paths, rename and copy pairs with their similarity, modes (a symlink or a
//! submodule among them), each carrying admission's verdict. A withheld path
//! takes part in the delta (something changed) and is only counted.

use crate::admission::{Decision, Meta, NotAnalysedReason, WithholdReason};
use crate::error::{Result, SnapshotError};
use crate::git::git_command;
use crate::reader::{SnapshotReader, path_of};
use crate::types::{Mode, RepoPath};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Change {
    pub kind: ChangeKind,
    /// The path after the change (before it, for a deletion).
    pub path: RepoPath,
    /// The source of a rename or copy.
    pub from: Option<RepoPath>,
    /// Percent, for a rename or copy.
    pub similarity: Option<u8>,
    pub old_mode: Option<Mode>,
    pub new_mode: Option<Mode>,
    pub decision: Decision,
}

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ChangeSet {
    /// Sorted by path; never a withheld path.
    pub changes: Vec<Change>,
    /// Changed paths admission withheld, by count only: the secret floor and
    /// `.diffctx/ignore`.
    pub withheld: usize,
    /// Changed paths that only git's own ignore rules exclude. Mundane, so
    /// they keep their names (#188).
    pub gitignored: Vec<RepoPath>,
}

impl ChangeSet {
    /// The changes to symlinks: links are not followed, so a retargeted one
    /// is reported as itself.
    pub fn symlinks(&self) -> impl Iterator<Item = &Change> {
        self.changes
            .iter()
            .filter(|c| c.new_mode == Some(Mode::Symlink) || c.old_mode == Some(Mode::Symlink))
    }
}

/// The delta from `base` to `head`. Two trees are compared object to object;
/// a working-tree head is compared with git's own diff of the base tree, and
/// its admitted untracked files are added.
pub(crate) fn compute(base: &SnapshotReader, head: &SnapshotReader) -> Result<ChangeSet> {
    let Some(base_tree) = base.tree() else {
        return Err(SnapshotError::Range(
            "the base of a range is a tree, never the working tree".into(),
        ));
    };
    let exec = head.exec();
    let work = &head.root().work_tree;
    let mut cmd = git_command(work);
    let label: &[&str] = if let Some(head_tree) = head.tree() {
        cmd.args(["diff-tree", "-r", "-M", "--raw", "-z", "--no-commit-id"])
            .arg(base_tree.as_str())
            .arg(head_tree.as_str());
        &["diff-tree"]
    } else {
        cmd.args([
            "diff",
            "--raw",
            "-z",
            "-M",
            "--no-textconv",
            "--no-ext-diff",
            "--no-color",
        ])
        .arg(base_tree.as_str());
        &["diff"]
    };
    let raw = exec.finish(cmd, work, label, &[])?.stdout;

    let mut set = ChangeSet::default();
    for entry in parse_raw(&raw)? {
        let (path, path_ok) = path_of(&entry.path);
        let (from, from_ok) = match &entry.from {
            Some(name) => {
                let (from, ok) = path_of(name);
                (Some(from), ok)
            }
            None => (None, true),
        };
        let mut decision = if entry.kind == ChangeKind::Deleted {
            verdict(base, &path, entry.old_mode)?
        } else {
            verdict(head, &path, entry.new_mode)?
        };
        if let Some(from) = &from {
            if let Decision::Withhold(reason) = verdict(base, from, entry.old_mode)? {
                decision = Decision::Withhold(reason);
            }
        }
        if !(path_ok && from_ok) {
            decision = Decision::NotAnalysed(NotAnalysedReason::Unaddressable);
        }
        push(
            &mut set,
            Change {
                kind: entry.kind,
                path,
                from,
                similarity: entry.similarity,
                old_mode: entry.old_mode,
                new_mode: entry.new_mode,
                decision,
            },
        );
    }
    if head.tree().is_none() {
        for row in &head.table()?.rows {
            if !row.tracked {
                push(
                    &mut set,
                    Change {
                        kind: ChangeKind::Added,
                        path: row.path.clone(),
                        from: None,
                        similarity: None,
                        old_mode: None,
                        new_mode: Some(row.mode),
                        decision: row.decision,
                    },
                );
            }
        }
    }
    set.changes.sort_by(|a, b| a.path.cmp(&b.path));
    set.gitignored.sort();
    Ok(set)
}

fn push(set: &mut ChangeSet, change: Change) {
    if matches!(
        change.decision,
        Decision::Withhold(WithholdReason::Gitignore)
    ) {
        set.gitignored.push(change.path);
    } else if matches!(change.decision, Decision::Withhold(_)) {
        set.withheld += 1;
    } else {
        set.changes.push(change);
    }
}

/// The snapshot's own verdict for a path it holds; for one it does not (a
/// path the delta removed from this side) the part of admission that needs no
/// lookup, so a secret never slips through by being absent from a listing.
fn verdict(reader: &SnapshotReader, path: &RepoPath, mode: Option<Mode>) -> Result<Decision> {
    match reader.decision(path)? {
        Some(decision) => Ok(decision),
        None => Ok(reader.admission().decide(
            path,
            &Meta {
                mode: mode.unwrap_or(Mode::Regular),
                size: 0,
                tracked: true,
                ignored: None,
            },
        )),
    }
}

struct Raw {
    kind: ChangeKind,
    path: Vec<u8>,
    from: Option<Vec<u8>>,
    similarity: Option<u8>,
    old_mode: Option<Mode>,
    new_mode: Option<Mode>,
}

/// `:<old mode> <new mode> <old oid> <new oid> <status>\0<path>\0` (two paths
/// for a rename or copy), as `--raw -z` prints it.
fn parse_raw(raw: &[u8]) -> Result<Vec<Raw>> {
    let bad = |what: &str| SnapshotError::Git(format!("git diff answered {what}"));
    let tokens: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < tokens.len() {
        let token = tokens[at];
        at += 1;
        if token.is_empty() {
            continue;
        }
        let Some(rest) = token.strip_prefix(b":") else {
            return Err(bad("a record without its mode line"));
        };
        let Ok(meta) = std::str::from_utf8(rest) else {
            return Err(bad("a mode line that is not text"));
        };
        let mut fields = meta.split(' ');
        let (Some(old), Some(new), Some(_), Some(_), Some(status)) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        ) else {
            return Err(bad("a short mode line"));
        };
        let paired = status.starts_with(['R', 'C']);
        let wanted = if paired { 2 } else { 1 };
        let Some(names) = tokens.get(at..at + wanted) else {
            return Err(bad("a record without its path"));
        };
        at += wanted;
        let kind = match status.as_bytes().first() {
            Some(b'A') => ChangeKind::Added,
            Some(b'M' | b'U') => ChangeKind::Modified,
            Some(b'D') => ChangeKind::Deleted,
            Some(b'R') => ChangeKind::Renamed,
            Some(b'C') => ChangeKind::Copied,
            Some(b'T') => ChangeKind::TypeChanged,
            _ => return Err(bad(&format!("the status '{status}'"))),
        };
        let (from, path) = if paired {
            (Some(names[0].to_vec()), names[1].to_vec())
        } else {
            (None, names[0].to_vec())
        };
        found.push(Raw {
            kind,
            path,
            from,
            similarity: paired.then(|| status[1..].parse().unwrap_or(0)),
            old_mode: Mode::from_git(old),
            new_mode: Mode::from_git(new),
        });
    }
    Ok(found)
}
