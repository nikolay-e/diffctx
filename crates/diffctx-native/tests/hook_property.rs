//! The hook computes a review from what git recorded, so for any command
//! line an agent can write, what the released hook reviewed must be what
//! landed: the reviewed tree equals the landed `HEAD^{tree}` in each
//! repository, a pushed branch's tip is reviewed, or the hook says in words
//! that it did not review (gh#508's gate; AC8).
//!
//! Lines come from a grammar (`support/hook/line.rs`) in families, run in
//! scratch repositories under every bash this machine has — 3.2 at
//! `/bin/bash` on macOS, 5 beside it when installed — with the released
//! binary driven Pre -> line -> Post as Claude Code does. What a line
//! recorded is read from git; what was reviewed, from what the hook printed.
//!
//! The red baseline is below, checked both ways: a red family whose lines
//! the hook now reviews fails the suite until it moves to green, and a green
//! one that fails is a regression. A red family is built so that every line
//! in it shows the miss, so the baseline does not depend on the seed. The
//! hook decides from git's record (T19); what is left red is a repository no
//! line, cwd or earlier call of the session names (T21).
//!
//! `DIFFCTX_HOOK_PROPERTY_CASES` (default 6 lines per family and bash),
//! `DIFFCTX_HOOK_PROPERTY_SEED` (a number; `random` for the nightly run),
//! `DIFFCTX_HOOK_BIN` (a binary other than the one cargo built).

#![cfg(unix)]

#[path = "support/hook/mod.rs"]
mod hook;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed, TestError, TestRunner};

use hook::line::{Cwd, Join, Line, Op, Part, Reach, Repo, Wrapper};

const DEFAULT_SEED: u64 = 0x0508_0001;

#[derive(Clone, Copy)]
enum Expect {
    Green,
    /// Every line of the family shows a `tag` violation; `issue` owns it.
    Red {
        tag: &'static str,
        issue: &'static str,
    },
}

