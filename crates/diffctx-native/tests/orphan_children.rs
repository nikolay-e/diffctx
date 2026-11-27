// A run that dies without unwinding leaves nothing of itself behind (fj#211):
// three `git grep` outlived their diffctx by 17 hours, each holding a core and
// a pattern file. A `git` earlier on PATH stalls the one search that carries a
// removed definition's names, the run is killed under it, and what the run
// started is read off a FIFO every descendant holds open. Nothing here reads
// the clock: a poll is a count of looks, and a stall never ends on its own.
#![cfg(unix)]

use std::ffi::CString;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use tempfile::TempDir;

mod support;
use support::{diffctx, git, init_repo};

/// Looks at a condition before the test gives up on it, and the pause between
/// two of them.
const LOOKS: usize = 1200;
const PAUSE: Duration = Duration::from_millis(50);

/// The most patterns one search may carry. A run that hands git every removed
/// name at once spends minutes in its matcher, which is superlinear in the
/// pattern count.
const PATTERNS_PER_SEARCH: usize = 50;

/// Past this many removed names a run traces the rest no further.
const REMOVED_NAMES: usize = 2000;

/// Counts the patterns of every `git grep` (its `-e` arguments, the alternatives
/// of a `-P` one, and the lines of a `-f` file), and with `FAKE_GREP_STALL` set stalls the first search that
/// carries more than one: the one a removed definition's names go to. `exec`,
/// so the stalled process is git's own pid and nothing it forks.
const GREP_SHIM: &str = r#"#!/bin/sh
case " $* " in *" grep "*) ;; *) exec "$REAL_GIT" "$@" ;; esac
n=0
prev=
pcre=
for a in "$@"; do
  case "$prev" in
    -e) if [ -n "$pcre" ]; then n=$((n + 1 + $(printf '%s' "$a" | tr -cd '|' | wc -c))); else n=$((n + 1)); fi ;;
    -f) n=$((n + $(wc -l < "$a"))) ;;
  esac
  [ "$a" = -P ] && pcre=1
  prev=$a
done
echo "$n" >> "$FAKE_GREP_COUNTS"
if [ -n "$FAKE_GREP_STALL" ] && [ "$n" -ge 2 ]; then
  echo $$ >> "$FAKE_GREP_PIDS"
  exec sleep 3600
fi
exec "$REAL_GIT" "$@"
"#;

struct Fixture {
    tmp: TempDir,
}

