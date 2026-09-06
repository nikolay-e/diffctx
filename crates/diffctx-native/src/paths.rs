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
use std::path::Path;

/// Rewrites `\` to `/` on Windows, where it is the component separator, and
/// leaves the string alone everywhere else, where it is an ordinary character.
#[cfg(windows)]
pub(crate) fn to_posix_display(s: Cow<'_, str>) -> String {
    s.replace('\\', "/")
}

#[cfg(not(windows))]
pub(crate) fn to_posix_display(s: Cow<'_, str>) -> String {
    s.into_owned()
}

/// `path` written relative to `root`, ready for output. `None` when `path` is
/// not inside `root` — callers that must still show something decide what.
pub(crate) fn display_rel(root: &Path, path: &Path) -> Option<String> {
    path.strip_prefix(root)
        .ok()
        .map(|rel| to_posix_display(rel.to_string_lossy()))
}

/// `display_rel`, falling back to the path as given when it is not under
/// `root`. The file lists in the diff output used a private copy of this that
/// rewrote `\` unconditionally (#239).
pub(crate) fn display_rel_or_abs(root: &Path, path: &Path) -> String {
    display_rel(root, path).unwrap_or_else(|| to_posix_display(path.to_string_lossy()))
}

/// `path` if reading it stays inside `root`, `None` if it would escape.
///
/// The repository is the trust boundary: a tool that reads a stranger's
/// checkout must not be walked out of it by a symlink the checkout contains.
/// `~/repo/evil -> /etc/shadow` is one `read_to_string` away from being a
/// context fragment, and `canonicalize()` on that name returns what it POINTS
/// AT — which is how an absolute out-of-root path reached the changed-file
/// list.
///
/// This is the READ-time guard, and it answers containment by canonicalising
/// both sides, so a symlink chain, a `..` segment and a symlinked parent
/// directory are all caught by one comparison. It therefore requires the path
/// to exist, which makes it the wrong question for a name git handed us: a
/// deleted file is by definition gone, and a bare clone has no working tree at
/// all. Those callers get `contains_lexically` instead — containment and
/// existence are different questions, and answering the second one where the
/// first was meant empties both lists.
///
/// The path is returned **as given**, never canonicalised: the repo-relative
/// name is what every downstream layer keys on (fragment ids, ignore rules,
/// the secret policy), and substituting the link target renames the object
/// mid-pipeline.
pub(crate) fn resolve_within(root: &Path, path: &Path) -> Option<std::path::PathBuf> {
    let root_canon = root.canonicalize().ok()?;
    let canon = path.canonicalize().ok()?;
    canon.starts_with(&root_canon).then(|| path.to_path_buf())
}

/// Containment for a repo-relative name that need not exist on disk — what `git ls-files`,
/// `git diff --name-only` and the deleted-file query hand back. Purely
/// lexical: it normalises `.` and `..` against the root and asks whether the
/// result is still under it, touching no filesystem, so a deleted path and a
/// bare clone's tracked files answer the same as a present one.
///
/// It cannot see a symlink — that is not its job. A working-tree name that
/// passes here and is then READ goes through `resolve_within` at the point of
/// the read.
pub(crate) fn contains_lexically(relative: &Path) -> bool {
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
    fn a_symlink_leaving_the_repository_is_not_resolvable_within_it() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let root = tmp.path().join("repo");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("secret.txt"), "LEAK\n").expect("write");
        std::fs::write(root.join("real.txt"), "ok\n").expect("write");

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.join("secret.txt"), root.join("escape.txt"))
                .expect("symlink");
            std::os::unix::fs::symlink(&outside, root.join("escape_dir")).expect("symlink dir");
            assert_eq!(resolve_within(&root, &root.join("escape.txt")), None);
            // A symlinked *directory* hides the escape one level up: the leaf
            // is an ordinary file, so only the canonical form catches it.
            assert_eq!(
                resolve_within(&root, &root.join("escape_dir").join("secret.txt")),
                None
            );
        }
        assert_eq!(
            resolve_within(&root, &root.join("real.txt")),
            Some(root.join("real.txt")),
            "a real file inside the root keeps the name it was asked about"
        );
        assert_eq!(resolve_within(&root, &outside.join("secret.txt")), None);
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
