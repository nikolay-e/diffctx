//! One answer to "how does a path get written into output".
//!
//! There were several. `render` normalised separators behind `#[cfg(windows)]`
//! with a correct reason: on POSIX a backslash is a legal filename character,
//! so `src\utils.py` is one file, and rewriting it reports a path that does not
//! exist while silently merging two distinct files under one heading.
//! `pipeline::rel_path_string` and `locate::rel_path` did the same replacement
//! unconditionally, on every platform — the exact bug that comment warns about,
//! live in the two copies that never received the fix.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Rewrites `\` to `/` on Windows, where it is the component separator, and
/// leaves the string alone everywhere else, where it is an ordinary character.
#[cfg(windows)]
pub fn to_posix_display(s: Cow<'_, str>) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
pub fn to_posix_display(s: Cow<'_, str>) -> String {
    s.into_owned()
}

/// `base.join(rel)` for a `/`-separated `rel` from git or a manifest, spelled
/// the way the platform spells every other path: `join` keeps the `/`, and on
/// Windows `root\src/lib.rs` is then a different string key from the
/// `root\src\lib.rs` canonicalisation produces for the same file.
pub fn repo_join(base: &Path, rel: &str) -> PathBuf {
    base.join(native_separators(Cow::Borrowed(rel)))
}

pub fn join_native(base: &Path, rel: &str) -> String {
    repo_join(base, rel).to_string_lossy().into_owned()
}

#[cfg(windows)]
fn native_separators(s: Cow<'_, str>) -> String {
    s.replace('/', "\\")
}

#[cfg(not(windows))]
fn native_separators(s: Cow<'_, str>) -> String {
    s.into_owned()
}

/// `path` written relative to `root`, ready for output. `None` when `path` is
/// not inside `root` — callers that must still show something decide what.
pub fn display_rel(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|rel| to_posix_display(rel.to_string_lossy()))
}

/// `display_rel`, falling back to the path as given when it is not under
/// `root`. The file lists in the diff output used a private copy of this that
/// rewrote `\` unconditionally (#239).
pub fn display_rel_or_abs(root: &Path, path: &Path) -> String {
    display_rel(root, path).unwrap_or_else(|| to_posix_display(path.to_string_lossy()))
}

/// Containment for a repo-relative name that need not exist on disk — what `git ls-files`,
/// `git diff --name-only` and the deleted-file query hand back. Purely
/// lexical: it normalises `.` and `..` against the root and asks whether the
/// result is still under it, touching no filesystem, so a deleted path and a
/// bare clone's tracked files answer the same as a present one.
///
/// It cannot see a symlink — that is not its job. A working-tree name that
/// passes here and is then READ goes through `source::Containment` at the point of
/// the read.
pub fn contains_lexically(relative: &Path) -> bool {
    use std::path::Component;

    let mut depth: i32 = 0;
    for component in relative.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => return false,
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            Component::CurDir => {}
            Component::Normal(_) => depth += 1,
        }
    }
    true
}

/// `.` and `..` folded lexically out of a slash path; `None` when it climbs
/// above its start.
pub fn normalize_lexical(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

/// A fragment's path as `FragmentIndex` keys it: repo-relative, posix. In
/// production fragment paths are absolute (`fragmentation.rs` stores what it
/// was handed) and the index strips `repo_root`; a caller that derives a
/// reference from a fragment's own path has to do the same, or its reference
/// is absolute and matches nothing.
pub fn index_key_of(path: &str, repo_root: Option<&Path>) -> String {
    repo_root
        .and_then(|root| Path::new(path).strip_prefix(root).ok())
        .map(|rel| to_posix_display(rel.to_string_lossy()))
        .unwrap_or_else(|| to_posix_display(path.into()))
}

/// Lock files: every `*.lock` (mise, Podfile, a package manager's own) and the
/// few that are named otherwise. Tree mode drops them outright; diff mode cannot,
/// because a bumped dependency IS part of the change — but rendering the raw
/// hunks costs thousands of tokens of checksums for a fact that fits on one
/// line, so the paths are reported and the content is left out (#112).
pub fn is_lockfile_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.ends_with(".lock")
        || matches!(
            name,
            "package-lock.json"
                | "npm-shrinkwrap.json"
                | "pnpm-lock.yaml"
                | "bun.lockb"
                | "go.sum"
                | "packages.lock.json"
                | "gradle.lockfile"
                | "Package.resolved"
                | "cabal.project.freeze"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_the_root_has_no_relative_form() {
        assert_eq!(
            display_rel(Path::new("/repo"), Path::new("/elsewhere/a.rs")),
            None
        );
    }

    #[test]
    fn a_path_inside_the_root_loses_exactly_the_root() {
        assert_eq!(
            display_rel(Path::new("/repo"), Path::new("/repo/src/a.rs")).as_deref(),
            Some("src/a.rs")
        );
    }

    /// The divergence this module exists to end. A literal backslash in a POSIX
    /// filename must survive: rewriting it names a file that is not there.
    #[cfg(not(windows))]
    #[test]
    fn a_backslash_in_a_posix_filename_is_left_alone() {
        assert_eq!(
            display_rel(Path::new("/repo"), Path::new(r"/repo/src\utils.py")).as_deref(),
            Some(r"src\utils.py")
        );
    }

    /// Containment is not existence. Everything here names a path that is not
    /// on disk — a file the diff says was DELETED, and a tracked file of a
    /// bare clone, which has no working tree for anything to exist in. Asking
    /// `canonicalize()` about those returns `Err`, and a filter built on it
    /// reports every one of them as an escape: the deleted-file list empties
    /// every time and a bare clone reports no changed files at all.
    #[test]
    fn containment_of_a_path_that_is_not_on_disk_is_still_answered() {
        for inside in [
            "src/deleted.rs",
            "deleted.rs",
            "a/b/../c.rs",
            "./x.rs",
            "nested/very/deep/gone.py",
        ] {
            assert!(
                contains_lexically(Path::new(inside)),
                "{inside} is inside the repository"
            );
        }
        for outside in ["../escape.rs", "a/../../escape.rs", "/etc/shadow"] {
            assert!(
                !contains_lexically(Path::new(outside)),
                "{outside} escapes the repository"
            );
        }
    }
}
