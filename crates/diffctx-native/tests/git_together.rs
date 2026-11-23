//! Questions one git child answers for several askers: inside a run the first
//! of them starts the combined command and the rest are read from its answer;
//! each answer is the one its own command gives outside a run, on a repository
//! that has an unmerged path, a skip-worktree one, untracked and ignored
//! directories, a nested repository, a tab and a space in names, and a word
//! spelled in several cases beside word characters. One test: the counter is the
//! process's.

use std::path::Path;
use std::process::Command;

use diffctx_snapshot::resource::RunContext;
use diffctx_snapshot::snapshot::{
    files_with_word, files_with_word_any_case, find_toplevel, git_spawns, is_git_repo, open,
    run_git, run_git_or_none, run_git_z,
};
use tempfile::TempDir;

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=n", "-c", "user.email=n@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success() || args[0] == "merge",
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(repo: &Path, rel: &str, text: &str) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
    std::fs::write(path, text).expect("write");
}

fn repository() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    write(repo, "f1", "charge total\n");
    write(repo, "a/.diffctx/ignore", "secret\n");
    write(repo, ".diffctx/ignore", "other\n");
    write(repo, "sp ace/n\u{e9}", "Charge\n");
    #[cfg(unix)]
    write(repo, "tab\tname", "x_charge charge_ CHARGE charge1\n");
    #[cfg(not(unix))]
    write(repo, "word_borders", "x_charge charge_ CHARGE charge1\n");
    write(
        repo,
        "sub/g",
        "(charge) charge.total chargecharge xcharge charge\n",
    );
    write(repo, ".gitignore", "*.log\nignored/\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "one"]);
    git(repo, &["checkout", "-q", "-b", "other"]);
    write(repo, "f1", "other side\n");
    git(repo, &["commit", "-q", "-a", "-m", "other"]);
    git(repo, &["checkout", "-q", "main"]);
    write(repo, "f1", "main side\n");
    git(repo, &["commit", "-q", "-a", "-m", "main"]);
    git(repo, &["merge", "other"]);
    git(repo, &["update-index", "--skip-worktree", "sub/g"]);
    write(repo, "un/d1/x", "charge\n");
    write(repo, "un/y", "Charge\n");
    write(repo, "un/.diffctx/ignore", "z\n");
    write(repo, "new1", "chargeX\n");
    write(repo, "ignored/q", "charge\n");
    write(repo, "top.log", "charge\n");
    git(repo, &["init", "-q", "nest"]);
    write(repo, "nest/f", "charge\n");
    dir
}

/// The listing of one repository: what each of the questions says.
fn answers(repo: &Path) -> Vec<String> {
    let mut all = vec![
        format!("{:?}", is_git_repo(repo).map_err(|e| e.to_string())),
        format!("{:?}", find_toplevel(repo).map_err(|e| e.to_string())),
        format!("{:?}", open(repo).map_err(|e| e.to_string())),
    ];
    all.push(format!(
        "{:?}",
        run_git(repo, &["rev-parse", "--git-dir"]).map_err(|e| e.to_string())
    ));
    for spec in ["HEAD", "main", "other", "nothing"] {
        for kind in ["tree", "commit"] {
            let spec = format!("{spec}^{{{kind}}}");
            all.push(format!(
                "{:?}",
                run_git_or_none(repo, &["rev-parse", "--verify", "--quiet", &spec], 1)
                    .map_err(|e| e.to_string())
            ));
        }
    }
    for args in [
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--directory",
        ][..],
        &["ls-files", "-z", "-t", "--cached"],
        &[
            "ls-files",
            "-z",
            "--cached",
            "--",
            ":(glob)**/.diffctx/ignore",
        ],
        &["ls-files", "-s", "-z"],
    ] {
        all.push(format!(
            "{:?}",
            run_git_z(repo, args).map_err(|e| e.to_string())
        ));
    }
    for word in [
        "charge", "Charge", "CHARGE", "total", "nothing", "x", "side",
    ] {
        for any_case in [false, true] {
            let found = if any_case {
                files_with_word_any_case(repo, word, &[])
            } else {
                files_with_word(repo, word, &[])
            };
            all.push(format!(
                "{word} {any_case} {:?}",
                found.map_err(|e| e.to_string())
            ));
        }
    }
    all
}

#[test]
fn a_run_answers_each_question_as_its_own_command_does() {
    let dir = repository();
    let repo = dir.path();
    let alone = answers(repo);
    let before = git_spawns();
    let run = RunContext::unbounded();
    let together = {
        let _in_run = run.enter();
        answers(repo)
    };
    let spawned = git_spawns() - before;
    assert_eq!(
        together, alone,
        "the combined commands answer as the questions do"
    );
    assert!(
        spawned < alone.len(),
        "a run starts fewer children than it asks questions ({spawned} for {})",
        alone.len()
    );
}
