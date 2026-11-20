#![allow(dead_code, reason = "each test file uses its own part of the fixture")]

use std::collections::HashSet;
use std::path::Path;

use _diffctx::snapshot::{Listing, Oid, RangeSpec, RepoRoot, open};

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

    /// The lines a range wrote, by path: the new side of `git diff -U0` from its base to its
    /// head, or to the tracked working tree. A snapshot writes none.
    pub(crate) fn written(&self, range: &RangeSpec) -> HashSet<(String, u32)> {
        let (base, head) = match range {
            RangeSpec::Review { base, head } => (base, Some(head.as_str())),
            RangeSpec::WorkingTree { base } => (base, None),
            _ => return HashSet::new(),
        };
        let mut args = vec!["diff", "-U0", "--no-color", base.as_str()];
        args.extend(head);
        let (mut path, mut written) = (String::new(), HashSet::new());
        for line in self.git(&args).lines() {
            if let Some(to) = line.strip_prefix("+++ ") {
                path = to.strip_prefix("b/").unwrap_or(to).to_string();
            } else if let Some(new) = line.strip_prefix("@@ -").and_then(|h| h.split(' ').nth(1)) {
                let new = new.trim_start_matches('+');
                let (start, count) = new.split_once(',').unwrap_or((new, "1"));
                let start: u32 = start.parse().expect("a hunk's first line");
                let count: u32 = count.parse().expect("a hunk's line count");
                written.extend((start..start + count).map(|n| (path.clone(), n)));
            }
        }
        written
    }
}

pub(crate) fn names(listing: &Listing) -> Vec<String> {
    listing
        .entries
        .iter()
        .map(|e| e.path.as_str().to_string())
        .collect()
}
