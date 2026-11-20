//! A repository path is the name git stores: relative, `/`-separated, never a
//! location on the disk. A name that spells one (a rooted path, a Windows drive
//! or share) is refused where it is made, so no later layer has to guess
//! whether it was handed a place or a name.

use diffctx_snapshot::snapshot::RepoPath;

#[test]
fn a_relative_name_is_a_repository_path() {
    for name in [
        "a.py",
        "src/lib.rs",
        "a b/c d.txt",
        "dir/.hidden",
        "x/y/z/w.tf",
    ] {
        let path = RepoPath::new(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(path.as_str(), name);
    }
}

/// On POSIX a backslash is an ordinary filename character, so `a\b.py` is one
/// file in the root, not a file `b.py` in a directory `a`.
#[test]
fn a_backslash_inside_a_name_is_no_separator() {
    let path = RepoPath::new("a\\b.py").expect("one file");
    assert_eq!(path.file_name(), "a\\b.py");
    assert_eq!(path.parent(), None);
}

#[test]
fn a_name_that_is_no_relative_path_is_refused() {
    for name in [
        "",
        "/",
        "/etc/passwd",
        "a//b",
        "a/",
        "./a.py",
        "a/./b",
        "../a.py",
        "a/../b",
        "a\0b",
    ] {
        assert!(RepoPath::new(name).is_err(), "{name:?} was accepted");
    }
}

#[test]
fn a_windows_drive_or_share_is_a_location_not_a_name() {
    for name in [
        "C:/repo/a.py",
        "c:/a.py",
        "C:\\repo\\a.py",
        "D:",
        "\\\\server\\share\\a.py",
        "\\\\?\\C:\\repo\\a.py",
    ] {
        assert!(RepoPath::new(name).is_err(), "{name:?} was accepted");
    }
}

#[test]
fn a_colon_that_is_no_drive_stays_part_of_a_name() {
    for name in ["notes:todo.txt", "ab:/c.py", "src/C:/a.py", "a:b/c.py"] {
        assert!(RepoPath::new(name).is_ok(), "{name:?} was refused");
    }
}
