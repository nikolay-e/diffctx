//! One line, the way Claude Code runs it: the hook's `PreToolUse`, the line
//! in a real bash, then the hook's `PostToolUse` (`PostToolUseFailure` for a
//! non-zero exit) with the event JSON a harness sends. A line run in the
//! background gets its `PostToolUse` at launch and a later event of the
//! session after the line finished.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use super::Shell;
use super::line::{Cwd, Line, Peer, Repo};
use super::world::{RepoState, World};

const SESSION: &str = "prop-session";

pub struct HookRun {
    pub event: &'static str,
    pub code: Option<i32>,
    pub stdout: String,
}

pub struct Driven {
    pub shell: String,
    pub command: String,
    pub before: BTreeMap<Repo, RepoState>,
    pub after: BTreeMap<Repo, RepoState>,
    /// Commits another process made in the checkout, per repository.
    pub peer_commits: BTreeMap<Repo, BTreeSet<String>>,
    pub runs: Vec<HookRun>,
    pub log: String,
}

impl Driven {
    /// Everything the hook put in front of the agent, run by run.
    pub fn printed(&self) -> String {
        let mut out = Vec::new();
        for run in &self.runs {
            let Ok(doc) = serde_json::from_str::<serde_json::Value>(run.stdout.trim()) else {
                continue;
            };
            for key in ["additionalContext", "permissionDecisionReason"] {
                if let Some(text) = doc["hookSpecificOutput"][key].as_str() {
                    out.push(text.to_string());
                }
            }
        }
        out.join("\n---\n")
    }
}

pub fn hook_bin() -> PathBuf {
    std::env::var_os("DIFFCTX_HOOK_BIN").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_diffctx")),
        PathBuf::from,
    )
}

fn payload(
    world: &World,
    kind: &str,
    tool_use_id: &str,
    cwd: &Path,
    command: &str,
    background: bool,
) -> serde_json::Value {
    let mut input = serde_json::json!({ "command": command, "description": "property case" });
    if background {
        input["run_in_background"] = true.into();
    }
    serde_json::json!({
        "session_id": SESSION,
        "transcript_path": world.root().join("transcript.jsonl"),
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": kind,
        "tool_name": "Bash",
        "tool_input": input,
        "tool_use_id": tool_use_id,
    })
}

fn run_hook(world: &World, event: &'static str, cwd: &Path, stdin: &serde_json::Value) -> HookRun {
    let mut child = world
        .command(hook_bin())
        .args(["hook", event])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("HARNESS: spawn the hook");
    child
        .stdin
        .take()
        .expect("HARNESS: hook stdin")
        .write_all(stdin.to_string().as_bytes())
        .expect("HARNESS: feed the hook");
    let out = child
        .wait_with_output()
        .expect("HARNESS: wait for the hook");
    HookRun {
        event,
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

pub fn drive(world: &World, shell: &Shell, line: &Line) -> Driven {
    let command = line.render(world);
    let cwd = match line.cwd {
        Cwd::In(repo) => world.path(repo),
        Cwd::Neutral => world.neutral(),
    };
    let mut peer_commits: BTreeMap<Repo, BTreeSet<String>> = BTreeMap::new();
    if let Peer::CommitBefore(repo) = line.peer {
        peer_commits
            .entry(repo)
            .or_default()
            .insert(world.peer_commit(repo));
    }
    let before = world.states();
    let mut runs = vec![run_hook(
        world,
        "pretooluse",
        &cwd,
        &payload(
            world,
            "PreToolUse",
            "toolu_1",
            &cwd,
            &command,
            line.background,
        ),
    )];
    if let Peer::PushBefore(repo) = line.peer {
        world.peer_push(repo);
    }
    if line.background {
        background(world, shell, &cwd, &command, &mut runs);
    } else {
        let out = world
            .command(&shell.path)
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .stdin(Stdio::null())
            .output()
            .expect("HARNESS: run the line");
        if let Peer::CommitAfter(repo) = line.peer {
            peer_commits
                .entry(repo)
                .or_default()
                .insert(world.peer_commit(repo));
        }
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        let mut post = payload(world, "PostToolUse", "toolu_1", &cwd, &command, false);
        let event = if out.status.success() {
            post["tool_response"] = serde_json::json!({
                "stdout": stdout, "stderr": stderr, "interrupted": false, "isImage": false
            });
            "posttooluse"
        } else {
            post["hook_event_name"] = "PostToolUseFailure".into();
            post["error"] = format!(
                "Exit code {}\n{stdout}{stderr}",
                out.status.code().unwrap_or(1)
            )
            .into();
            post["is_interrupt"] = false.into();
            "posttoolusefailure"
        };
        runs.push(run_hook(world, event, &cwd, &post));
    }
    let log = std::fs::read_to_string(world.cache().join("diffctx").join("hook-runs.log"))
        .unwrap_or_default();
    Driven {
        shell: shell.label.clone(),
        command,
        before,
        after: world.states(),
        peer_commits,
        runs,
        log,
    }
}

/// The line starts only once the hook has answered its launch, so that the
/// `PostToolUse` precedes what git records, as it does for a slow
/// pre-commit; the gate is a fifo, not a clock.
fn background(world: &World, shell: &Shell, cwd: &Path, command: &str, runs: &mut Vec<HookRun>) {
    let gate = world.root().join("gate.fifo");
    let made = world
        .command("mkfifo")
        .arg(&gate)
        .status()
        .expect("HARNESS: mkfifo");
    assert!(made.success(), "HARNESS: mkfifo failed");
    let child = world
        .command("sh")
        .args([
            "-c",
            "read -r _ < \"$GATE\"; exec \"$LINE_SHELL\" -c \"$LINE\"",
        ])
        .env("GATE", &gate)
        .env("LINE_SHELL", &shell.path)
        .env("LINE", command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("HARNESS: start the background line");
    let mut launched = payload(world, "PostToolUse", "toolu_1", cwd, command, true);
    launched["tool_response"] =
        serde_json::json!({ "backgroundTaskId": "bg1", "stdout": "", "stderr": "" });
    runs.push(run_hook(world, "posttooluse", cwd, &launched));
    std::fs::OpenOptions::new()
        .write(true)
        .open(&gate)
        .and_then(|mut f| f.write_all(b"go\n"))
        .expect("HARNESS: open the gate");
    child
        .wait_with_output()
        .expect("HARNESS: wait for the background line");
    let mut next = payload(world, "PostToolUse", "toolu_2", cwd, "echo done", false);
    next["tool_response"] = serde_json::json!({ "stdout": "done\n", "stderr": "" });
    runs.push(run_hook(world, "posttooluse", cwd, &next));
}

/// The files the engine reads a printed range to cover.
pub fn covered_files(world: &World, repo: Repo, range: &str) -> BTreeSet<String> {
    let out = world
        .command(hook_bin())
        .arg(world.path(repo))
        .args(["--diff", range, "--mode", "impact", "-q", "-f", "json"])
        .stdin(Stdio::null())
        .output()
        .expect("HARNESS: run the engine");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "HARNESS: the engine printed no JSON for {range}: {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    doc["changed_files"]
        .as_array()
        .unwrap_or_else(|| panic!("HARNESS: no changed_files for {range}"))
        .iter()
        .filter_map(|f| f.as_str().map(str::to_string))
        .collect()
}
