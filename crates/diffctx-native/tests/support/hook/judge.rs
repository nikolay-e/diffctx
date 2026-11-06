//! The verdict on one driven line. Every violation carries a tag, which the
//! red baseline in `hook_property.rs` names:
//!
//! * `[contract]` — a run exits non-zero or prints anything but the hook JSON;
//! * `[landed]` — a commit the line recorded, or a branch it pushed, is in no
//!   range the hook reviewed, and the hook did not say it did not review;
//! * `[own]` — the hook reviewed, or a push published, commits the line did
//!   not make, and nothing says so;
//! * `[force-delta]` — a force-push is reviewed as the whole rewritten commit
//!   instead of what changed between the two tips.
//!
//! What was recorded comes from git's refs and the bare remote, never from the
//! text of the line; what was reviewed comes only from what the hook printed.

use std::collections::BTreeSet;

use regex::Regex;

use super::drive::{Driven, covered_files};
use super::line::Repo;
use super::world::{PEER_BRANCH, RepoState, World};

/// The wording that tells the agent a range holds more than its own work
/// (T19, gh#669).
const HEDGES: [&str; 2] = ["during this call", "did not make"];

pub fn judge(world: &World, driven: &Driven) -> Result<(), String> {
    let printed = driven.printed();
    let ranges = ranges_in(&printed);
    let mut found: Vec<String> = Vec::new();
    for run in &driven.runs {
        if run.code != Some(0) {
            found.push(format!("[contract] {} exited {:?}", run.event, run.code));
        }
        let text = run.stdout.trim();
        let json = serde_json::from_str::<serde_json::Value>(text).ok();
        if !text.is_empty() && json.is_none_or(|doc| doc["hookSpecificOutput"].is_null()) {
            found.push(format!("[contract] {} printed {text:?}", run.event));
        }
    }
    for repo in Repo::BOTH {
        check(world, driven, repo, &printed, &ranges, &mut found);
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(format!(
        "{}\n{}\n  {}\nline ({}): {}\nprinted:\n{printed}\nlog:\n{}",
        found.join("\n"),
        "-".repeat(8),
        driven
            .runs
            .iter()
            .map(|r| format!("{} -> {:?}", r.event, r.code))
            .collect::<Vec<_>>()
            .join(", "),
        driven.shell,
        driven.command,
        driven.log
    ))
}

type Range = (String, String);

fn ranges_in(printed: &str) -> Vec<Range> {
    let pattern = Regex::new(r"\b([0-9a-f]{7,40})\.\.([0-9a-f]{7,40})\b").expect("range pattern");
    pattern
        .captures_iter(printed)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

fn check(
    world: &World,
    driven: &Driven,
    repo: Repo,
    printed: &str,
    ranges: &[Range],
    found: &mut Vec<String>,
) {
    let (before, after) = (&driven.before[&repo], &driven.after[&repo]);
    let peers = driven.peer_commits.get(&repo).cloned().unwrap_or_default();
    let created: BTreeSet<String> = after
        .all
        .difference(&before.all)
        .filter(|c| !after.remotes.contains(*c) && !peers.contains(*c))
        .cloned()
        .collect();
    let pushed = pushed_refs(before, after);
    let published = published_commits(world, repo, before, &pushed);
    let hedged = HEDGES.iter().any(|h| printed.contains(h));
    let say = |what: &str| format!("{what} ({}, repository {})", driven.shell, repo.name());

    let mut landed: Vec<(String, String)> = Vec::new();
    if let Some(commit) = landed_commit(world, repo, &after.head, &created) {
        landed.push(("commit".into(), commit));
    }
    landed.extend(
        pushed
            .iter()
            .map(|(name, _, new)| (format!("push of {name}"), new.clone())),
    );
    for (what, oid) in &landed {
        if !reviewed(world, repo, ranges, oid) && !stated(world, repo, printed, ranges) {
            found.push(say(&format!(
                "[landed] the {what} at {oid} is in no reviewed range and not reported unreviewed"
            )));
        }
    }

    let mut allowed: BTreeSet<String> = created.union(&published).cloned().collect();
    if before.head != after.head {
        let path = world.path(repo);
        let args = [
            "rev-list",
            after.head.as_str(),
            "--not",
            before.head.as_str(),
        ];
        allowed.extend(
            world
                .try_git(&path, &args)
                .unwrap_or_default()
                .lines()
                .map(str::to_string),
        );
    }
    allowed.retain(|c| !peers.contains(c));
    for (x, y) in ranges {
        let (Some(x), Some(y)) = (world.commit_of(repo, x), world.commit_of(repo, y)) else {
            continue;
        };
        let listed = world
            .try_git(&world.path(repo), &["rev-list", &format!("{x}..{y}")])
            .unwrap_or_default();
        let foreign: Vec<&str> = listed.lines().filter(|c| !allowed.contains(*c)).collect();
        if !foreign.is_empty() && !hedged {
            found.push(say(&format!(
                "[own] the reviewed range {x}..{y} holds {} commit(s) the line did not make: {}",
                foreign.len(),
                foreign.join(" ")
            )));
        }
    }
    let strangers: Vec<&String> = published.intersection(&peers).collect();
    if !strangers.is_empty() && !hedged {
        found.push(say(&format!(
            "[own] the push published {} commit(s) the line did not make",
            strangers.len()
        )));
    }

    let mut covered_once: BTreeSet<&Range> = BTreeSet::new();
    for (name, old, new) in &pushed {
        let Some(old) = old else { continue };
        let bare = world.bare(repo);
        if world
            .try_git(&bare, &["merge-base", "--is-ancestor", old, new])
            .is_some()
        {
            continue;
        }
        let delta: BTreeSet<String> = world
            .try_git(&bare, &["diff", "--name-only", old, new])
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect();
        let want = world.tree_of(repo, new);
        for range in ranges {
            if want.is_none()
                || world.tree_of(repo, &range.1) != want
                || !covered_once.insert(range)
            {
                continue;
            }
            let covered = covered_files(world, repo, &format!("{}..{}", range.0, range.1));
            if covered != delta {
                found.push(say(&format!(
                    "[force-delta] the force-push of {name} changed {delta:?} between the tips; the engine reads the reviewed range {}..{} as {covered:?}",
                    range.0, range.1
                )));
            }
        }
    }
}

/// The remote branches the line moved: name, value before, value after.
fn pushed_refs(before: &RepoState, after: &RepoState) -> Vec<(String, Option<String>, String)> {
    after
        .remote_heads
        .iter()
        .filter(|(name, new)| {
            name.as_str() != PEER_BRANCH && before.remote_heads.get(*name) != Some(*new)
        })
        .map(|(name, new)| {
            (
                name.clone(),
                before.remote_heads.get(name).cloned(),
                new.clone(),
            )
        })
        .collect()
}

/// What the pushes added to the remote: the commits it did not have.
fn published_commits(
    world: &World,
    repo: Repo,
    before: &RepoState,
    pushed: &[(String, Option<String>, String)],
) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, _, new) in pushed {
        let mut args = vec!["rev-list", new.as_str()];
        if !before.remote_heads.is_empty() {
            args.push("--not");
            args.extend(before.remote_heads.values().map(String::as_str));
        }
        out.extend(
            world
                .try_git(&world.bare(repo), &args)
                .unwrap_or_default()
                .lines()
                .map(str::to_string),
        );
    }
    out
}