impl Fixture {
    /// A Ruby file of `names` definitions that the last commit empties down to one:
    /// no cell reads Ruby, so its removed names are found by name.
    fn removing(names: usize) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        for dir in ["bin", "repo", "tmp"] {
            std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
        }
        let shim = tmp.path().join("bin").join("git");
        std::fs::write(&shim, GREP_SHIM).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let repo = tmp.path().join("repo");
        init_repo(&repo);
        let mut module = String::new();
        for i in 0..names {
            writeln!(module, "def removed_handler_{i}\n  {i}\nend\n").unwrap();
        }
        std::fs::write(repo.join("handlers.rb"), module).unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        std::fs::write(repo.join("handlers.rb"), "def kept\n  0\nend\n").unwrap();
        git(&repo, &["commit", "-qam", "empty the module"]);
        Self { tmp }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.tmp.path().join(name)
    }

    fn sentinel(&self) -> CString {
        CString::new(self.path("sentinel").into_os_string().into_encoded_bytes()).unwrap()
    }

    /// The run, started: its stdout and stderr are files, so a stalled run
    /// never blocks on a full pipe, and every descendant holds the sentinel.
    fn start(&self, stall: bool) -> Child {
        let sentinel = self.sentinel();
        // SAFETY: mkfifo(2) only creates the path.
        #[allow(unsafe_code)]
        unsafe {
            libc::mkfifo(sentinel.as_ptr(), 0o600);
        }
        let mut command = diffctx();
        // SAFETY: between fork and exec only open(2), which is async-signal
        // safe, runs. O_RDWR, because a write-only open of a FIFO with no
        // reader blocks; the descriptor is not close-on-exec, so it follows
        // every descendant.
        #[allow(unsafe_code)]
        unsafe {
            use std::os::unix::process::CommandExt as _;
            command.pre_exec(move || {
                libc::open(sentinel.as_ptr(), libc::O_RDWR);
                Ok(())
            });
        }
        let path = format!(
            "{}:{}",
            self.path("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        command
            .current_dir(self.path("repo"))
            .args([
                ".",
                "--diff",
                "HEAD~1..HEAD",
                "--mode",
                "impact",
                "-f",
                "json",
                "-q",
                "--timeout",
                "86400",
            ])
            .env("PATH", path)
            .env("REAL_GIT", real_git())
            .env("TMPDIR", self.path("tmp"))
            .env("FAKE_GREP_COUNTS", self.path("counts"))
            .env("FAKE_GREP_PIDS", self.path("pids"))
            .env("DIFFCTX_NO_MARKER", "1")
            .stdin(Stdio::null())
            .stdout(std::fs::File::create(self.path("stdout")).unwrap())
            .stderr(std::fs::File::create(self.path("stderr")).unwrap());
        if stall {
            command.env("FAKE_GREP_STALL", "1");
        }
        command.spawn().expect("start diffctx")
    }

    fn stalled_search(&self) -> Option<String> {
        let pids = std::fs::read_to_string(self.path("pids")).ok()?;
        pids.split_whitespace().next().map(str::to_string)
    }

    /// No process of the run holds the sentinel any more.
    fn sentinel_is_free(&self) -> bool {
        let sentinel = self.sentinel();
        // SAFETY: opens a path we created, reads one byte into a local.
        #[allow(unsafe_code)]
        unsafe {
            let fd = libc::open(sentinel.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK);
            assert!(fd >= 0, "the sentinel cannot be opened");
            let mut byte = 0u8;
            let n = libc::read(fd, (&raw mut byte).cast(), 1);
            libc::close(fd);
            n == 0
        }
    }

    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.path("tmp"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect()
    }
}

/// A stall never ends on its own, so whatever a failed run left behind is
/// killed here: a regression leaks nothing onto the machine.
impl Drop for Fixture {
    fn drop(&mut self) {
        let Some(pid) = self.stalled_search() else {
            return;
        };
        let out = Command::new("ps")
            .args(["-o", "command=", "-p", &pid])
            .output()
            .expect("ps");
        if String::from_utf8_lossy(&out.stdout).contains("sleep 3600") {
            let _ = Command::new("kill").args(["-9", &pid]).status();
        }
    }
}

fn real_git() -> String {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("locate git");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn looked_for(mut found: impl FnMut() -> bool) -> bool {
    for _ in 0..LOOKS {
        if found() {
            return true;
        }
        std::thread::sleep(PAUSE);
    }
    found()
}

/// Kills the run with `signal` while its search is stalled, and reads what is
/// left of it.
fn kill_mid_search(signal: i32) -> Fixture {
    let fx = Fixture::removing(REMOVED_NAMES / 20);
    let mut run = fx.start(true);
    assert!(
        looked_for(|| fx.stalled_search().is_some()),
        "the run never reached the search the shim stalls: the test measured nothing"
    );
    let pid = i32::try_from(run.id()).unwrap();
    // SAFETY: signals the child this test started and still holds.
    #[allow(unsafe_code)]
    unsafe {
        libc::kill(pid, signal);
    }
    run.wait().expect("reap the run");
    fx
}

fn assert_nothing_left(fx: &Fixture, how: &str) {
    let drained = looked_for(|| fx.sentinel_is_free());
    let files = fx.leftovers();
    assert!(
        drained && files.is_empty(),
        "{how}: a process the run started is still running: {}; files left in the temp directory: {files:?}",
        !drained
    );
}

#[test]
fn a_run_killed_outright_leaves_no_search_and_no_file() {
    let fx = kill_mid_search(libc::SIGKILL);
    assert_nothing_left(&fx, "SIGKILL");
}

#[test]
fn a_run_asked_to_stop_leaves_no_search_and_no_file() {
    let fx = kill_mid_search(libc::SIGTERM);
    assert_nothing_left(&fx, "SIGTERM");
}

/// The shape of gh#619 turned round: tens of thousands of names, here more
/// than a run traces. No search carries more than a bounded few of them, and
/// the answer says the rest were not traced.
#[test]
fn removed_names_are_searched_in_bounded_batches_and_the_cap_is_named() {
    let fx = Fixture::removing(REMOVED_NAMES + REMOVED_NAMES / 4);
    let status = fx.start(false).wait().expect("wait for the run");
    assert!(status.success(), "{}", read(&fx.path("stderr")));
    let counts = read(&fx.path("counts"));
    let widest = counts
        .lines()
        .filter_map(|n| n.trim().parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    assert!(
        (2..=PATTERNS_PER_SEARCH).contains(&widest),
        "the widest search carried {widest} patterns: it must carry some, and at most {PATTERNS_PER_SEARCH}"
    );
    let doc: serde_json::Value = serde_json::from_str(&read(&fx.path("stdout"))).expect("JSON");
    assert!(
        doc["coverage"]["limits"]
            .to_string()
            .contains("candidate_limit"),
        "names past the cap must be disclosed, got {}",
        doc["coverage"]
    );
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}
