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
}
