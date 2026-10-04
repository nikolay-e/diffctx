// `diffctx hook pretooluse` / `posttooluse` through the binary, on real git
// repositories, with the payloads Claude Code sends. The contract under test
// is the one a hook lives or dies by: the right answer when there is one,
// silence and exit 0 in every other case.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

/// `pricing.total` is edited in the working tree; `checkout.charge` calls it
/// from another file and `test_checkout.py` pins `charge`.
fn repo_with_pending_change() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return sum(i.price for i in items)\n",
    );
    write(
        repo,
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return cart.pay(total(cart.items))\n",
    );
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 1\n",
    );
    write(
        repo,
        "shop/shelf.py",
        "from shop.inventory import restock\n\n\ndef refill(items):\n    return [restock(i) for i in items]\n",
    );
    write(
        repo,
        "tests/test_checkout.py",
        "from shop.checkout import charge\n\n\ndef test_charge_pays_the_total(cart):\n    assert charge(cart) == 30\n",
    );
    write(repo, "README.md", "# shop\n");
    commit_all(repo, "initial");
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    );
    tmp
}

struct Hook {
    stdout: String,
    code: Option<i32>,
}

fn hook_with(repo: &Path, cache: &Path, event: &str, extra: &[&str], stdin: &str) -> Hook {
    let mut child = Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache)
        .args(["hook", event])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    Hook {
        stdout: String::from_utf8(out.stdout).unwrap(),
        code: out.status.code(),
    }
}

fn hook(repo: &Path, cache: &Path, stdin: &str) -> Hook {
    hook_with(repo, cache, "pretooluse", &[], stdin)
}

fn payload_for(event: &str, repo: &Path, command: &str) -> String {
    serde_json::json!({
        "session_id": "s1",
        "hook_event_name": event,
        "tool_name": "Bash",
        "cwd": repo.to_string_lossy(),
        "tool_input": {"command": command}
    })
    .to_string()
}

fn payload(repo: &Path, command: &str) -> String {
    payload_for("PreToolUse", repo, command)
}

fn output_of(h: &Hook, event: &str) -> serde_json::Value {
    let doc: serde_json::Value = serde_json::from_str(h.stdout.trim()).expect("hook JSON");
    assert_eq!(doc["hookSpecificOutput"]["hookEventName"], event);
    doc["hookSpecificOutput"].clone()
}

fn context_of(h: &Hook) -> String {
    output_of(h, "PreToolUse")["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

fn payload_in_session(session: &str, repo: &Path, command: &str) -> String {
    let mut doc: serde_json::Value = serde_json::from_str(&payload(repo, command)).unwrap();
    doc["session_id"] = serde_json::Value::String(session.to_string());
    doc.to_string()
}

#[test]
fn a_change_one_session_was_shown_is_news_to_the_next() {
    // The marker used to be keyed by content alone, so a second session
    // committing content the first had reviewed got silence (#323).
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let cache = TempDir::new().unwrap();
    let first = hook(
        repo,
        cache.path(),
        &payload_in_session("one", repo, "git commit -am vat"),
    );
    assert!(context_of(&first).contains("shop/checkout.py::charge"));
    let again = hook(
        repo,
        cache.path(),
        &payload_in_session("one", repo, "git commit -am vat"),
    );
    assert_eq!(again.stdout, "", "the same session is not told twice");
    let other = hook(
        repo,
        cache.path(),
        &payload_in_session("two", repo, "git commit -am vat"),
    );
    assert!(
        context_of(&other).contains("shop/checkout.py::charge"),
        "{}",
        other.stdout
    );
}

#[test]
fn a_manual_impact_run_just_before_still_counts() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let cache = TempDir::new().unwrap();
    let manual = Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache.path())
        .args(["--diff", "HEAD", "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(manual.status.success());
    let h = hook(
        repo,
        cache.path(),
        &payload_in_session("fresh", repo, "git commit -am vat"),
    );
    assert_reminder(&h);
}

/// A commit of a change whose impact the agent already read: one line that
/// names what was shown, never the listing again and never silence (#346).
fn assert_reminder(h: &Hook) {
    assert_eq!(h.code, Some(0));
    let context = output_of(h, "PreToolUse")["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        context.starts_with("diffctx: this command records a change whose impact was"),
        "{context}"
    );
    assert!(
        context.contains("1 caller(s) outside the diff"),
        "{context}"
    );
    assert!(!context.contains("shop/checkout.py::charge"), "{context}");
    assert_eq!(context.lines().count(), 1, "{context}");
}

#[test]
fn a_commit_with_an_outside_caller_gets_the_impact_once() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let first = hook(repo, cache.path(), &payload(repo, "git commit -am 'vat'"));
    assert_eq!(first.code, Some(0));
    let context = context_of(&first);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        context.contains("reachable from tests: tests/test_checkout.py"),
        "{context}"
    );
    assert!(context.contains("Check these callers"), "{context}");
    assert!(context.chars().count() <= 9_200, "{}", context.len());
    let out = output_of(&first, "PreToolUse");
    assert!(
        out.get("permissionDecision").is_none(),
        "no gate by default"
    );

    let again = hook(repo, cache.path(), &payload(repo, "git commit -am 'vat'"));
    assert_eq!(again.code, Some(0));
    assert_eq!(again.stdout, "", "the same content is reviewed once");
}

