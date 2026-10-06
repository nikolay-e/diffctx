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
    hook_in_env(repo, cache, event, extra, &[], stdin)
}

fn hook_in_env(
    repo: &Path,
    cache: &Path,
    event: &str,
    extra: &[&str],
    env: &[(&str, &Path)],
    stdin: &str,
) -> Hook {
    let mut child = Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache)
        .envs(env.iter().copied())
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

/// The picked commits' patch reaches `git apply` byte for byte: decoded as
/// UTF-8 first, a Latin-1 file no longer applied and the pick went unreviewed.
#[test]
fn a_pick_of_several_commits_with_a_latin1_file_is_reviewed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["stash", "-q"]);
    std::fs::write(repo.join("shop/names.py"), b"NAME = '\xe9t\xe9'\n").unwrap();
    commit_all(repo, "names");
    git(repo, &["checkout", "-q", "-b", "topic"]);
    std::fs::write(repo.join("shop/names.py"), b"NAME = '\xe9t\xe9 2'\n").unwrap();
    commit_all(repo, "names on topic");
    git(repo, &["stash", "pop", "-q"]);
    commit_all(repo, "vat on topic");
    git(repo, &["checkout", "-q", "-"]);
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git cherry-pick topic~1 topic"),
    );
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
    let last = log.lines().last().unwrap();
    assert!(
        last.contains(" commit ") && last.contains(" shown "),
        "{log}"
    );
    assert!(
        !last.contains(" HEAD "),
        "`-a` is planned as a tree, not read off the working tree: {log}"
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

/// A path `git hash-object` refuses — an untracked nested repository —
/// used to fail the whole identity, logged as "clean", and the hook went
/// quiet on the real change beside it.
#[test]
fn a_nested_repository_in_the_working_tree_does_not_silence_the_answer() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let nested = repo.join("vendor/dep");
    std::fs::create_dir_all(&nested).unwrap();
    git(&nested, &["init", "-q", "."]);
    git(&nested, &["config", "user.email", "test@example.com"]);
    git(&nested, &["config", "user.name", "diffctx tests"]);
    write(&nested, "lib.py", "X = 1\n");
    commit_all(&nested, "dep");
    let h = inspect(repo, cache.path(), "git diff");
    assert!(answered(&h), "{}", h.stdout);
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

fn last_log_line(cache: &Path) -> String {
    let log = std::fs::read_to_string(cache.join("diffctx").join("hook.log")).unwrap();
    log.lines().last().unwrap().to_string()
}

/// An unrelated edit beside the pending one, the shape of a worktree another
/// session shares: `inventory.restock` changes, `shelf.refill` calls it.
fn add_unrelated_edit(repo: &Path) {
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 10\n",
    );
}

#[test]
fn an_add_and_commit_on_one_line_reviews_only_what_the_add_stages() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    add_unrelated_edit(repo);
    let command = "git add shop/pricing.py && git commit -q -F - <<'EOF'\nvat: don't round twice; keep cents | all of them\n\nbody `x` & more\nEOF";
    let h = hook(repo, cache.path(), &payload(repo, command));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        !context.contains("shop/shelf.py"),
        "the unstaged inventory edit is not in this commit: {context}"
    );
    assert!(!context.contains("uncommitted changes"), "{context}");
    let line = last_log_line(cache.path());
    assert!(!line.contains(" HEAD "), "{line}");
}

#[test]
fn a_pathspec_commit_reviews_only_its_paths() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    add_unrelated_edit(repo);
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git commit -m vat -- shop/pricing.py"),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/shelf.py"), "{context}");
    assert!(!context.contains("uncommitted changes"), "{context}");

    // `-i` adds the paths to what is already staged.
    git(repo, &["add", "shop/inventory.py"]);
    let cache = TempDir::new().unwrap();
    write(repo, "README.md", "# shop, again\n");
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git commit -m vat -i shop/pricing.py"),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