fn cases() -> u32 {
    std::env::var("DIFFCTX_HOOK_PROPERTY_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6)
}

fn seed() -> RngSeed {
    match std::env::var("DIFFCTX_HOOK_PROPERTY_SEED") {
        Ok(v) if v == "random" => RngSeed::Random,
        Ok(v) => v
            .parse::<u64>()
            .map_or(RngSeed::Fixed(DEFAULT_SEED), RngSeed::Fixed),
        Err(_) => RngSeed::Fixed(DEFAULT_SEED),
    }
}

fn family(name: &str, expect: Expect, lines: &BoxedStrategy<Line>) {
    let shells = hook::shells();
    assert!(!shells.is_empty(), "{name}: no bash on this machine");
    for shell in &shells {
        let config = Config {
            cases: cases(),
            max_shrink_iters: if matches!(expect, Expect::Red { .. }) {
                0
            } else {
                24
            },
            failure_persistence: None,
            rng_seed: seed(),
            ..Config::default()
        };
        let outcome = TestRunner::new(config).run(lines, |line| {
            let world = hook::world::World::new();
            let driven = hook::drive::drive(&world, shell, &line);
            hook::judge::judge(&world, &driven).map_err(TestCaseError::fail)
        });
        verdict(name, &shell.label, expect, outcome);
    }
}

fn verdict(name: &str, shell: &str, expect: Expect, outcome: Result<(), TestError<Line>>) {
    let failure = match outcome {
        Ok(()) => None,
        Err(TestError::Fail(reason, line)) => Some((reason.to_string(), line)),
        Err(TestError::Abort(reason)) => panic!("{name} [{shell}]: proptest aborted: {reason}"),
    };
    match (expect, failure) {
        (Expect::Green, None) => {}
        (Expect::Green, Some((reason, line))) => {
            panic!(
                "{name} [{shell}]: a line the hook must answer for is not answered for\n{reason}\nminimal line: {line:?}"
            )
        }
        (Expect::Red { .. }, Some((reason, _))) if reason.contains("HARNESS") => {
            panic!("{name} [{shell}]: the harness failed, which a red family would hide\n{reason}")
        }
        (Expect::Red { tag, issue }, Some((reason, line))) => {
            assert!(
                reason.contains(tag),
                "{name} [{shell}]: red for {issue} as {tag}, but the violation is another\n{reason}"
            );
            eprintln!("known red {name} [{shell}] {tag} {issue}: {line:?}");
        }
        (Expect::Red { tag, issue }, None) => panic!(
            "{name} [{shell}]: every line was answered for: the {tag} baseline entry for {issue} is stale, move the family to green"
        ),
    }
}

#[test]
fn commits_in_every_spelling_are_reviewed() {
    family(
        "commit_forms",
        Expect::Green,
        &hook::strategies::commit_forms(),
    );
}

#[test]
fn a_line_in_two_repositories_answers_for_both() {
    family(
        "two_repositories",
        Expect::Green,
        &hook::strategies::two_repositories(),
    );
}

#[test]
fn pushes_are_reviewed_as_git_printed_them() {
    family("push_forms", Expect::Green, &hook::strategies::push_forms());
}

#[test]
fn merges_picks_reverts_and_am_are_reviewed() {
    family(
        "integration_verbs",
        Expect::Green,
        &hook::strategies::integration_verbs(),
    );
}

#[test]
fn a_background_line_is_reviewed_at_the_next_event() {
    family(
        "background_lines",
        Expect::Green,
        &hook::strategies::background_lines(),
    );
}

#[test]
fn a_line_that_exits_non_zero_is_still_reviewed() {
    family(
        "failing_lines",
        Expect::Green,
        &hook::strategies::failing_lines(),
    );
}

#[test]
fn a_git_only_the_shell_runs_is_reviewed_or_reported() {
    family("hidden_git", Expect::Green, &hook::strategies::hidden_git());
}

#[test]
fn a_new_branch_on_a_pushed_one_reviews_only_its_own_commits() {
    family(
        "new_branches",
        Expect::Green,
        &hook::strategies::new_branches(),
    );
}

/// How the engine is stopped under a review: the compute deadline spent
/// before the first poll, cut at the n-th poll, or the n-th blob read failing.
#[derive(Clone, Debug)]
enum Stop {
    Expired,
    AfterPolls(u32),
    ReadFault(u32),
}

fn stops() -> BoxedStrategy<Stop> {
    prop_oneof![
        Just(Stop::Expired),
        (0u32..160).prop_map(Stop::AfterPolls),
        (0u32..60).prop_map(Stop::ReadFault),
    ]
    .boxed()
}

/// The words a review that did not finish uses for it.
const NAMED_REASONS: [&str; 5] = [
    "partial",
    "not determined",
    "did not review",
    "could not read",
    "stopped",
];

fn commit_payload(world: &hook::world::World, kind: &str, cwd: &std::path::Path) -> String {
    let mut payload = serde_json::json!({
        "session_id": "prop-session",
        "transcript_path": world.root().join("transcript.jsonl"),
        "cwd": cwd,
        "permission_mode": "default",
        "hook_event_name": kind,
        "tool_name": "Bash",
        "tool_input": { "command": "git commit -qm wide", "description": "property case" },
        "tool_use_id": "toolu_1",
    });
    if kind == "PostToolUse" {
        payload["tool_response"] = serde_json::json!({ "stdout": "", "stderr": "", "interrupted": false, "isImage": false });
    }
    payload.to_string()
}

/// What one hook event printed for the agent, and what the engine said on
/// stderr while it ran.
fn hook_under(
    world: &hook::world::World,
    event: &str,
    cwd: &std::path::Path,
    stdin: &str,
    stop: &Stop,
) -> (String, String) {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut command = world.command(hook::drive::hook_bin());
    command
        .args(["hook", event])
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match stop {
        Stop::Expired => command.env("DIFFCTX_TEST_DEADLINE_EXPIRED", "1"),
        Stop::AfterPolls(n) => command.env("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", n.to_string()),
        Stop::ReadFault(n) => command.env("DIFFCTX_TEST_READ_FAULT", n.to_string()),
    };
    let mut child = command.spawn().expect("HARNESS: spawn the hook");
    child
        .stdin
        .take()
        .expect("HARNESS: hook stdin")
        .write_all(stdin.as_bytes())
        .expect("HARNESS: feed the hook");
    let out = child
        .wait_with_output()
        .expect("HARNESS: wait for the hook");
    let doc: serde_json::Value =
        serde_json::from_slice(&out.stdout).unwrap_or(serde_json::Value::Null);
    let printed = ["additionalContext", "permissionDecisionReason"]
        .iter()
        .filter_map(|key| doc["hookSpecificOutput"][*key].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (printed, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A commit the engine could not review to the end is said to be unfinished,
/// with the reason: a deadline, a failed read, a failed engine each reach the
/// hook's output by name, and none of them reads as "reaches nothing" or as
/// silence.
#[test]
fn an_unfinished_review_reaches_the_output_with_its_reason() {
    let config = Config {
        cases: cases(),
        max_shrink_iters: 24,
        failure_persistence: None,
        rng_seed: seed(),
        ..Config::default()
    };
    let outcome = TestRunner::new(config).run(&stops(), |stop| {
        let world = hook::world::World::new();
        let work = world.path(Repo::A);
        std::fs::write(
            work.join("tracked.py"),
            "def total(items):\n    return sum(items) * 2\n",
        )
        .expect("HARNESS: write");
        world.git(&work, &["add", "tracked.py"]);
        let (pre, pre_err) = hook_under(
            &world,
            "pretooluse",
            &work,
            &commit_payload(&world, "PreToolUse", &work),
            &stop,
        );
        world.git(&work, &["commit", "-q", "-m", "wide"]);
        let (post, post_err) = hook_under(
            &world,
            "posttooluse",
            &work,
            &commit_payload(&world, "PostToolUse", &work),
            &stop,
        );
        let printed = format!("{pre}\n{post}");
        let struck = pre_err.contains("injected read fault") || post_err.contains("injected read fault");
        let finished = printed.contains("charge") && !NAMED_REASONS.iter().any(|w| printed.contains(w));
        let named = NAMED_REASONS.iter().any(|w| printed.contains(w));
        let expired = matches!(stop, Stop::Expired);
        if printed.trim().is_empty() {
            return Err(TestCaseError::fail(format!(
                "[silent] {stop:?}: a commit whose review did not finish printed nothing"
            )));
        }
        if (expired || struck) && !named {
            return Err(TestCaseError::fail(format!(
                "[unnamed] {stop:?}: the engine was stopped and the output names no reason:\n{printed}"
            )));
        }
        if (expired || struck) && printed.contains("reaches nothing outside the diff") {
            return Err(TestCaseError::fail(format!(
                "[clean] {stop:?}: a stopped review claims the change reaches nothing:\n{printed}"
            )));
        }
        if !named && !finished {
            return Err(TestCaseError::fail(format!(
                "[unfinished] {stop:?}: neither the review's callers nor a reason were printed:\n{printed}"
            )));
        }
        Ok(())
    });
    match outcome {
        Ok(()) => {}
        Err(TestError::Fail(reason, stop)) => {
            panic!("an unfinished review is not named\n{reason}\nminimal stop: {stop:?}")
        }
        Err(TestError::Abort(reason)) => panic!("proptest aborted: {reason}"),
    }
}

// What the hook rebuilt on git's record answers for (wave 3).

#[test]
fn a_commit_through_git_dir_from_elsewhere_is_reviewed() {
    family(
        "git_dir_elsewhere",
        Expect::Green,
        &hook::strategies::git_dir_elsewhere(),
    );
}

#[test]
fn a_rebase_is_reviewed() {
    family("rebases", Expect::Green, &hook::strategies::rebases());
}

/// A script that commits in the repository the call starts in is seen by
/// git's record of that repository, whatever the line shows.
#[test]
fn a_commit_inside_a_script_in_the_watched_repository_is_reviewed() {
    family("scripts_here", Expect::Green, &scripts_here());
}

#[test]
fn an_empty_commit_is_said_to_change_no_file() {
    family(
        "empty_commits",
        Expect::Green,
        &hook::strategies::empty_commits(),
    );
}

#[test]
fn a_force_push_reviews_what_the_push_changed() {
    family(
        "force_pushes",
        Expect::Green,
        &hook::strategies::force_pushes(),
    );
}

#[test]
fn a_push_over_a_peer_commit_says_it_publishes_it() {
    family(
        "pushes_over_a_peer_commit",
        Expect::Green,
        &hook::strategies::pushes_over_a_peer_commit(),
    );
}

#[test]
fn a_peer_commit_in_the_window_is_read_as_changed_during_the_call() {
    family(
        "peer_commits_in_the_window",
        Expect::Green,
        &hook::strategies::peer_commits_in_the_window(),
    );
}

/// A script the call starts from another directory commits in a repository
/// that no cwd, no `cd`, no `-C` and no `--git-dir` names, and that no earlier
/// call of the session saw: git has no record of it that the hook can open,
/// and nothing says so. The one baseline entry left.
#[test]
fn red_a_script_commits_in_a_repository_the_call_does_not_name() {
    family(
        "scripts_elsewhere",
        Expect::Red {
            tag: "[landed]",
            issue: "fj#127",
        },
        &scripts_elsewhere(),
    );
}

fn wrappers() -> BoxedStrategy<Wrapper> {
    prop_oneof![Just(Wrapper::Bash), Just(Wrapper::Exec)].boxed()
}

fn repos() -> BoxedStrategy<Repo> {
    prop_oneof![Just(Repo::A), Just(Repo::B)].boxed()
}

fn script_line(cwd: Cwd, repo: Repo, wrapper: Wrapper) -> Line {
    Line::new(
        cwd,
        vec![Part::new(repo, Reach::Here, Op::Script(wrapper))],
        vec![Join::And],
    )
}

fn scripts_here() -> BoxedStrategy<Line> {
    (repos(), wrappers())
        .prop_map(|(repo, wrapper)| script_line(Cwd::In(repo), repo, wrapper))
        .boxed()
}

fn scripts_elsewhere() -> BoxedStrategy<Line> {
    (repos(), wrappers(), any::<bool>())
        .prop_map(|(repo, wrapper, neutral)| {
            let other = if repo == Repo::A { Repo::B } else { Repo::A };
            let cwd = if neutral {
                Cwd::Neutral
            } else {
                Cwd::In(other)
            };
            script_line(cwd, repo, wrapper)
        })
        .boxed()
}