#[test]
fn a_plain_commit_reviews_the_index_not_the_working_tree() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    // pricing is staged; inventory is edited but NOT staged.
    git(repo, &["add", "shop/pricing.py"]);
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 10\n",
    );
    let h = hook(repo, cache.path(), &payload(repo, "git commit -m 'vat'"));
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        !context.contains("shop/shelf.py"),
        "the unstaged inventory edit is not part of this commit: {context}"
    );

    // `-a` takes the working tree, and the inventory caller with it.
    let cache2 = TempDir::new().unwrap();
    let h = hook(repo, cache2.path(), &payload(repo, "git commit -am 'vat'"));
    let context = context_of(&h);
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

#[test]
fn a_manual_impact_run_counts_as_the_review() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let status = Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache.path())
        .args(["--mode", "impact", "-q"])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let h = hook(repo, cache.path(), &payload(repo, "git commit -am 'vat'"));
    assert_reminder(&h);
    let again = hook(repo, cache.path(), &payload(repo, "git commit -am 'vat'"));
    assert_eq!(
        again.stdout, "",
        "a second commit attempt of the same change"
    );
}

#[test]
fn a_change_nothing_depends_on_is_silence() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "README.md", "# shop\n\nVAT included.\n");
    let h = hook(repo, cache.path(), &payload(repo, "git commit -am docs"));
    assert_eq!(h.code, Some(0));
    assert_eq!(h.stdout, "");
}

#[test]
fn a_push_names_the_range_since_upstream() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "."]);
    git(
        repo,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(repo, &["push", "-q", "-u", "origin", "HEAD"]);
    commit_all(repo, "vat");
    let h = hook(repo, cache.path(), &payload(repo, "git push"));
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

#[test]
fn a_push_without_an_upstream_falls_back_to_origin_main() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "."]);
    git(
        repo,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
    // No tracking branch, and no origin/HEAD after a plain `remote add`.
    commit_all(repo, "vat");
    let h = hook(repo, cache.path(), &payload(repo, "git push origin HEAD"));
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

#[test]
fn a_push_with_no_remote_at_all_is_silence() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    commit_all(repo, "vat");
    let h = hook(repo, cache.path(), &payload(repo, "git push origin main"));
    assert_eq!(h.code, Some(0));
    assert_eq!(h.stdout, "");
}

#[test]
fn a_merge_reviews_the_branch_not_our_own_commits() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "-b", "topic"]);
    commit_all(repo, "vat on topic");
    git(repo, &["checkout", "-q", "-"]);
    // Diverge: main edits inventory, which topic never touched.
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 10\n",
    );
    commit_all(repo, "restock more");
    let h = hook(repo, cache.path(), &payload(repo, "git merge topic"));
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        !context.contains("shop/shelf.py"),
        "our own inventory commit is not what the merge brings in: {context}"
    );
}

#[test]
fn a_cherry_pick_reviews_the_picked_commit() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "-b", "topic"]);
    commit_all(repo, "vat on topic");
    git(repo, &["checkout", "-q", "-"]);
    let h = hook(repo, cache.path(), &payload(repo, "git cherry-pick topic"));
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