#[test]
fn a_home_relative_cd_reaches_the_repository() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let home = repo.parent().unwrap();
    let name = repo.file_name().unwrap().to_string_lossy();
    let elsewhere = TempDir::new().unwrap();
    let command = format!(
        "cd ~/{name} && FP=$(cat README.md) && git add shop/pricing.py && git commit -q -m x -- shop/pricing.py"
    );
    let h = hook_in_env(
        elsewhere.path(),
        cache.path(),
        "pretooluse",
        &[],
        &[("HOME", home)],
        &payload(elsewhere.path(), &command),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    let line = last_log_line(cache.path());
    assert!(!line.contains("unsupported-syntax"), "{line}");
}

#[test]
fn a_cherry_pick_of_several_commits_reviews_them_all() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "-b", "topic"]);
    commit_all(repo, "vat on topic");
    add_unrelated_edit(repo);
    commit_all(repo, "restock on topic");
    git(repo, &["checkout", "-q", "-"]);
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git cherry-pick -x topic~1 topic"),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

#[test]
fn a_staged_amend_on_one_line_reviews_onto_the_parent() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    commit_all(repo, "vat");
    write(repo, "docs/NOTES.md", "fixup\n");
    add_unrelated_edit(repo);
    let h = hook(
        repo,
        cache.path(),
        &payload(
            repo,
            "git add docs/NOTES.md && git commit -q --amend --no-edit",
        ),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/shelf.py"), "{context}");
}

#[test]
fn a_commit_in_a_linked_worktree_plans_on_its_own_index() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["stash", "-q"]);
    let wt_parent = TempDir::new().unwrap();
    let wt = wt_parent.path().join("wt");
    git(
        repo,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"],
    );
    write(
        &wt,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    );
    add_unrelated_edit(&wt);
    let h = hook(
        &wt,
        cache.path(),
        &payload(&wt, "git add shop/pricing.py && git commit -m vat"),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/shelf.py"), "{context}");
}

#[test]
fn the_first_commit_of_a_repository_is_planned_from_the_empty_tree() {
    let tmp = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "."]);
    write(repo, "a.py", "def f():\n    return 1\n");
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git add -A && git commit -qm init"),
    );
    assert_eq!(h.code, Some(0));
    let line = last_log_line(cache.path());
    assert!(
        !line.contains("no-range") && !line.contains("error"),
        "{line}"
    );
}

#[test]
fn an_empty_answer_stays_empty_after_the_commit_lands() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "README.md", "# shop\n\nmore\n");
    let command = "git add README.md && git commit -qm readme";
    let pre = hook(repo, cache.path(), &payload(repo, command));
    assert_eq!(pre.stdout, "");
    git(repo, &["add", "README.md"]);
    git(repo, &["commit", "-qm", "readme"]);
    inspect(repo, cache.path(), command);
    let line = last_log_line(cache.path());
    assert!(line.contains(" empty "), "{line}");
}

#[test]
fn merge_and_pick_switches_leave_their_target_alone() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "-b", "topic"]);
    commit_all(repo, "vat on topic");
    git(repo, &["checkout", "-q", "-"]);
    for command in [
        "git merge --squash topic",
        "git merge -S topic",
        "git cherry-pick -S topic",
    ] {
        let cache = TempDir::new().unwrap();
        let h = hook(repo, cache.path(), &payload(repo, command));
        assert!(
            h.stdout.contains("shop/checkout.py::charge"),
            "{command}: {}",
            last_log_line(cache.path())
        );
    }
}

#[test]
fn a_pathspec_commit_takes_known_paths_and_never_untracked_ones() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    write(
        repo,
        "shop/coupon.py",
        "from shop.pricing import total\n\n\ndef discount(cart):\n    return total(cart.items) * 0.9\n",
    );
    for command in ["git commit -m vat -- shop/", "git commit -m vat -i shop/"] {
        let cache = TempDir::new().unwrap();
        let context = context_of(&hook(repo, cache.path(), &payload(repo, command)));
        assert!(context.contains("1 changed file"), "{command}: {context}");
    }
    // Staged before the line, the new file is known to the index: `--only`
    // takes it with the rest of its paths.
    git(repo, &["add", "shop/coupon.py"]);
    let cache = TempDir::new().unwrap();
    let context = context_of(&hook(
        repo,
        cache.path(),
        &payload(repo, "git commit -m vat -- shop/"),
    ));
    assert!(context.contains("2 changed file"), "{context}");
}

