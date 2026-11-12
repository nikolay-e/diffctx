#![allow(dead_code, reason = "each test file uses its own part of the fixture")]

use std::path::Path;

use _diffctx::snapshot::{Listing, Oid, RepoRoot, open};

/// A real repository in a temp directory, driven by the real `git`, with the
/// user's global and system configuration out of the way.
pub(crate) struct Repo {
    dir: tempfile::TempDir,
    pub root: RepoRoot,
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = _diffctx::snapshot::git_command(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

impl Repo {
    pub(crate) fn init() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        run(dir.path(), &["init", "-q", "-b", "main"]);
        let root = open(dir.path()).expect("open the fixture");
        Self { dir, root }
    }

    pub(crate) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(crate) fn git(&self, args: &[&str]) -> String {
        run(self.dir.path(), args)
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        let at = self.dir.path().join(rel);
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("mkdir");
        std::fs::write(at, content).expect("write");
    }

    pub(crate) fn remove(&self, rel: &str) {
        std::fs::remove_file(self.dir.path().join(rel)).expect("remove");
    }

    #[cfg(unix)]
    pub(crate) fn symlink(&self, rel: &str, target: &str) {
        let at = self.dir.path().join(rel);
        std::fs::create_dir_all(at.parent().expect("a parent")).expect("mkdir");
        std::os::unix::fs::symlink(target, at).expect("symlink");
    }

    pub(crate) fn head(&self) -> Oid {
        Oid::parse(self.git(&["rev-parse", "HEAD"]).trim()).expect("an oid")
    }

    pub(crate) fn rev(&self, rev: &str) -> Oid {
        Oid::parse(self.git(&["rev-parse", rev]).trim()).expect("an oid")
    }

    /// Stages everything and commits it.
    pub(crate) fn commit(&self, message: &str) -> Oid {
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
        self.head()
    }
}

pub(crate) fn names(listing: &Listing) -> Vec<String> {
    listing
        .entries
        .iter()
        .map(|e| e.path.as_str().to_string())
        .collect()
}