#[test]
fn a_git_diff_the_agent_ran_gets_the_impact_after_it() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let h = hook_with(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &payload_for("PostToolUse", repo, "git diff"),
    );
    assert_eq!(h.code, Some(0));
    let out = output_of(&h, "PostToolUse");
    let context = out["additionalContext"].as_str().unwrap();
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(out.get("permissionDecision").is_none());

    // The commit that follows is the same content: a reminder, not a repeat.
    let again = hook(repo, cache.path(), &payload(repo, "git commit -am vat"));
    assert_reminder(&again);

    // A historical diff is not the pending change — with a real HEAD~1 and
    // a fresh pending edit, so the silence comes from the verb, not from an
    // unresolvable revision.
    commit_all(repo, "vat");
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    let cache2 = TempDir::new().unwrap();
    let h = hook_with(
        repo,
        cache2.path(),
        "posttooluse",
        &[],
        &payload_for("PostToolUse", repo, "git diff HEAD~1..HEAD"),
    );
    assert_eq!(h.stdout, "");
    let h = hook_with(
        repo,
        cache2.path(),
        "posttooluse",
        &[],
        &payload_for("PostToolUse", repo, "git diff"),
    );
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        h.stdout
    );
}

#[test]
fn one_change_through_status_diff_add_and_commit_is_shown_once() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    // Untracked files make the worktree diff and the index diff different
    // patches; the answer for the reader is the same.
    write(repo, "NOTES.txt", "scratch\n");
    let shown = hook_with(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &payload_for("PostToolUse", repo, "git status"),
    );
    assert!(
        shown.stdout.contains("shop/checkout.py::charge"),
        "{}",
        shown.stdout
    );
    git(repo, &["add", "-A"]);
    for command in ["git diff --cached", "git status"] {
        let again = hook_with(
            repo,
            cache.path(),
            "posttooluse",
            &[],
            &payload_for("PostToolUse", repo, command),
        );
        assert_eq!(again.stdout, "", "{command} repeated the same answer");
    }
    let commit = hook(repo, cache.path(), &payload(repo, "git commit -m vat"));
    assert_reminder(&commit);
}

#[test]
fn an_amend_reviews_the_index_onto_the_parent() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    commit_all(repo, "vat");
    write(repo, "docs/NOTES.md", "fixup\n");
    git(repo, &["add", "docs/NOTES.md"]);
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git commit -q --amend --no-edit"),
    );
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        h.stdout
    );
}

#[test]
fn the_strict_gate_denies_once_with_the_impact_attached() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let first = hook_with(
        repo,
        cache.path(),
        "pretooluse",
        &["--gate"],
        &payload(repo, "git commit -am vat"),
    );
    assert_eq!(first.code, Some(0));
    let out = output_of(&first, "PreToolUse");
    assert_eq!(out["permissionDecision"], "deny");
    assert!(
        out["additionalContext"]
            .as_str()
            .unwrap()
            .contains("shop/checkout.py::charge")
    );
    assert!(
        out["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("shop/checkout.py::charge")
    );
    let retry = hook_with(
        repo,
        cache.path(),
        "pretooluse",
        &["--gate"],
        &payload(repo, "git commit -am vat"),
    );
    assert_eq!(retry.stdout, "", "reviewed once: the retry passes");

    // Nothing to review, nothing to gate.
    let cache2 = TempDir::new().unwrap();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "README.md", "# shop\n\nVAT.\n");
    let docs = hook_with(
        repo,
        cache2.path(),
        "pretooluse",
        &["--gate"],
        &payload(repo, "git commit -am docs"),
    );
    assert_eq!(docs.stdout, "");
}

#[test]
fn everything_else_is_silence_with_exit_zero() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    for stdin in [
        "",
        "not json",
        "{}",
        &payload(repo, "git status"),
        &payload(repo, "git merge --abort"),
        &payload(repo, "git commit --amend --no-edit"),
        &payload(repo, "cargo test"),
    ] {
        let h = hook(repo, cache.path(), stdin);
        assert_eq!(h.code, Some(0), "{stdin}");
        assert_eq!(h.stdout, "", "{stdin}");
    }
    let outside = TempDir::new().unwrap();
    let h = hook(
        outside.path(),
        cache.path(),
        &payload(outside.path(), "git commit -am x"),
    );
    assert_eq!(h.code, Some(0));
    assert_eq!(h.stdout, "", "not a repository");
}

#[test]
fn the_git_dash_c_directory_wins_over_cwd() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let elsewhere = TempDir::new().unwrap();
    let command = format!("git -C {} commit -am vat", repo.to_string_lossy());
    let h = hook(
        elsewhere.path(),
        cache.path(),
        &payload(elsewhere.path(), &command),
    );
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

