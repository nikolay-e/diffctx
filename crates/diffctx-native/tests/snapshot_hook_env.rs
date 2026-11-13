//! A git hook exports `GIT_DIR`, `GIT_INDEX_FILE` and their kin, and a diffctx
//! run from inside one must still read the repository it was pointed at. The
//! snapshot module is the only door onto git, so a variable it leaves in the
//! child's environment answers for the wrong repository.

mod support;
use support::{commit_all, diffctx, init_repo, write};

const HOOK_ENV: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_PREFIX",
];

fn repo_with(file: &str, before: &str, after: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    init_repo(dir.path());
    write(dir.path(), file, before);
    commit_all(dir.path(), "initial");
    write(dir.path(), file, after);
    commit_all(dir.path(), "change");
    dir
}

#[test]
fn a_run_inside_a_hook_reads_its_own_repository_not_the_hooks() {
    let ours = repo_with(
        "ours.py",
        "def a():\n    return 1\n",
        "def a():\n    return 2\n",
    );
    let hooks = repo_with(
        "hooks.py",
        "def b():\n    return 1\n",
        "def b():\n    return 2\n",
    );
    let git_dir = hooks.path().join(".git");

    let mut command = diffctx();
    command
        .current_dir(ours.path())
        .env("DIFFCTX_NO_MARKER", "1")
        .env("GIT_DIR", &git_dir)
        .env("GIT_WORK_TREE", hooks.path())
        .env("GIT_INDEX_FILE", git_dir.join("index"))
        .env("GIT_COMMON_DIR", &git_dir)
        .env("GIT_OBJECT_DIRECTORY", git_dir.join("objects"))
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", git_dir.join("objects"))
        .env("GIT_PREFIX", "elsewhere/")
        .args([".", "--diff", "HEAD~1..HEAD", "-f", "json", "-q"]);
    for name in HOOK_ENV {
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == name && value.is_some()),
            "{name} is not set for the run"
        );
    }
    let out = command.output().expect("run diffctx");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("ours.py"),
        "its own file is missing: {stdout}"
    );
    assert!(
        !stdout.contains("hooks.py"),
        "the hook's repository answered: {stdout}"
    );
}

const PROBE: &str = "DIFFCTX_FIXTURE_GIT_PROBE";

/// The fixtures a suite builds repositories with run under the pre-commit hook:
/// a `git add -A` that inherited `GIT_INDEX_FILE` wrote the temporary
/// repository's files into the enclosing repository's index. The test runs
/// itself with the hook's variables pointing nowhere; the inner run builds a
/// repository with the shared fixture and only commits if it ignored them.
#[test]
fn the_fixture_builds_its_repository_whatever_the_environment_names() {
    if std::env::var_os(PROBE).is_some() {
        let dir = repo_with("a.py", "x = 1\n", "x = 2\n");
        assert!(dir.path().join(".git").is_dir());
        return;
    }
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "the_fixture_builds_its_repository_whatever_the_environment_names",
        ])
        .env(PROBE, "1")
        .env("GIT_INDEX_FILE", "/nonexistent/index")
        .env("GIT_DIR", "/nonexistent/git")
        .output()
        .expect("run the probe");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
