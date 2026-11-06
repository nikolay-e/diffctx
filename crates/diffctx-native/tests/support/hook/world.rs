//! Two scratch repositories, each with a bare remote, branches for every
//! integration verb, and the processes that act on them. Every command runs
//! with git's global and system configuration off. A harness failure panics
//! with `HARNESS`, so that no suite mistakes it for a property that failed.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

use super::line::Repo;

/// The remote branch a peer pushes to; no line of the grammar pushes it.
pub const PEER_BRANCH: &str = "refs/heads/peer-branch";

pub struct World {
    _tmp: TempDir,
    root: PathBuf,
}

/// What a repository holds, as git reports it.
pub struct RepoState {
    /// Every commit reachable from a ref.
    pub all: BTreeSet<String>,
    /// Every commit reachable from a remote-tracking ref.
    pub remotes: BTreeSet<String>,
    pub head: String,
    /// The bare remote's branches.
    pub remote_heads: BTreeMap<String, String>,
}

impl World {
    pub fn new() -> Self {
        let tmp = TempDir::new().expect("HARNESS: tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("HARNESS: canonicalize");
        let world = Self { _tmp: tmp, root };
        for dir in ["home", "neutral", "cache", "scripts"] {
            std::fs::create_dir_all(world.root.join(dir)).expect("HARNESS: mkdir");
        }
        for repo in Repo::BOTH {
            world.seed(repo);
        }
        world
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, repo: Repo) -> PathBuf {
        self.root.join(repo.name())
    }

    pub fn bare(&self, repo: Repo) -> PathBuf {
        self.root.join(format!("{}.git", repo.name()))
    }

    pub fn neutral(&self) -> PathBuf {
        self.root.join("neutral")
    }

    pub fn cache(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn script(&self, repo: Repo) -> String {
        self.root
            .join("scripts")
            .join(format!("{}.sh", repo.name()))
            .to_string_lossy()
            .into_owned()
    }

    pub fn patch(&self, repo: Repo) -> String {
        self.root
            .join(format!("{}.patch", repo.name()))
            .to_string_lossy()
            .into_owned()
    }

    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        for (key, _) in std::env::vars_os() {
            let key = key.to_string_lossy().into_owned();
            if ["GIT_", "DIFFCTX_", "CLAUDE"]
                .iter()
                .any(|prefix| key.starts_with(prefix))
            {
                command.env_remove(key);
            }
        }
        command
            .env("HOME", self.root.join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_EDITOR", "true")
            .env("GIT_MERGE_AUTOEDIT", "no")
            .env("LC_ALL", "C")
            .env("DIFFCTX_CACHE_DIR", self.cache());
        command
    }

    /// git's stdout, trimmed; a failure is the harness's.
    pub fn git(&self, dir: &Path, args: &[&str]) -> String {
        let out = self
            .command("git")
            .current_dir(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("HARNESS: spawn git");
        assert!(
            out.status.success(),
            "HARNESS: git {args:?} in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// git's stdout, trimmed, when it succeeded.
    pub fn try_git(&self, dir: &Path, args: &[&str]) -> Option<String> {
        let out = self
            .command("git")
            .current_dir(dir)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    pub fn tree_of(&self, repo: Repo, rev: &str) -> Option<String> {
        self.try_git(
            &self.path(repo),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{tree}}"),
            ],
        )
    }

    pub fn commit_of(&self, repo: Repo, rev: &str) -> Option<String> {
        self.try_git(
            &self.path(repo),
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{rev}^{{commit}}"),
            ],
        )
    }

    pub fn state(&self, repo: Repo) -> RepoState {
        let work = self.path(repo);
        let lines = |out: String| -> BTreeSet<String> { out.lines().map(str::to_string).collect() };
        let heads = self.git(
            &self.bare(repo),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
            ],
        );
        RepoState {
            all: lines(self.git(&work, &["rev-list", "--all"])),
            remotes: lines(self.git(&work, &["rev-list", "--remotes"])),
            head: self.git(&work, &["rev-parse", "HEAD"]),
            remote_heads: heads
                .lines()
                .filter_map(|l| l.split_once(' '))
                .map(|(name, oid)| (name.to_string(), oid.to_string()))
                .collect(),
        }
    }

    pub fn states(&self) -> BTreeMap<Repo, RepoState> {
        Repo::BOTH.iter().map(|r| (*r, self.state(*r))).collect()
    }

    /// A commit another session makes in the same checkout.
    pub fn peer_commit(&self, repo: Repo) -> String {
        let work = self.path(repo);
        append(&work.join("peer.py"), "def peer():\n    return 1\n");
        self.git(&work, &["add", "peer.py"]);
        self.git(&work, &["commit", "-q", "-m", "peer edit"]);
        self.git(&work, &["rev-parse", "HEAD"])
    }

    /// A push another checkout of the repository makes to the remote's
    /// other branch.
    pub fn peer_push(&self, repo: Repo) {
        let clone = self.root.join(format!("peer-{}", repo.name()));
        if !clone.exists() {
            self.git(
                &self.root,
                &[
                    "clone",
                    "-q",
                    "-b",
                    "main",
                    &self.bare(repo).to_string_lossy(),
                    &clone.to_string_lossy(),
                ],
            );
            self.git(&clone, &["config", "user.email", "peer@example.com"]);
            self.git(&clone, &["config", "user.name", "peer"]);
        }
        append(
            &clone.join("peer_push.py"),
            "def peer_push():\n    return 1\n",
        );
        self.git(&clone, &["add", "peer_push.py"]);
        self.git(&clone, &["commit", "-q", "-m", "peer push"]);
        self.git(
            &clone,
            &["push", "-q", "origin", &format!("HEAD:{PEER_BRANCH}")],
        );
    }

    fn seed(&self, repo: Repo) {
        let name = repo.name();
        let work = self.path(repo);
        let bare = self.bare(repo);
        let commit = |message: &str| {
            self.git(&work, &["add", "-A"]);
            self.git(&work, &["commit", "-q", "-m", &format!("{message} {name}")]);
        };
        self.git(
            &self.root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &bare.to_string_lossy(),
            ],
        );
        self.git(
            &self.root,
            &["init", "-q", "-b", "main", &work.to_string_lossy()],
        );
        for (key, value) in [
            ("user.email", "agent@example.com"),
            ("user.name", "agent"),
            ("commit.gpgsign", "false"),
            ("pull.rebase", "false"),
            ("alias.ci", "commit"),
            ("gc.auto", "0"),
        ] {
            self.git(&work, &["config", key, value]);
        }
        let put = |file: &str, content: String| {
            std::fs::write(work.join(file), content).expect("HARNESS: write");
        };
        put(
            "tracked.py",
            format!("def total(items):\n    return sum(items)  # {name}\n"),
        );
        put(
            "caller.py",
            "from tracked import total\n\n\ndef charge(items):\n    return total(items)\n".into(),
        );
        put("other.py", format!("RATE = 1  # {name}\n"));
        put("README.md", format!("# {name}\n"));
        commit("base");
        self.git(&work, &["remote", "add", "origin", &bare.to_string_lossy()]);
        self.git(&work, &["push", "-q", "-u", "origin", "main"]);

        self.git(&work, &["checkout", "-q", "-b", "pushed"]);
        put("pushed.py", format!("def pushed():\n    return '{name}'\n"));
        commit("pushed");
        self.git(&work, &["push", "-q", "origin", "pushed"]);

        self.git(&work, &["checkout", "-q", "-b", "topic", "main"]);
        put("topic.py", format!("def topic():\n    return '{name}'\n"));
        commit("topic");

        self.git(&work, &["checkout", "-q", "main"]);
        put("other.py", format!("RATE = 2  # {name}\n"));
        commit("advance main");
        self.git(&work, &["push", "-q", "origin", "main"]);

        self.git(&work, &["checkout", "-q", "-b", "patch-src", "main"]);
        put("am.py", format!("def am():\n    return '{name}'\n"));
        commit("patch");
        let patch = self
            .command("git")
            .current_dir(&work)
            .args(["format-patch", "-1", "--stdout"])
            .output()
            .expect("HARNESS: format-patch");
        assert!(patch.status.success(), "HARNESS: format-patch failed");
        std::fs::write(self.patch(repo), patch.stdout).expect("HARNESS: write patch");
        self.git(&work, &["checkout", "-q", "main"]);
        self.git(&work, &["branch", "-q", "-D", "patch-src"]);

        let script = self.script(repo);
        std::fs::write(
            &script,
            format!(
                "#!/bin/bash\nset -e\ncd '{}'\nprintf 'def scripted():\\n    return 1\\n' >> tracked.py\ngit add -A\ngit commit -q -m scripted\n",
                work.display()
            ),
        )
        .expect("HARNESS: write script");
        make_executable(&script);
    }
}

fn append(path: &Path, text: &str) {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .expect("HARNESS: append");
}

fn make_executable(path: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("HARNESS: chmod");
}