/// The newest commit the line made on `HEAD`'s first-parent chain.
fn landed_commit(
    world: &World,
    repo: Repo,
    head: &str,
    created: &BTreeSet<String>,
) -> Option<String> {
    if created.is_empty() {
        return None;
    }
    let chain = world.try_git(
        &world.path(repo),
        &["rev-list", "--first-parent", "-n", "50", head],
    )?;
    chain
        .lines()
        .find(|c| created.contains(*c))
        .map(str::to_string)
}

/// `oid` is in a range the hook printed, or a printed range ends at its tree.
fn reviewed(world: &World, repo: Repo, ranges: &[Range], oid: &str) -> bool {
    let want = world.tree_of(repo, oid);
    ranges.iter().any(|(x, y)| {
        if want.is_some() && world.tree_of(repo, y) == want {
            return true;
        }
        let (Some(x), Some(y)) = (world.commit_of(repo, x), world.commit_of(repo, y)) else {
            return false;
        };
        world
            .try_git(&world.path(repo), &["rev-list", &format!("{x}..{y}")])
            .is_some_and(|listed| listed.lines().any(|c| c == oid))
    })
}

/// The hook said, in words, that it did not review what was recorded here.
fn stated(world: &World, repo: Repo, printed: &str, ranges: &[Range]) -> bool {
    let resolves = || {
        ranges
            .iter()
            .any(|(_, y)| world.commit_of(repo, y).is_some())
    };
    printed.contains("did not analyse this command")
        || (printed.contains("did not review") && resolves())
        || (printed.contains("other pushed ref(s) are not analysed") && resolves())
}