fn search_payload(repo: &Path, tool: &str, query: &str) -> String {
    let tool_input = if tool == "Grep" {
        serde_json::json!({"pattern": query})
    } else {
        serde_json::json!({"command": query})
    };
    serde_json::json!({
        "session_id": "s1",
        "hook_event_name": "PostToolUse",
        "tool_name": tool,
        "cwd": repo.to_string_lossy(),
        "tool_input": tool_input
    })
    .to_string()
}

#[test]
fn a_search_for_a_changed_name_is_answered_with_its_callers() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let search = |tool: &str, query: &str| {
        hook_with(
            repo,
            cache.path(),
            "posttooluse",
            &[],
            &search_payload(repo, tool, query),
        )
    };
    let h = search("Bash", r"rg -n 'def total\(' shop/");
    assert_eq!(h.code, Some(0));
    let context = output_of(&h, "PostToolUse")["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(context.contains("`total`"), "{context}");
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("Contracts"), "{context}");
    assert_eq!(search("Grep", "total").stdout, "", "once per name");
    assert_eq!(search("Bash", "grep -rn restock .").stdout, "");

    // The search answer does not stand in for the commit's review.
    let commit = hook(repo, cache.path(), &payload(repo, "git commit -am vat"));
    assert!(context_of(&commit).contains("shop/checkout.py::charge"));

    let log = std::fs::read_to_string(cache.path().join("diffctx/hook.log")).unwrap();
    let outcomes: Vec<&str> = log.lines().map(|l| l.split(' ').nth(5).unwrap()).collect();
    assert_eq!(outcomes, ["shown", "seen", "no-symbol", "shown"], "{log}");
    assert!(
        log.lines().last().unwrap().contains(" commit HEAD shown "),
        "{log}"
    );
}

fn inspect(repo: &Path, cache: &Path, command: &str) -> Hook {
    hook_with(
        repo,
        cache,
        "posttooluse",
        &[],
        &payload_for("PostToolUse", repo, command),
    )
}

fn answered(h: &Hook) -> bool {
    h.stdout.contains("shop/checkout.py::charge")
}

#[test]
fn an_unchanged_inspection_is_answered_once_and_any_new_byte_answers_again() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    assert!(answered(&inspect(repo, cache.path(), "git diff")));
    assert_eq!(
        inspect(repo, cache.path(), "git diff").stdout,
        "",
        "unchanged identity"
    );
    // Same file, new content, seconds later: a new identity, answered now —
    // no ten-minute window hides it.
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.21, 2)\n",
    );
    assert!(answered(&inspect(repo, cache.path(), "git diff")));
    // A change to a caller's file is a change to the answer's input too.
    write(
        repo,
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return cart.pay(total(cart.items) + 0)\n",
    );
    let h = inspect(repo, cache.path(), "git diff");
    assert!(
        h.stdout.contains("diffctx"),
        "the caller's edit is a new identity: {}",
        h.stdout
    );
}

#[test]
fn a_view_the_agent_does_not_read_whole_is_no_inspection() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    for (command, expected) in [
        ("git diff | grep total", false),
        ("git diff |& head -5", false),
        ("git status | head", false),
        ("git diff --stat", false),
        ("git diff --name-only", false),
        ("git diff --quiet", false),
        ("git diff --stat --patch", true),
        ("git diff --exit-code", true),
        ("git diff || true", true),
        ("echo start | true && git diff", true),
        ("git diff -- 'shop|other'", true),
        ("git diff 2>&1", true),
    ] {
        let cache = TempDir::new().unwrap();
        let h = inspect(repo, cache.path(), command);
        assert_eq!(answered(&h), expected, "{command}: {}", h.stdout);
        assert_eq!(h.code, Some(0), "{command}");
    }
}

#[test]
fn a_reminder_needs_the_same_snapshot_and_a_different_one_is_answered() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    // Inspected: the working tree, with an unstaged edit to a second file.
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 2\n",
    );
    assert!(answered(&inspect(repo, cache.path(), "git diff")));
    // Committed: the index, which holds only pricing.py — not what was shown.
    git(repo, &["add", "shop/pricing.py"]);
    let commit = hook(repo, cache.path(), &payload(repo, "git commit -m vat"));
    let context = context_of(&commit);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shown earlier"), "{context}");
}