#[test]
fn staging_the_line_does_by_other_means_previews_the_working_tree() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let diff = Command::new("git")
        .current_dir(repo)
        .args(["diff"])
        .output()
        .unwrap();
    std::fs::write(repo.join("vat.patch"), diff.stdout).unwrap();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "git apply --cached vat.patch && git commit -m vat"),
    );
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        last_log_line(cache.path())
    );
}

#[test]
fn a_step_in_another_repository_stages_nothing_here() {
    let tmp = repo_with_pending_change();
    let other = TempDir::new().unwrap();
    git(other.path(), &["init", "-q", "."]);
    write(other.path(), "lib.py", "def helper():\n    return 1\n");
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["add", "shop/pricing.py"]);
    let command = format!(
        "git -C {} add -A && git commit -m vat",
        other.path().display()
    );
    let h = hook(repo, cache.path(), &payload(repo, &command));
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        last_log_line(cache.path())
    );
}

/// The preview reads a copy of the index: another process holding the
/// index lock neither blocks it nor is disturbed by it, and the planned
/// replay leaves no scratch file behind, even when one of its steps fails.
#[test]
fn a_preview_never_touches_the_index_it_reads() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    git(repo, &["add", "shop/pricing.py"]);
    let git_dir = repo.join(".git");
    let listing = || {
        let mut names: Vec<String> = std::fs::read_dir(&git_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        names
    };
    let index = std::fs::read(git_dir.join("index")).unwrap();
    std::fs::write(git_dir.join("index.lock"), b"").unwrap();
    let before = listing();
    let cache = TempDir::new().unwrap();
    let h = hook(repo, cache.path(), &payload(repo, "git commit -m vat"));
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        last_log_line(cache.path())
    );
    std::fs::remove_file(git_dir.join("index.lock")).unwrap();
    let before_steps = listing();
    for command in [
        "git add -A && git commit -m vat",
        "git add nope.py && git commit -m vat",
    ] {
        let cache = TempDir::new().unwrap();
        hook(repo, cache.path(), &payload(repo, command));
    }
    assert_eq!(listing(), before_steps);
    assert!(before.contains(&"index.lock".to_string()));
    assert_eq!(std::fs::read(git_dir.join("index")).unwrap(), index);
}

#[test]
fn the_first_commit_of_a_repository_is_checked_once_it_lands() {
    let tmp = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
    write(repo, "a.py", "def f():\n    return 1\n");
    hook(
        repo,
        cache.path(),
        &payload(repo, "git add -A && git commit -qm init"),
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "init"]);
    inspect(repo, cache.path(), "git add -A && git commit -qm init");
    let line = last_log_line(cache.path());
    assert!(line.contains("committed"), "{line}");
    assert!(!line.contains(" clean "), "{line}");
}

#[test]
fn a_manual_run_that_found_nothing_leaves_no_reminder() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "README.md", "# shop\n\nVAT included.\n");
    let status = Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache.path())
        .args(["--mode", "impact", "-q"])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let h = hook(repo, cache.path(), &payload(repo, "git commit -am docs"));
    assert_eq!(h.stdout, "");
}

/// Catching up with the branch's own upstream brings in commits that
/// already landed there; merging another branch is still reviewed.
#[test]
fn a_fast_forward_to_the_upstream_is_silence() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "."]);
    git(
        repo,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(repo, &["push", "-q", "-u", "origin", "HEAD"]);
    let branch = String::from_utf8(
        Command::new("git")
            .current_dir(repo)
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    git(repo, &["checkout", "-q", "-b", "topic"]);
    commit_all(repo, "vat");
    git(repo, &["push", "-q", "origin", &format!("topic:{branch}")]);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    commit_all(repo, "more vat");
    git(repo, &["checkout", "-q", &branch]);
    git(repo, &["fetch", "-q", "origin"]);
    let cache = TempDir::new().unwrap();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, &format!("git merge --ff-only origin/{branch}")),
    );
    assert_eq!(h.stdout, "", "{}", last_log_line(cache.path()));
    let cache = TempDir::new().unwrap();
    let h = hook(repo, cache.path(), &payload(repo, "git merge topic"));
    assert!(h.stdout.contains("shop/checkout.py::charge"));
}
