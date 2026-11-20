//! The untracked part of a pending change, read collapsed. A checkout can hold
//! a million untracked files, and git walks every one of them to list them
//! one by one (26 s on the checkout that made fj#170) although the question
//! asked is only which of them a change could be about. An untracked directory
//! is one entry (`--directory`), expanded one level at a time, breadth first,
//! while a budget of entries lasts; a directory the budget leaves is named in
//! [`Found::unread`], never dropped.

use std::path::Path;

use crate::snapshot::error::Result;
use crate::snapshot::git::Exec;
use crate::snapshot::types::RepoPath;

/// Entries read below the top level of an untracked listing. A directory
/// whose own entries do not fit in what is left stays unread.
const ENTRY_BUDGET: usize = 5_000;

/// What a `.diffctx` directory may hold beyond the budget: its policy file
/// decides what the rest of the read is shown, so it is read first and free.
const POLICY_ROOM: usize = 100;

/// Pathspecs per `ls-files` call: an argv has a size limit.
const CHUNK: usize = 500;

/// What a collapsed read found. `names` are the untracked files, exactly as
/// git names them, and a nested repository as one `dir/` entry; `unread` are
/// the directories (no trailing slash) whose content the budget did not reach.
pub(crate) struct Found {
    pub names: Vec<Vec<u8>>,
    pub unread: Vec<String>,
}

/// [`Found`] with names as text, for a caller outside the snapshot layer.
pub struct Untracked {
    pub names: Vec<String>,
    pub unread: Vec<String>,
}

/// The untracked, unignored files under `pathspec` (all of the checkout when
/// empty), read under the limits of the run on this thread.
pub fn untracked(root: &Path, pathspec: &[String]) -> Result<Untracked> {
    crate::snapshot::validate_pathspec(pathspec)?;
    let found = read(&Exec::current(), root, pathspec)?;
    Ok(Untracked {
        names: found
            .names
            .iter()
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect(),
        unread: found.unread,
    })
}

pub(crate) fn read(exec: &Exec, work: &Path, pathspec: &[String]) -> Result<Found> {
    let mut args: Vec<&str> = vec![
        "ls-files",
        "-z",
        "--others",
        "--exclude-standard",
        "--directory",
    ];
    if !pathspec.is_empty() {
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    let mut found = Found {
        names: Vec::new(),
        unread: Vec::new(),
    };
    let mut level = split(exec.records(work, &args)?, &mut found);
    let mut room = ENTRY_BUDGET;
    while !level.is_empty() {
        level.sort_by_key(|dir| !is_policy_dir(dir));
        let mut children: Vec<String> = Vec::new();
        for dir in level {
            let policy = is_policy_dir(&dir);
            let limit = if policy { room.max(POLICY_ROOM) } else { room };
            match children_of(work, &dir, limit)? {
                Children::Repository => found.names.push(format!("{dir}/").into_bytes()),
                Children::Over => found.unread.push(dir),
                Children::Names(names) => {
                    if !policy {
                        room -= names.len();
                    }
                    children.extend(names);
                }
            }
        }
        level = Vec::new();
        for chunk in children.chunks(CHUNK) {
            let mut args: Vec<&str> = vec![
                "--literal-pathspecs",
                "ls-files",
                "-z",
                "--others",
                "--exclude-standard",
                "--directory",
                "--",
            ];
            args.extend(chunk.iter().map(String::as_str));
            level.extend(split(exec.records(work, &args)?, &mut found));
        }
    }
    Ok(found)
}

fn is_policy_dir(dir: &str) -> bool {
    dir == ".diffctx" || dir.ends_with("/.diffctx")
}

/// Files go to `found`; the directories (a record ending in `/`) are returned
/// to be expanded, one that is not text straight to `unread`.
fn split(records: Vec<Vec<u8>>, found: &mut Found) -> Vec<String> {
    let mut dirs = Vec::new();
    for mut record in records {
        if record.last() != Some(&b'/') {
            found.names.push(record);
            continue;
        }
        record.pop();
        match String::from_utf8(record) {
            Ok(dir) => dirs.push(dir),
            Err(not_text) => found
                .unread
                .push(String::from_utf8_lossy(not_text.as_bytes()).into_owned()),
        }
    }
    dirs
}

enum Children {
    /// A nested repository: one entry of this one, never walked.
    Repository,
    /// More entries than the budget has left, or a name no path can spell.
    Over,
    Names(Vec<String>),
}

#[allow(
    clippy::disallowed_methods,
    reason = "lists one directory of the checkout to bound the walk; any failure is an Err, never an empty directory"
)]
fn children_of(work: &Path, dir: &str, room: usize) -> Result<Children> {
    let Ok(path) = RepoPath::new(dir) else {
        return Ok(Children::Over);
    };
    let at = path.under(work);
    match std::fs::symlink_metadata(at.join(".git")) {
        Ok(_) => return Ok(Children::Repository),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&at)? {
        if names.len() >= room {
            return Ok(Children::Over);
        }
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            return Ok(Children::Over);
        };
        names.push(format!("{dir}/{name}"));
    }
    Ok(Children::Names(names))
}