#[test]
fn another_session_or_worktree_gets_its_own_answer() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    assert!(answered(&inspect(repo, cache.path(), "git diff")));
    let mut other: serde_json::Value =
        serde_json::from_str(&payload_for("PostToolUse", repo, "git diff")).unwrap();
    other["session_id"] = serde_json::Value::String("s2".to_string());
    let h = hook_with(repo, cache.path(), "posttooluse", &[], &other.to_string());
    assert!(answered(&h), "{}", h.stdout);
    let wt_parent = TempDir::new().unwrap();
    let wt = wt_parent.path().join("wt");
    git(repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    write(
        &wt,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    );
    let h = inspect(&wt, cache.path(), "git diff");
    assert!(answered(&h), "a worktree is another checkout: {}", h.stdout);
}

#[test]
fn concurrent_hooks_leave_one_answer_and_whole_markers() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path().to_path_buf();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let (repo, cache) = (repo.clone(), cache.path().to_path_buf());
            std::thread::spawn(move || inspect(&repo, &cache, "git diff"))
        })
        .collect();
    let results: Vec<Hook> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.iter().all(|h| h.code == Some(0)));
    assert!(results.iter().any(answered));
    assert_eq!(
        inspect(&repo, cache.path(), "git diff").stdout,
        "",
        "delivered once"
    );
    for entry in std::fs::read_dir(cache.path().join("diffctx").join("seen")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().to_string();
        assert!(
            !name.contains(".tmp"),
            "a half-written marker survived: {name}"
        );
    }
}

#[test]
fn a_partial_answer_is_retried_a_bounded_number_of_times() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let run = || {
        let mut child = Command::new(&*BIN)
            .current_dir(repo)
            .env("DIFFCTX_CACHE_DIR", cache.path())
            .env("DIFFCTX_TEST_DEADLINE_EXPIRED", "1")
            .args(["hook", "posttooluse"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload_for("PostToolUse", repo, "git diff").as_bytes())
            .unwrap();
        String::from_utf8(child.wait_with_output().unwrap().stdout).unwrap()
    };
    let shown: Vec<bool> = (0..4).map(|_| run().contains("partial")).collect();
    assert_eq!(
        shown,
        vec![true, true, false, false],
        "partial, retried once, then left alone"
    );
}

#[test]
fn a_commit_previewed_from_the_working_tree_is_checked_once_it_lands() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let pre = hook(
        repo,
        cache.path(),
        &payload(repo, "git add -A && git commit -m vat"),
    );
    assert!(
        context_of(&pre).starts_with("diffctx previewed"),
        "{}",
        context_of(&pre)
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "vat"]);
    let post = inspect(repo, cache.path(), "git add -A && git commit -m vat");
    assert_eq!(post.stdout, "", "the commit is what was previewed");

    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.5, 2)\n",
    );
    let pre = hook(repo, cache.path(), &payload(repo, "git commit -am more"));
    assert!(context_of(&pre).starts_with("diffctx previewed"));
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.7, 2)\n",
    );
    git(repo, &["commit", "-qam", "more"]);
    let post = inspect(repo, cache.path(), "git commit -am more");
    assert!(
        post.stdout.contains("commit that just landed"),
        "the commit differs from the preview: {}",
        post.stdout
    );
}

#[test]
fn a_git_command_hidden_in_a_subshell_is_logged_as_unsupported() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "sh -c 'git commit -am vat'"),
    );
    assert_eq!(h.stdout, "");
    let log = std::fs::read_to_string(cache.path().join("diffctx").join("hook.log")).unwrap();
    assert!(log.contains("unsupported-syntax"), "{log}");
}

#[test]
fn the_automatic_answer_fits_its_token_cap() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    for i in 0..40 {
        write(
            repo,
            &format!("shop/user_{i}.py"),
            &format!(
                "from shop.pricing import total\n\n\ndef use_{i}(cart):\n    return total(cart.items) + {i}\n"
            ),
        );
    }
    commit_all(repo, "users");
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.3, 2)\n",
    );
    let h = inspect(repo, cache.path(), "git diff");
    let out = output_of(&h, "PostToolUse");
    let context = out["additionalContext"].as_str().unwrap();
    let body = context
        .split_once("What it reaches outside the diff:\n")
        .map(|(_, b)| b)
        .unwrap()
        .rsplit_once("\nCheck these callers")
        .map(|(b, _)| b)
        .unwrap();
    let words = body.split_whitespace().count();
    assert!(words < 600, "the automatic answer is {words} words: {body}");
    assert!(body.contains("omitted to stay under 600 tokens"), "{body}");
}
