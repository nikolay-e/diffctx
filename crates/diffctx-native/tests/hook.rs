// `diffctx hook pretooluse` / `posttooluse` through the binary, on real git
// repositories, with the payloads Claude Code sends. The contract under test
// is the one a hook lives or dies by: the right answer when there is one,
// silence and exit 0 in every other case.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

mod support;
use support::{commit_all, diffctx, git, write};

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
    let mut child = diffctx()
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache)
        .env_remove("CLAUDE_PLUGIN_ROOT")
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
    let doc: serde_json::Value = serde_json::from_str(h.stdout.trim())
        .unwrap_or_else(|_| panic!("no hook JSON: {:?}", h.stdout));
    doc["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_string()
}

fn in_session(session: &str, payload: &str) -> String {
    let mut doc: serde_json::Value = serde_json::from_str(payload).unwrap();
    doc["session_id"] = serde_json::Value::String(session.to_string());
    doc.to_string()
}

fn payload_in_session(session: &str, repo: &Path, command: &str) -> String {
    in_session(session, &payload(repo, command))
}

/// `command` as the Bash tool runs it: through `sh`, in `cwd`. A line that
/// fails is still a line the agent ran.
fn sh(cwd: &Path, env: &[(&str, &Path)], command: &str) {
    Command::new("sh")
        .current_dir(cwd)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .envs(env.iter().copied())
        .args(["-c", command])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run sh");
}

/// One line as the agent runs it in `session`: its `PreToolUse`, which
/// only records the refs, the line itself, then the `PostToolUse` whose
/// answer is returned.
fn line_in(session: &str, cwd: &Path, cache: &Path, env: &[(&str, &Path)], command: &str) -> Hook {
    let pre = hook_in_env(
        cwd,
        cache,
        "pretooluse",
        &[],
        env,
        &payload_in_session(session, cwd, command),
    );
    assert_eq!(
        (pre.code, pre.stdout.as_str()),
        (Some(0), ""),
        "{command}: a recording line is answered once it ran"
    );
    sh(cwd, env, command);
    let after = in_session(session, &payload_for("PostToolUse", cwd, command));
    hook_in_env(cwd, cache, "posttooluse", &[], env, &after)
}

fn run_line(repo: &Path, cache: &Path, command: &str) -> Hook {
    line_in("s1", repo, cache, &[], command)
}

#[test]
fn a_change_one_session_was_shown_is_news_to_the_next() {
    // The marker used to be keyed by content alone, so a second session
    // committing content the first had reviewed got silence (#323).
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let cache = TempDir::new().unwrap();
    let inspect_in = |session: &str| {
        let diff = in_session(session, &payload_for("PostToolUse", repo, "git diff"));
        hook_with(repo, cache.path(), "posttooluse", &[], &diff)
    };
    assert!(answered(&inspect_in("one")));
    assert_eq!(
        inspect_in("one").stdout,
        "",
        "the same session is not told twice"
    );
    let other = line_in("two", repo, cache.path(), &[], "git commit -am vat");
    let context = context_of(&other);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shown earlier"), "{context}");
}

#[test]
fn a_manual_impact_run_just_before_still_counts() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let cache = TempDir::new().unwrap();
    let manual = diffctx()
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache.path())
        .args(["--diff", "HEAD", "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(manual.status.success());
    let h = line_in("fresh", repo, cache.path(), &[], "git commit -am vat");
    assert_reminder(&h);
}

/// A line that recorded a change whose impact the agent already read: one
/// line that names what was shown, never the listing again and never
/// silence (#346, #501).
fn assert_reminder(h: &Hook) {
    assert_eq!(h.code, Some(0));
    let context = output_of(h, "PostToolUse")["additionalContext"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        context.starts_with("diffctx: this command recorded a change whose impact was"),
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
    let first = run_line(repo, cache.path(), "git commit -am 'vat'");
    assert_eq!(first.code, Some(0));
    let context = context_of(&first);
    assert!(
        context.starts_with("diffctx reviewed what this commit recorded."),
        "{context}"
    );
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        context.contains("reachable from tests: tests/test_checkout.py"),
        "{context}"
    );
    assert!(context.contains("Check these callers"), "{context}");
    assert!(context.chars().count() <= 9_200, "{}", context.len());
    let out = output_of(&first, "PostToolUse");
    assert!(
        out.get("permissionDecision").is_none(),
        "no gate by default"
    );

    let again = run_line(repo, cache.path(), "git commit -am 'vat'");
    assert_eq!(again.code, Some(0));
    assert_eq!(again.stdout, "", "a line that records nothing says nothing");
}

/// #524: a session keeps the plugin it started with. Its hook says once per
/// session that a newer version is installed beside it, without dropping the
/// review, and every log line names the version that wrote it.
#[test]
fn an_older_pin_says_once_per_session_that_a_newer_one_is_installed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let plugins = TempDir::new().unwrap();
    let repo = tmp.path();
    let running = env!("CARGO_PKG_VERSION");
    let root = plugins.path().join(running);
    for dir in [running, "999.0.0", "1000.0", "not-a-version"] {
        std::fs::create_dir_all(plugins.path().join(dir)).unwrap();
    }
    let env = [("CLAUDE_PLUGIN_ROOT", root.as_path())];
    let notice = format!("diffctx 999.0.0 is installed; this session runs {running}.");
    let pre = |session: &str| {
        hook_in_env(
            repo,
            cache.path(),
            "pretooluse",
            &[],
            &env,
            &payload_in_session(session, repo, "git commit -am vat"),
        )
    };

    let first = pre("s1");
    assert!(context_of(&first).starts_with(&notice), "{}", first.stdout);

    sh(repo, &[], "git commit -am vat");
    let post = hook_in_env(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &env,
        &in_session(
            "s1",
            &payload_for("PostToolUse", repo, "git commit -am vat"),
        ),
    );
    let review = context_of(&post);
    assert!(!review.contains(&notice), "once per session: {review}");
    assert!(review.contains("shop/checkout.py::charge"), "{review}");

    assert!(
        context_of(&pre("s2")).starts_with(&notice),
        "a new session hears it"
    );

    let same = TempDir::new().unwrap();
    let current = same.path().join(running);
    std::fs::create_dir_all(&current).unwrap();
    let quiet = hook_in_env(
        repo,
        cache.path(),
        "pretooluse",
        &[],
        &[("CLAUDE_PLUGIN_ROOT", current.as_path())],
        &payload_in_session("s3", repo, "git status"),
    );
    assert_eq!(quiet.stdout, "", "nothing newer, nothing said");

    // A build no release pipeline made says so: it shares the release's
    // version, and only the mark tells its runs from the release's (#570).
    let label = _diffctx::run_provenance::BUILD_LABEL;
    assert!(label == running || label == format!("{running}+source"));
    let log = hook_log(cache.path());
    assert!(
        log.lines().all(|l| l.split(' ').last() == Some(label)),
        "{log}"
    );
}

/// #527: a search later on the line (here over the commit's own log, a
/// file outside git) must not take the answer from what the line recorded.
#[test]
fn a_commit_line_that_then_greps_its_log_is_still_reviewed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let log = cache.path().join("commit.log");
    let command = format!(
        "git commit -am 'vat' > '{log}' 2>&1; echo \"exit=$?\"; grep -E 'Failed|modified' '{log}' | head; git log --oneline -1",
        log = log.display()
    );
    let h = run_line(repo, cache.path(), &command);
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(
        context.starts_with("diffctx reviewed what this commit recorded."),
        "{context}"
    );
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

/// #405: a subshell's parentheses are words of their own, and its `cd`
/// ends with it.
#[test]
fn a_subshell_add_before_a_commit_is_reviewed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let h = run_line(
        repo,
        cache.path(),
        "(cd shop && git add pricing.py) && git commit -m vat",
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

/// #405: a comment's apostrophe opens no quote; a `cd` inside a subshell
/// names the repository its commit runs in; `cd -` goes where the line does
/// not say; an odd `"` inside `-m "$(cat <<'EOF' … EOF)"` closes nothing.
#[test]
fn comments_subshell_cds_cd_dash_and_heredoc_messages_keep_their_commit() {
    let answered = |command: &str| {
        let tmp = repo_with_pending_change();
        let cache = TempDir::new().unwrap();
        let h = run_line(tmp.path(), cache.path(), command);
        assert_eq!(h.code, Some(0));
        context_of(&h)
    };
    let commented = answered("# Don't forget the tests\ngit add -A && git commit -m vat");
    assert!(
        commented.contains("shop/checkout.py::charge"),
        "{commented}"
    );
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    // Recorded, not read as git hidden in a substitution. Not run: bash 3.2,
    // macOS's `sh`, cannot parse this line itself.
    let cache = TempDir::new().unwrap();
    let message = "git commit -am \"$(cat <<'EOF'\nSupport 5\" screens (and more)\nEOF\n)\"";
    let h = hook(repo, cache.path(), &payload(repo, message));
    assert_eq!(h.stdout, "");
    let line = last_log_line(cache.path());
    assert!(line.contains(" recorded "), "{line}");

    let cache = TempDir::new().unwrap();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "cd - && git commit -am vat"),
    );
    assert!(
        h.stdout.contains("did not analyse this command"),
        "cd - names no directory, and says so: {}",
        h.stdout
    );

    let other = repo_with_pending_change();
    write(
        other.path(),
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 5\n",
    );
    let cache = TempDir::new().unwrap();
    // Quoted as a shell carries it: a Windows path's backslashes are not escapes.
    let command = format!("(cd '{}' && git commit -am x)", other.path().display());
    let context = context_of(&run_line(repo, cache.path(), &command));
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

#[test]
fn a_plain_commit_reviews_the_index_not_the_working_tree() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    // pricing is staged; inventory is edited but NOT staged.
    git(repo, &["add", "shop/pricing.py"]);
    add_unrelated_edit(repo);
    let context = context_of(&run_line(repo, cache.path(), "git commit -m 'vat'"));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        !context.contains("shop/shelf.py"),
        "the unstaged inventory edit is not part of this commit: {context}"
    );

    // `-a` takes the working tree, and the inventory caller with it.
    let context = context_of(&run_line(repo, cache.path(), "git commit -am 'restock'"));
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

#[test]
fn a_manual_impact_run_counts_as_the_review() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let status = diffctx()
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache.path())
        .args(["--mode", "impact", "-q"])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let h = run_line(repo, cache.path(), "git commit -am 'vat'");
    assert_reminder(&h);
}

/// #454: a commit of code this build reads as text is not silence — the
/// hook saying nothing read as "nothing outside the diff is reached".
#[test]
fn a_commit_of_unparsed_code_says_it_was_not_analysed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "num/solver.f90", "module solver\nend module solver\n");
    commit_all(repo, "solver");
    write(
        repo,
        "num/solver.f90",
        "module solver\ncontains\n  subroutine step(x)\n    real :: x\n  end subroutine step\nend module solver\n",
    );
    let h = run_line(repo, cache.path(), "git commit -am solver");
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("1 not analysed"), "{context}");
    assert!(
        context.contains("fortran (no grammar in this build): num/solver.f90"),
        "{context}"
    );
}

/// #464: an empty answer is one line, so that no block means the hook did
/// not run, never "reviewed, nothing".
#[test]
fn a_change_nothing_depends_on_says_so_in_one_line() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "README.md", "# shop\n\nVAT included.\n");
    let h = run_line(
        repo,
        cache.path(),
        "git add README.md && git commit -qm docs",
    );
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(
        context.contains("reaches nothing outside the diff"),
        "{context}"
    );
    assert_eq!(context.lines().count(), 1, "{context}");
    assert!(!h.stdout.contains("permissionDecision"), "{}", h.stdout);
    assert!(last_log_line(cache.path()).contains(" empty "));
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
    let h = run_line(repo, cache.path(), "git push");
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

/// A branch the remote does not have yet sends what it holds past the
/// remote's default branch.
#[test]
fn a_push_of_a_new_branch_is_measured_from_the_remote_default() {
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
    git(repo, &["checkout", "-q", "-b", "topic"]);
    commit_all(repo, "vat");
    let h = run_line(repo, cache.path(), "git push origin HEAD");
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
    let h = run_line(repo, cache.path(), "git push origin main");
    assert_eq!(h.code, Some(0));
    assert_eq!(h.stdout, "");
}

/// On `main`: `origin` has the first commit as `release` and `old`, and
/// `restock` (called by `shelf.refill`) as `main`. Local `feature` adds
/// `total` (called by `checkout.charge`), local `main` adds `rate` (called
/// by `invoice.bill`); neither is pushed.
fn repo_with_branches_beside_origin() -> (TempDir, TempDir) {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let vat = std::fs::read_to_string(repo.join("shop/pricing.py")).unwrap();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "shop/tax.py", "def rate(country):\n    return 0.19\n");
    write(
        repo,
        "shop/invoice.py",
        "from shop.tax import rate\n\n\ndef bill(order):\n    return order.amount * (1 + rate(order.country))\n",
    );
    commit_all(repo, "tax");
    git(repo, &["branch", "-M", "main"]);
    let remote = TempDir::new().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "."]);
    git(
        repo,
        &["remote", "add", "origin", &remote.path().to_string_lossy()],
    );
    git(
        repo,
        &[
            "push",
            "-q",
            "origin",
            "HEAD:refs/heads/release",
            "HEAD:refs/heads/old",
        ],
    );
    add_unrelated_edit(repo);
    commit_all(repo, "restock");
    git(repo, &["push", "-q", "-u", "origin", "main"]);
    git(repo, &["remote", "set-head", "origin", "main"]);
    git(repo, &["checkout", "-q", "-b", "feature"]);
    write(repo, "shop/pricing.py", &vat);
    commit_all(repo, "total");
    git(repo, &["checkout", "-q", "main"]);
    write(
        repo,
        "shop/tax.py",
        "def rate(country):\n    return 0.07 if country == 'reduced' else 0.19\n",
    );
    commit_all(repo, "rate");
    (tmp, remote)
}

/// #416: a push reviews the ref it moved. From `main`, `git push origin
/// feature` reviewed `main`'s own unpushed commit, or logged "clean" once
/// `main` had none, and never the feature commit it sends.
#[test]
fn a_push_reviews_the_ref_it_names_not_the_current_branch() {
    // A branch `origin` does not have yet, and one it has, forced.
    for command in [
        "git push origin feature",
        "git push -u origin +feature:main",
    ] {
        let (tmp, _remote) = repo_with_branches_beside_origin();
        let cache = TempDir::new().unwrap();
        let context = context_of(&run_line(tmp.path(), cache.path(), command));
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{command}: {context}"
        );
        assert!(
            !context.contains("shop/invoice.py"),
            "{command}: main's own commit is not what it sends: {context}"
        );
    }
    // Pushed again, a branch sends only what it gained since.
    let (tmp, _remote) = repo_with_branches_beside_origin();
    let repo = tmp.path();
    git(repo, &["push", "-q", "origin", "feature"]);
    git(repo, &["checkout", "-q", "feature"]);
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    return item.count + 3\n",
    );
    commit_all(repo, "restock on feature");
    git(repo, &["checkout", "-q", "main"]);
    let cache = TempDir::new().unwrap();
    let context = context_of(&run_line(repo, cache.path(), "git push origin feature"));
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
    assert!(!context.contains("shop/checkout.py::charge"), "{context}");
}

/// One push of several refs reviews the first it moved and counts the
/// rest, never the current branch in their place (#416).
#[test]
fn a_push_of_several_refs_reviews_one_and_counts_the_rest() {
    for command in ["git push origin feature main", "git push --all origin"] {
        let (tmp, _remote) = repo_with_branches_beside_origin();
        let cache = TempDir::new().unwrap();
        let context = context_of(&run_line(tmp.path(), cache.path(), command));
        assert!(
            context.contains("1 other pushed ref(s) are not analysed"),
            "{command}: {context}"
        );
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{command}: {context}"
        );
    }
}

#[test]
fn a_push_that_only_deletes_is_silence() {
    for command in [
        "git push origin :old",
        "git push origin +:old",
        "git push origin --delete old",
        "git push -fd origin old",
    ] {
        let (tmp, _remote) = repo_with_branches_beside_origin();
        let cache = TempDir::new().unwrap();
        let h = run_line(tmp.path(), cache.path(), command);
        assert_eq!(h.code, Some(0), "{command}");
        assert_eq!(h.stdout, "", "{command}: {}", last_log_line(cache.path()));
    }
}

/// #416: `gh pr create --base release` opens the PR against `release`,
/// which lacks `restock` too; `--head` names the branch it is opened from.
#[test]
fn a_pull_request_reviews_the_branches_it_names() {
    let (tmp, _remote) = repo_with_branches_beside_origin();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "feature"]);
    let cache = TempDir::new().unwrap();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "gh pr create --base release --fill"),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
    git(repo, &["checkout", "-q", "main"]);
    let cache = TempDir::new().unwrap();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "gh pr create -B main --head feature --fill"),
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/invoice.py"), "{context}");
}

/// #416: a ref only the shell can name, or one the repository does not
/// have, moves nothing and is answered with nothing, never with the current
/// branch; a PR base nothing resolves is logged unresolved.
#[test]
fn a_ref_the_line_does_not_resolve_is_logged_not_replaced() {
    let (tmp, _remote) = repo_with_branches_beside_origin();
    let repo = tmp.path();
    for command in [
        "git push origin \"$BRANCH\"",
        "git push origin nope",
        "git push origin feature:",
    ] {
        let cache = TempDir::new().unwrap();
        let h = run_line(repo, cache.path(), command);
        assert_eq!(h.code, Some(0), "{command}");
        assert_eq!(h.stdout, "", "{command}");
    }
    let cache = TempDir::new().unwrap();
    let h = hook(
        repo,
        cache.path(),
        &payload(repo, "gh pr create --base nope --fill"),
    );
    assert_eq!(h.stdout, "");
    let line = last_log_line(cache.path());
    assert!(line.contains(" unresolved-ref "), "{line}");
}

/// One command line's `PreToolUse` and `PostToolUse` in `session`, with what
/// the line does in between.
fn hook_pair(session: &str, repo: &Path, cache: &Path, command: &str, runs: impl FnOnce()) -> Hook {
    hook(repo, cache, &payload_in_session(session, repo, command));
    runs();
    let after = in_session(session, &payload_for("PostToolUse", repo, command));
    hook_with(repo, cache, "posttooluse", &[], &after)
}

/// #406: after a `git commit` that landed nothing, a commit another session
/// made seconds earlier is no "commit that just landed"; nor is one that
/// replaced `HEAD` while the line ran.
#[test]
fn a_commit_that_landed_nothing_is_not_answered_with_another_sessions_commit() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    // Session `a` commits.
    commit_all(repo, "vat");
    // Nothing staged: git prints "nothing to commit" and HEAD stays.
    let h = hook_pair(
        "b",
        repo,
        cache.path(),
        "git commit -m y; git status",
        || {},
    );
    assert_eq!(h.stdout, "", "{}", last_log_line(cache.path()));
    let h = hook_pair("b", repo, cache.path(), "git commit -qam z", || {
        git(repo, &["reset", "-q", "--hard", "HEAD~1"]);
        add_unrelated_edit(repo);
        commit_all(repo, "restock elsewhere");
    });
    assert_eq!(h.stdout, "", "{}", last_log_line(cache.path()));
}

/// #575: a recording verb word inside another command's argument (`--jq`,
/// `-m`, a `grep` pattern) does not make a read-only git line record refs and
/// claim a commit another session lands while it runs.
#[test]
fn a_read_only_git_line_with_commit_in_another_argument_claims_no_commit() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let command = "git rev-parse HEAD; gh api repos/o/r/commits/abc --jq '.commit.author.date'";
    let h = hook_pair("b", repo, cache.path(), command, || {
        commit_all(repo, "vat");
    });
    assert_eq!(h.stdout, "", "{}", last_log_line(cache.path()));
    assert!(
        last_log_line(cache.path()).contains(" no-trigger "),
        "{}",
        last_log_line(cache.path())
    );
}

/// An amend replaces `HEAD` on its parent: what it records is measured from
/// that parent, the earlier commit's change included, whatever staged it.
#[test]
fn an_amend_is_reviewed_onto_the_parent() {
    for (command, takes_inventory) in [
        (
            "git add docs/NOTES.md && git commit -q --amend --no-edit",
            false,
        ),
        ("git commit -qa --amend --no-edit", true),
    ] {
        let tmp = repo_with_pending_change();
        let cache = TempDir::new().unwrap();
        let repo = tmp.path();
        commit_all(repo, "vat");
        write(repo, "docs/NOTES.md", "fixup\n");
        add_unrelated_edit(repo);
        let context = context_of(&run_line(repo, cache.path(), command));
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{command}: {context}"
        );
        assert_eq!(
            context.contains("shop/shelf.py::refill"),
            takes_inventory,
            "{command}: {context}"
        );
    }
}

/// A Claude Code transcript at `dir/b.jsonl` whose session edited `edited`;
/// `by_subagent` was edited by a subagent of it, whose transcript sits in
/// `dir/b/subagents/`.
fn transcript(
    dir: &Path,
    edited: &[std::path::PathBuf],
    by_subagent: &[std::path::PathBuf],
) -> std::path::PathBuf {
    let calls = |tool: &str, paths: &[std::path::PathBuf]| -> String {
        let mut lines = vec![
            serde_json::json!({"type": "user", "sessionId": "b", "message": {"role": "user", "content": "what changed?"}})
                .to_string(),
            serde_json::json!({"type": "assistant", "sessionId": "b", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t0", "name": "Bash", "input": {"command": "git status"}}
            ]}})
            .to_string(),
        ];
        lines.extend(paths.iter().map(|p| {
            serde_json::json!({"type": "assistant", "sessionId": "b", "message": {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t1", "name": tool, "input": {"file_path": p, "content": "x"}}
            ]}})
            .to_string()
        }));
        lines.join("\n") + "\n"
    };
    let main = dir.join("b.jsonl");
    std::fs::write(&main, calls("Edit", edited)).unwrap();
    let subagents = dir.join("b").join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    std::fs::write(subagents.join("agent-1.jsonl"), calls("Write", by_subagent)).unwrap();
    main
}

fn inspect_with_transcript(repo: &Path, command: &str, transcript: Option<&Path>) -> Hook {
    let mut doc: serde_json::Value =
        serde_json::from_str(&payload_for("PostToolUse", repo, command)).unwrap();
    if let Some(t) = transcript {
        doc["transcript_path"] = serde_json::Value::String(t.to_string_lossy().to_string());
    }
    let cache = TempDir::new().unwrap();
    hook_with(repo, cache.path(), "posttooluse", &[], &doc.to_string())
}

/// #404: in a checkout another session shares, an inspection reviews the
/// files this session's own transcript edited and only counts the rest;
/// without a transcript to tell, it says so.
#[test]
fn an_inspection_reviews_only_what_this_session_edited() {
    // `pricing.total` is another session's uncommitted edit.
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let dir = TempDir::new().unwrap();
    let nothing = transcript(dir.path(), &[], &[]);
    let h = inspect_with_transcript(repo, "git status", Some(&nothing));
    assert_eq!(h.stdout, "", "this session edited nothing");

    add_unrelated_edit(repo);
    let inventory = repo.join("shop/inventory.py");
    for (edited, by_subagent) in [(vec![inventory.clone()], vec![]), (vec![], vec![inventory])] {
        let dir = TempDir::new().unwrap();
        let own = transcript(dir.path(), &edited, &by_subagent);
        let h = inspect_with_transcript(repo, "git diff", Some(&own));
        assert!(h.stdout.contains("shop/shelf.py::refill"), "{}", h.stdout);
        assert!(
            !h.stdout.contains("shop/checkout.py::charge"),
            "{}",
            h.stdout
        );
        assert!(
            h.stdout.contains(
                "1 other changed file(s) show no write by this session or its subagents and are read as committed: shop/pricing.py."
            ),
            "{}",
            h.stdout
        );
    }

    let missing = dir.path().join("gone.jsonl");
    for transcript in [None, Some(missing.as_path())] {
        let h = inspect_with_transcript(repo, "git diff", transcript);
        assert!(
            h.stdout.contains("shop/checkout.py::charge"),
            "{}",
            h.stdout
        );
        assert!(
            h.stdout.contains("cannot tell this session's edits"),
            "{}",
            h.stdout
        );
    }
}

/// A merge reviews what it brings in, not our own commits; `--squash`
/// records nothing, and the commit that follows it is what is reviewed.
#[test]
fn a_merge_reviews_the_branch_not_our_own_commits() {
    let diverged = || {
        let tmp = repo_with_pending_change();
        let repo = tmp.path();
        git(repo, &["checkout", "-q", "-b", "topic"]);
        commit_all(repo, "vat on topic");
        git(repo, &["checkout", "-q", "-"]);
        // Diverge: main edits inventory, which topic never touched.
        add_unrelated_edit(repo);
        commit_all(repo, "restock more");
        tmp
    };
    let tmp = diverged();
    let cache = TempDir::new().unwrap();
    let context = context_of(&run_line(tmp.path(), cache.path(), "git merge topic"));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(
        !context.contains("shop/shelf.py"),
        "our own inventory commit is not what the merge brings in: {context}"
    );

    let tmp = diverged();
    let repo = tmp.path();
    let cache = TempDir::new().unwrap();
    let squash = run_line(repo, cache.path(), "git merge --squash topic");
    assert_eq!(squash.stdout, "", "{}", last_log_line(cache.path()));
    let context = context_of(&run_line(repo, cache.path(), "git commit -m squashed"));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

/// A pick reviews the commits it picked, all of them, a Latin-1 file among
/// them included.
#[test]
fn a_cherry_pick_reviews_the_picked_commits() {
    let topic = || {
        let tmp = repo_with_pending_change();
        let repo = tmp.path();
        git(repo, &["stash", "-q"]);
        std::fs::write(repo.join("shop/names.py"), b"NAME = '\xe9t\xe9'\n").unwrap();
        commit_all(repo, "names");
        git(repo, &["checkout", "-q", "-b", "topic"]);
        std::fs::write(repo.join("shop/names.py"), b"NAME = '\xe9t\xe9 2'\n").unwrap();
        commit_all(repo, "names on topic");
        git(repo, &["stash", "pop", "-q"]);
        commit_all(repo, "vat on topic");
        add_unrelated_edit(repo);
        commit_all(repo, "restock on topic");
        git(repo, &["checkout", "-q", "-"]);
        tmp
    };
    for (command, takes_restock) in [
        ("git cherry-pick topic~1", false),
        ("git cherry-pick -x topic~2 topic~1 topic", true),
    ] {
        let tmp = topic();
        let cache = TempDir::new().unwrap();
        let context = context_of(&run_line(tmp.path(), cache.path(), command));
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{command}: {context}"
        );
        assert_eq!(
            context.contains("shop/shelf.py::refill"),
            takes_restock,
            "{command}: {context}"
        );
    }
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
    let again = run_line(repo, cache.path(), "git commit -am vat");
    assert_reminder(&again);

    // A historical diff is not the pending change — with a real HEAD~1 and
    // a fresh pending edit, so the silence comes from the verb, not from an
    // unresolvable revision.
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
    let commit = run_line(repo, cache.path(), "git commit -m vat");
    assert_reminder(&commit);
}

#[test]
fn the_strict_gate_denies_once_with_the_impact_attached() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let gated = |cache: &Path, command: &str| {
        hook_with(
            repo,
            cache,
            "pretooluse",
            &["--gate"],
            &payload(repo, command),
        )
    };
    // A line that stages for itself is asked to stage first: the gate never
    // predicts what a command will stage (#508).
    for command in [
        "git commit -am vat",
        "git add -A && git commit -m vat",
        "git commit -m vat -- shop/pricing.py",
        "git commit --pathspec-from-file=paths.txt -m vat",
    ] {
        let out = output_of(&gated(cache.path(), command), "PreToolUse");
        assert_eq!(out["permissionDecision"], "deny", "{command}");
        let reason = out["permissionDecisionReason"].as_str().unwrap();
        assert!(reason.contains("as its own command"), "{command}: {reason}");
        assert!(!reason.contains("again to proceed"), "{command}: {reason}");
    }
    git(repo, &["add", "-A"]);
    let out = output_of(&gated(cache.path(), "git commit -m vat"), "PreToolUse");
    assert_eq!(out["permissionDecision"], "deny");
    for field in ["additionalContext", "permissionDecisionReason"] {
        assert!(
            out[field]
                .as_str()
                .unwrap()
                .contains("shop/checkout.py::charge"),
            "{field}: {out}"
        );
    }
    let retry = gated(cache.path(), "git commit -m vat");
    assert_eq!(retry.stdout, "", "reviewed once: the retry passes");
    sh(repo, &[], "git commit -m vat");
    let landed = hook_with(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &payload_for("PostToolUse", repo, "git commit -m vat"),
    );
    assert_reminder(&landed);

    // Nothing to review, nothing to gate.
    let cache2 = TempDir::new().unwrap();
    write(repo, "README.md", "# shop\n\nVAT.\n");
    git(repo, &["add", "README.md"]);
    let docs = gated(cache2.path(), "git commit -m docs");
    assert!(
        !docs.stdout.contains("permissionDecision"),
        "{}",
        docs.stdout
    );
    assert!(docs.stdout.contains("reaches nothing"), "{}", docs.stdout);
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
    let command = format!("git -C '{}' commit -am vat", repo.to_string_lossy());
    let h = line_in("s1", elsewhere.path(), cache.path(), &[], &command);
    assert_eq!(h.code, Some(0));
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
}

/// #422: landing from a worktree is usually `L=/abs; cd $L && git commit …`;
/// the variable the line itself sets names the repository.
#[test]
fn a_directory_the_line_sets_in_a_variable_is_followed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let elsewhere = TempDir::new().unwrap();
    let command = format!(
        "L='{}'; cd \"$L\" && git commit -qam vat",
        tmp.path().to_string_lossy()
    );
    let h = line_in("s1", elsewhere.path(), cache.path(), &[], &command);
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
    // A name the change does not edit is answered as `--symbol` would (#628).
    let unedited = context_of(&search("Bash", "grep -rn restock ."));
    assert!(unedited.contains("shop/shelf.py::refill"), "{unedited}");

    // The search answer does not stand in for the commit's review.
    let commit = run_line(repo, cache.path(), "git commit -am vat");
    assert!(context_of(&commit).contains("shop/checkout.py::charge"));

    let log = hook_log(cache.path());
    let outcomes: Vec<&str> = log.lines().map(|l| l.split(' ').nth(5).unwrap()).collect();
    assert_eq!(
        outcomes,
        ["shown", "seen", "shown", "recorded", "shown"],
        "{log}"
    );
    let last = log.lines().last().unwrap();
    assert!(
        last.contains(" commit ") && last.contains(" shown "),
        "{log}"
    );
    assert!(
        !last.contains(" HEAD "),
        "`-a` is read off what the commit recorded, not the working tree: {log}"
    );
}

/// #628: most lookups come on a clean tree, where the hook said nothing.
/// A search for a defined name gets where it is defined, its callers and
/// their tests, once per name in a session; a name nothing here defines or
/// calls, or a filter over another command's output, gets silence.
#[test]
fn a_search_on_a_clean_tree_is_answered_as_symbol_would() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    commit_all(repo, "vat");
    let search = |query: &str| {
        hook_with(
            repo,
            cache.path(),
            "posttooluse",
            &[],
            &search_payload(repo, "Bash", query),
        )
    };
    let context = context_of(&search("grep -rn 'total(' shop/"));
    assert!(context.contains("--symbol total"), "{context}");
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(context.contains("tests/test_checkout.py"), "{context}");
    assert_eq!(search("grep -rn total .").stdout, "", "once per name");
    assert_eq!(search("grep -rn no_such_name_here .").stdout, "");
    // `refill` is defined and nothing here calls it: no zero is asserted,
    // since a caller in a file no model reads would read as none.
    assert_eq!(search("grep -rn refill .").stdout, "");
    assert_eq!(search("git log --oneline | grep restock").stdout, "");
}

/// A search the hook cannot place, and a name defined all over, get no
/// answer: a grep of `$OUT/notes.tsv` read as a search of the cwd's
/// repository, and `main` with 44 definitions printed the first two.
#[test]
fn a_search_it_cannot_place_or_an_ambiguous_name_is_silence() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    for n in 0..4 {
        write(
            repo,
            &format!("jobs/job{n}.py"),
            "def handle(event):\n    return event\n",
        );
    }
    write(
        repo,
        "jobs/run.py",
        "from jobs.job0 import handle\n\n\ndef run(e):\n    return handle(e)\n",
    );
    commit_all(repo, "vat");
    let outside = TempDir::new().unwrap();
    let search = |query: &str| {
        hook_with(
            repo,
            cache.path(),
            "posttooluse",
            &[],
            &search_payload(repo, "Bash", query),
        )
    };
    let scratch = format!(
        "OUT='{}'; grep -n total $OUT/notes.tsv",
        outside.path().display()
    );
    assert_eq!(
        search(&scratch).stdout,
        "",
        "{}",
        last_log_line(cache.path())
    );
    assert_eq!(search("grep -rn handle jobs/").stdout, "");
    assert!(
        last_log_line(cache.path()).contains(" ambiguous "),
        "{}",
        last_log_line(cache.path())
    );
    // A name defined once is still answered.
    assert!(context_of(&search("grep -rn 'total(' shop/")).contains("shop/checkout.py::charge"));
}

/// A search is answered for what it searches and where it found it: every
/// name it is a search for (#634), never a word inside a line of code or a
/// file name (#643, #646), only the definitions in the files its result
/// names or called from them (fj#95), and never for an inverted search, a
/// search of another revision or one that found nothing. The payload
/// carries the command's real output, as Claude Code sends it.
#[test]
fn a_search_is_answered_for_each_name_it_seeks_where_it_looked() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    write(
        repo,
        "legacy/stock.py",
        "def restock(n):\n    return n\n\n\ndef rerun(n):\n    return restock(n)\n",
    );
    write(repo, "AGENTS.md", "# shop\n");
    write(
        repo,
        "scripts/report.mjs",
        "export function md(s) {\n  return s;\n}\nconsole.log(md(\"x\"));\n",
    );
    write(
        repo,
        "shop/cart.py",
        "def models():\n    return []\n\n\ndef lines():\n    return models()\n",
    );
    write(repo, "docs/stock.md", "Call restock() nightly.\n");
    write(repo, "pyproject.toml", "[restock]\nnightly = true\n");
    commit_all(repo, "vat");
    let mut n = 0;
    let mut search = |query: &str| {
        n += 1;
        let ran = Command::new("sh")
            .args(["-c", query])
            .current_dir(repo)
            .output()
            .unwrap();
        let mut payload: serde_json::Value = serde_json::from_str(&in_session(
            &format!("s{n}"),
            &search_payload(repo, "Bash", query),
        ))
        .unwrap();
        payload["tool_response"] =
            serde_json::json!({"stdout": String::from_utf8_lossy(&ran.stdout), "stderr": ""});
        hook_with(repo, cache.path(), "posttooluse", &[], &payload.to_string())
    };
    let both = context_of(&search(r#"grep -rn "total\|restock" shop tests"#));
    assert!(
        both.contains("`total`") && both.contains("shop/checkout.py::charge"),
        "{both}"
    );
    assert!(
        both.contains("`restock`") && both.contains("shop/shelf.py::refill"),
        "{both}"
    );
    assert!(!both.contains("legacy/stock.py"), "{both}");
    let flags = context_of(&search("grep -rn -e total -e restock shop"));
    assert!(
        flags.contains("`total`") && flags.contains("`restock`"),
        "{flags}"
    );
    let scoped = context_of(&search("grep -n restock legacy/stock.py"));
    assert!(scoped.contains("legacy/stock.py::rerun"), "{scoped}");
    assert!(!scoped.contains("shop/shelf.py"), "{scoped}");
    // A glob path searches the directory it starts in.
    let globbed = context_of(&search("grep -n restock shop/*.py"));
    assert!(globbed.contains("shop/shelf.py::refill"), "{globbed}");
    let docs = context_of(&search("grep -rn restock ."));
    assert!(docs.contains("shop/shelf.py::refill"), "{docs}");
    for query in [
        "git grep -n 'AGENTS.md'",
        "grep -rn '^from models import' shop",
        "grep -rn 'lines.models' shop",
        "grep -rn --include='*.md' restock .",
        "grep -n '^\\[restock' pyproject.toml",
        "grep -rLn restock shop",
        "git grep -n refill HEAD~1",
        "grep -rn no_such_name_here .",
    ] {
        assert_eq!(search(query).stdout, "", "{query}");
    }
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
    let commit = run_line(repo, cache.path(), "git commit -m vat");
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
        let mut child = diffctx()
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

/// #612, #613: a commit whose review failed is said to be unreviewed, with
/// the range to run by hand; silence there read as a clean review.
#[test]
fn a_landed_commit_whose_review_failed_says_it_was_not_reviewed() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    // A strict run refuses an undeclared override: the pipeline fails.
    let env = [
        ("DIFFCTX_EVAL_STRICT", Path::new("1")),
        ("DIFFCTX_NOT_A_SETTING", Path::new("1")),
    ];
    let h = line_in("s1", repo, cache.path(), &env, "git commit -qam vat");
    let context = context_of(&h);
    assert!(context.contains("diffctx did not review"), "{context}");
    assert!(context.contains("--mode impact"), "{context}");
    let line = last_log_line(cache.path());
    assert!(line.contains(" error:pipeline "), "{line}");
}

/// #508: what a line recorded is read off git's reflog, whatever spelled
/// it: `git am` (#607), an alias, a git only the shell runs (#407).
#[test]
fn a_commit_is_reviewed_whatever_verb_or_shell_records_it() {
    for command in [
        "git am",
        "git ci -qam vat",
        "sh -c 'git commit -qam vat'",
        "bash -c \"git commit -qam vat\"",
        "echo $(git -C . commit -qam vat)",
    ] {
        let tmp = repo_with_pending_change();
        let repo = tmp.path();
        git(repo, &["config", "alias.ci", "commit"]);
        let patches = TempDir::new().unwrap();
        let command = if command == "git am" {
            commit_all(repo, "vat");
            let out = patches.path().to_string_lossy();
            git(repo, &["format-patch", "-q", "-1", "-o", &out, "HEAD"]);
            git(repo, &["reset", "-q", "--hard", "HEAD~1"]);
            format!("git am -q '{out}/0001-vat.patch'")
        } else {
            command.to_string()
        };
        let cache = TempDir::new().unwrap();
        let context = context_of(&run_line(repo, cache.path(), &command));
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{command}: {context}"
        );
    }
}

/// A git only the shell runs, that recorded nothing where the hook could
/// look, is said to be unread once it ran: never a guess before it runs.
#[test]
fn a_hidden_git_that_moved_nothing_here_says_it_was_not_analysed() {
    let tmp = repo_with_pending_change();
    let elsewhere = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let command = format!(
        "sh -c \"cd '{}' && git commit -qam vat\"",
        elsewhere.path().display()
    );
    let h = run_line(tmp.path(), cache.path(), &command);
    assert!(
        h.stdout.contains("did not analyse this command"),
        "{}",
        h.stdout
    );
    let line = last_log_line(cache.path());
    assert!(line.contains(" unsupported-syntax "), "{line}");
}

/// What landed is every entry git wrote while the line ran, not its last
/// one: a reset, switch, stash or rebase after the commit on the same line
/// hid it (#508, a commit then `fetch && rebase` in the field).
#[test]
fn a_commit_a_later_move_on_the_line_hides_is_still_reviewed() {
    for then in [
        "git reset -q --hard HEAD~1",
        "git switch -q -c side",
        "echo x >> README.md && git stash -q",
        "git rebase -q --force-rebase HEAD~1",
    ] {
        let tmp = repo_with_pending_change();
        let cache = TempDir::new().unwrap();
        let command = format!("git commit -qam vat && {then}");
        let context = context_of(&run_line(tmp.path(), cache.path(), &command));
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{command}: {context}"
        );
    }
}

/// #642: a push from a work tree the same line then removes is reviewed:
/// the repository's git directory still holds the refs the push moved.
#[test]
fn a_push_from_a_work_tree_the_line_removes_is_reviewed() {
    let (tmp, _remote) = repo_with_branches_beside_origin();
    let repo = tmp.path();
    let cache = TempDir::new().unwrap();
    let parent = TempDir::new().unwrap();
    let wt = parent.path().join("wt");
    let wt_arg = wt.to_string_lossy().to_string();
    git(
        repo,
        &["worktree", "add", "-q", "--detach", &wt_arg, "origin/main"],
    );
    write(&wt, "shop/tax.py", "def rate(country):\n    return 0.2\n");
    commit_all(&wt, "rate");
    let command =
        format!("git -C '{wt_arg}' push -q origin HEAD:main && git worktree remove '{wt_arg}'");
    let context = context_of(&run_line(repo, cache.path(), &command));
    assert!(context.contains("shop/invoice.py::bill"), "{context}");
}

/// #608: a line that records in two repositories gets both reviewed.
#[test]
fn every_repository_a_line_records_in_is_reviewed() {
    let a = repo_with_pending_change();
    let b = repo_with_pending_change();
    git(b.path(), &["checkout", "-q", "--", "shop/pricing.py"]);
    add_unrelated_edit(b.path());
    let cache = TempDir::new().unwrap();
    let command = format!(
        "git commit -qam vat; cd '{}' && git commit -qam restock",
        b.path().display()
    );
    let context = context_of(&run_line(a.path(), cache.path(), &command));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

/// #608: one commit pushed to two remotes moved one range: one review, and
/// no "other pushed ref(s)" left unanalysed.
#[test]
fn a_push_of_one_range_to_two_remotes_is_one_review() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let remotes = [TempDir::new().unwrap(), TempDir::new().unwrap()];
    for (name, remote) in ["origin", "mirror"].iter().zip(&remotes) {
        git(remote.path(), &["init", "-q", "--bare", "."]);
        git(
            repo,
            &["remote", "add", name, &remote.path().to_string_lossy()],
        );
        git(repo, &["push", "-q", name, "HEAD:refs/heads/main"]);
    }
    commit_all(repo, "vat");
    let cache = TempDir::new().unwrap();
    let command = "for r in origin mirror; do git push -q \"$r\" HEAD:refs/heads/main; done";
    let context = context_of(&run_line(repo, cache.path(), command));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("not analysed"), "{context}");
    assert_eq!(
        context.matches("diffctx impact for").count(),
        1,
        "{context}"
    );
}

/// #617: a line run in the background has its `PostToolUse` at launch,
/// before git records anything. What it records is reviewed at the
/// session's next hook event, once.
#[test]
fn a_background_commit_is_reviewed_at_the_next_event() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let command = "git commit -qam vat > commit.log 2>&1";
    let pre = hook(repo, cache.path(), &payload_in_session("s1", repo, command));
    assert_eq!(pre.stdout, "");
    let mut launched: serde_json::Value =
        serde_json::from_str(&payload_in_session("s1", repo, command)).unwrap();
    launched["hook_event_name"] = "PostToolUse".into();
    launched["tool_response"] = serde_json::json!({"backgroundTaskId": "b1", "stdout": ""});
    let at_launch = hook_with(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &launched.to_string(),
    );
    assert_eq!(at_launch.stdout, "", "{}", last_log_line(cache.path()));
    assert!(
        last_log_line(cache.path()).contains(" pending "),
        "{}",
        last_log_line(cache.path())
    );
    sh(repo, &[], command);
    let next = payload_in_session("s1", repo, "git log --oneline -1");
    let context = context_of(&hook(repo, cache.path(), &next));
    assert!(context.contains("background"), "{context}");
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert_eq!(hook(repo, cache.path(), &next).stdout, "");
}

/// #618: a push another checkout made while this line committed is not
/// this line's push; the line's own commit is what gets reviewed.
#[test]
fn another_checkouts_push_during_a_commit_is_not_this_lines() {
    let (tmp, _remote) = repo_with_branches_beside_origin();
    let repo = tmp.path();
    let other = TempDir::new().unwrap();
    let other_wt = other.path().join("wt");
    git(
        repo,
        &[
            "worktree",
            "add",
            "-q",
            &other_wt.to_string_lossy(),
            "feature",
        ],
    );
    write(
        repo,
        "shop/tax.py",
        "def rate(country):\n    return 0.0 if country == 'exempt' else 0.19\n",
    );
    let cache = TempDir::new().unwrap();
    let command = "git commit -qam exempt";
    let h = hook_pair("s1", repo, cache.path(), command, || {
        sh(repo, &[], command);
        git(&other_wt, &["push", "-q", "origin", "feature"]);
    });
    let context = context_of(&h);
    assert!(context.contains("what this commit recorded"), "{context}");
    assert!(context.contains("shop/invoice.py::bill"), "{context}");
    assert!(!context.contains("shop/checkout.py::charge"), "{context}");
}

/// #556: a line that commits in another repository is answered with that
/// commit, never with what moved in the cwd's repository meanwhile.
#[test]
fn a_commit_in_another_repository_is_not_answered_with_the_cwds() {
    let cwd = repo_with_pending_change();
    let there = repo_with_pending_change();
    git(there.path(), &["checkout", "-q", "--", "shop/pricing.py"]);
    add_unrelated_edit(there.path());
    let cache = TempDir::new().unwrap();
    let command = format!("cd '{}' && git commit -qam restock", there.path().display());
    let h = hook_pair("s1", cwd.path(), cache.path(), &command, || {
        sh(cwd.path(), &[], &command);
        commit_all(cwd.path(), "another session's vat");
    });
    let context = context_of(&h);
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
    assert!(!context.contains("shop/checkout.py::charge"), "{context}");
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

fn hook_log(cache: &Path) -> String {
    std::fs::read_to_string(cache.join("diffctx").join("hook-runs.log")).unwrap()
}

fn last_log_line(cache: &Path) -> String {
    hook_log(cache).lines().last().unwrap().to_string()
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
    let h = run_line(repo, cache.path(), command);
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
    let context = context_of(&run_line(
        repo,
        cache.path(),
        "git commit -m vat -- shop/pricing.py",
    ));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/shelf.py"), "{context}");
    assert!(!context.contains("uncommitted changes"), "{context}");

    // `-i` adds the paths to what is already staged.
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    add_unrelated_edit(repo);
    git(repo, &["add", "shop/inventory.py"]);
    write(repo, "README.md", "# shop, again\n");
    let cache = TempDir::new().unwrap();
    let context = context_of(&run_line(
        repo,
        cache.path(),
        "git commit -m vat -i shop/pricing.py",
    ));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(context.contains("shop/shelf.py::refill"), "{context}");
}

/// #508: paths read from a file the line never shows: the planner reviewed
/// another session's staged file with them; the commit's record holds only
/// what it took.
#[test]
fn a_commit_of_paths_from_a_file_reviews_only_those_paths() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    add_unrelated_edit(repo);
    git(repo, &["add", "shop/inventory.py"]);
    write(repo, "paths.txt", "shop/pricing.py\n");
    let context = context_of(&run_line(
        repo,
        cache.path(),
        "git commit -q --pathspec-from-file=paths.txt -m vat",
    ));
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/shelf.py"), "{context}");
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
    let h = line_in(
        "s1",
        elsewhere.path(),
        cache.path(),
        &[("HOME", home)],
        &command,
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    let line = last_log_line(cache.path());
    assert!(!line.contains("unsupported-syntax"), "{line}");
}

#[test]
fn a_commit_in_a_linked_worktree_is_read_on_its_own_branch() {
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
    let h = run_line(
        &wt,
        cache.path(),
        "git add shop/pricing.py && git commit -m vat",
    );
    let context = context_of(&h);
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    assert!(!context.contains("shop/shelf.py"), "{context}");
}

#[test]
fn the_first_commit_of_a_repository_is_read_from_the_empty_tree() {
    let tmp = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
    write(repo, "a.py", "def f():\n    return 1\n");
    let h = run_line(repo, cache.path(), "git add -A && git commit -qm init");
    assert_eq!(h.code, Some(0));
    let line = last_log_line(cache.path());
    assert!(
        line.contains(" commit ") && line.contains(" empty "),
        "{line}"
    );
    assert!(
        h.stdout.contains("reaches nothing outside the diff"),
        "{}",
        h.stdout
    );
}

/// Whatever stages the change — a patch, a loop, `xargs`, an option git
/// takes before its verb, a message with escaped quotes (#436) — the commit's
/// record is what is reviewed, and only what it took (#508).
#[test]
fn staging_by_any_means_is_read_off_the_commit() {
    for command in [
        "git apply --cached vat.patch && git commit -m vat",
        "for f in shop/pricing.py; do git add \"$f\"; done && git commit -m vat",
        "echo shop/pricing.py | xargs git add && git commit -m vat",
        "git --git-dir .git --work-tree . add shop/pricing.py && git --git-dir .git commit -m vat",
        "git add shop/pricing.py && git commit -m \"vat \\\"incl\\\" tax\"",
    ] {
        let tmp = repo_with_pending_change();
        let repo = tmp.path();
        let diff = Command::new("git")
            .current_dir(repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .args(["diff"])
            .output()
            .unwrap();
        std::fs::write(repo.join("vat.patch"), diff.stdout).unwrap();
        add_unrelated_edit(repo);
        let cache = TempDir::new().unwrap();
        let h = run_line(repo, cache.path(), command);
        assert!(
            h.stdout.contains("shop/checkout.py::charge"),
            "{command}: {}",
            last_log_line(cache.path())
        );
        assert!(
            !h.stdout.contains("shop/shelf.py"),
            "{command}: {}",
            h.stdout
        );
    }
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
    // Quoted as a shell carries it: a Windows path's backslashes are not escapes.
    let command = format!(
        "git -C '{}' add -A && git commit -m vat",
        other.path().display()
    );
    let h = run_line(repo, cache.path(), &command);
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        last_log_line(cache.path())
    );
}

/// The gate reads a copy of the index: another process holding the index
/// lock neither blocks it nor is disturbed by it, and it leaves nothing
/// behind.
#[test]
fn the_gate_never_touches_the_index_it_reads() {
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
    let h = hook_with(
        repo,
        cache.path(),
        "pretooluse",
        &["--gate"],
        &payload(repo, "git commit -m vat"),
    );
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        last_log_line(cache.path())
    );
    assert_eq!(listing(), before);
    assert_eq!(std::fs::read(git_dir.join("index")).unwrap(), index);
}

#[test]
fn a_manual_run_that_found_nothing_leaves_no_reminder() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "--", "shop/pricing.py"]);
    write(repo, "README.md", "# shop\n\nVAT included.\n");
    let status = diffctx()
        .current_dir(repo)
        .env("DIFFCTX_CACHE_DIR", cache.path())
        .args(["--mode", "impact", "-q"])
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let h = run_line(repo, cache.path(), "git commit -am docs");
    assert!(
        h.stdout.contains("reaches nothing outside the diff"),
        "{}",
        h.stdout
    );
    assert!(!h.stdout.contains("earlier"), "{}", h.stdout);
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
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
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
    let h = run_line(
        repo,
        cache.path(),
        &format!("git merge --ff-only origin/{branch}"),
    );
    assert_eq!(h.stdout, "", "{}", last_log_line(cache.path()));
    let cache = TempDir::new().unwrap();
    let h = run_line(repo, cache.path(), "git merge topic");
    assert!(
        h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        last_log_line(cache.path())
    );
}

/// #407: a cache the hook cannot write used to disable the dedupe without a
/// word, so every commit reran the pipeline; the temporary directory stands
/// in, for the dedupe and for the record a commit leaves its own answer.
#[cfg(unix)]
#[test]
fn an_unwritable_cache_still_reviews_a_change_once() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let locked = TempDir::new().unwrap();
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let scratch = TempDir::new().unwrap();
    let env = [("TMPDIR", scratch.path())];
    let diff = || {
        hook_in_env(
            repo,
            locked.path(),
            "posttooluse",
            &[],
            &env,
            &payload_for("PostToolUse", repo, "git diff"),
        )
    };
    let first = diff();
    assert!(
        first.stdout.contains("shop/checkout.py::charge"),
        "{}",
        first.stdout
    );
    assert_eq!(diff().stdout, "", "reviewed once");
    let landed = line_in("s1", repo, locked.path(), &env, "git commit -am vat");
    assert_reminder(&landed);
    std::fs::set_permissions(locked.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// #462: `git pull --rebase && git push` on one line. The pull moves the
/// remote-tracking ref first; what the push sent is measured from where the
/// push found it, not from where the line did, or another session's pushed
/// commit reads as this one's.
#[test]
fn a_push_after_a_pull_on_the_same_line_reviews_only_what_it_sent() {
    // The reopened shape lists the fetched commits first, then rebases.
    for line in [
        "git pull -q --rebase origin main && git push -q origin HEAD:main",
        "git fetch -q origin && git log --oneline HEAD..origin/main && git rebase -q origin/main && git push -q origin HEAD:main",
    ] {
        let tmp = repo_with_pending_change();
        let repo = tmp.path();
        let remote = TempDir::new().unwrap();
        git(remote.path(), &["init", "-q", "--bare", "."]);
        git(
            repo,
            &["remote", "add", "origin", &remote.path().to_string_lossy()],
        );
        git(repo, &["stash", "-q"]);
        git(repo, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
        git(repo, &["fetch", "-q", "origin"]);
        // Another session pushes a restock change.
        let other = TempDir::new().unwrap();
        git(
            other.path(),
            &[
                "clone",
                "-q",
                "-b",
                "main",
                &remote.path().to_string_lossy(),
                ".",
            ],
        );
        git(other.path(), &["config", "user.email", "test@example.com"]);
        git(other.path(), &["config", "user.name", "diffctx tests"]);
        add_unrelated_edit(other.path());
        commit_all(other.path(), "restock elsewhere");
        git(other.path(), &["push", "-q", "origin", "HEAD:main"]);
        // This session commits its own change, then lands it.
        git(repo, &["stash", "pop", "-q"]);
        commit_all(repo, "vat");
        let cache = TempDir::new().unwrap();
        let context = context_of(&run_line(repo, cache.path(), line));
        assert!(
            context.contains("shop/checkout.py::charge"),
            "{line}: {context}"
        );
        assert!(
            !context.contains("shop/shelf.py"),
            "{line}: the other session's restock was fetched, not pushed: {context}"
        );
    }
}

/// #448: a name defined only in an untracked file is the pending change's
/// as much as an edited one: a search for it is answered with its callers.
#[test]
fn a_search_for_a_name_defined_in_an_untracked_file_is_answered() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
    write(
        repo,
        "main.py",
        "from tools import convert\n\n\ndef run(x):\n    return convert(x)\n",
    );
    commit_all(repo, "main");
    write(repo, "tools.py", "def convert(x):\n    return int(x)\n");
    let cache = TempDir::new().unwrap();
    let h = hook_with(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &search_payload(repo, "Grep", "def convert"),
    );
    let context = context_of(&h);
    assert!(context.contains("`convert`"), "{context}");
    assert!(context.contains("main.py::run"), "{context}");
}

/// #486: `git grep PATTERN REV -- PATH` names a revision before `--`; it is
/// no directory, and the search is answered like any other.
#[test]
fn a_git_grep_over_a_revision_is_answered() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let h = hook_with(
        repo,
        cache.path(),
        "posttooluse",
        &[],
        &search_payload(repo, "Bash", r"git grep -n 'def total(' HEAD -- shop/"),
    );
    assert_eq!(h.code, Some(0));
    let context = output_of(&h, "PostToolUse")["additionalContext"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    let log = hook_log(cache.path());
    assert!(!log.contains("not-git"), "{log}");
}

/// #402: a qualified chain in a search pattern searches for its last name;
/// `total.rate` is no search for the edited `total`, while an alternation
/// branch that names it still is.
#[test]
fn a_qualified_search_word_is_not_the_name_it_qualifies() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let answer = |query: &str| {
        let cache = TempDir::new().unwrap();
        let h = hook_with(
            repo,
            cache.path(),
            "posttooluse",
            &[],
            &search_payload(repo, "Bash", query),
        );
        assert_eq!(h.code, Some(0));
        h.stdout
    };
    assert_eq!(answer(r"grep -rn 'total\.rate' shop/"), "");
    assert_eq!(answer("grep -rn 'total.rate' shop/"), "");
    assert_eq!(
        answer("grep -rn 'for (cart, total) in rows' shop/"),
        "",
        "a line of code names several identifiers, not one"
    );
    assert!(
        answer(r"grep -rn 'cart.rate\|def total' shop/").contains("shop/checkout.py::charge"),
        "an alternation branch naming `total` is a search for it"
    );
}

/// #569: markers past their TTL (`SEEN_TTL`, or `UPGRADE_NOTICE_TTL` for
/// `upgrade-*`) cannot be read by any hook run; rotating the log sweeps
/// them while keeping live ones.
#[test]
fn the_log_rotation_sweeps_markers_no_reader_can_ask_for() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let dir = cache.path().join("diffctx");
    let seen = dir.join("seen");
    std::fs::create_dir_all(&seen).unwrap();

    let day = std::time::Duration::from_secs(24 * 60 * 60);
    let stamp = |name: &str, age: std::time::Duration| {
        let path = seen.join(name);
        std::fs::write(&path, b"headline").unwrap();
        let mtime = std::time::SystemTime::now() - age;
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
    };
    stamp("id-stale.s1", day);
    stamp("upgrade-0.9.0.s1", 40 * day);
    stamp("upgrade-0.9.1.s1", 2 * day);
    stamp("id-fresh.s1", std::time::Duration::from_secs(60));

    std::fs::write(dir.join("hook-runs.log"), vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
    let h = hook(tmp.path(), cache.path(), &payload(tmp.path(), "git status"));
    assert_eq!(h.code, Some(0));

    assert!(dir.join("hook-runs.log.1").is_file(), "the log rotated");
    assert!(!seen.join("id-stale.s1").exists(), "day-old marker swept");
    assert!(
        !seen.join("upgrade-0.9.0.s1").exists(),
        "40-day upgrade notice swept"
    );
    assert!(
        seen.join("upgrade-0.9.1.s1").is_file(),
        "2-day upgrade notice kept"
    );
    assert!(seen.join("id-fresh.s1").is_file(), "fresh marker kept");
}

/// The sweep does not wait for the log to rotate, which takes tens of
/// thousands of runs. It runs once a day on its own stamp, not on every hook
/// run.
#[test]
fn the_marker_sweep_runs_daily_without_a_rotation() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let seen = cache.path().join("diffctx").join("seen");
    std::fs::create_dir_all(&seen).unwrap();
    let aged = |name: &str| {
        let path = seen.join(name);
        std::fs::write(&path, "").unwrap();
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(then)
            .unwrap();
        path
    };
    let stale = aged("id-0123.s1");
    run_line(tmp.path(), cache.path(), "git status");
    assert!(
        !cache.path().join("diffctx/hook-runs.log.1").exists(),
        "no rotation"
    );
    assert!(!stale.exists(), "swept without a rotation");

    let later = aged("id-89ab.s1");
    run_line(tmp.path(), cache.path(), "git status");
    assert!(later.exists(), "one sweep a day, not one per run");
}

/// #534: the body of a quoted here-document is data — Markdown code spans,
/// a `$(…)` quoted in an issue body — and so is a single-quoted argument or
/// a comment: none of them is git the shell hides.
#[test]
fn quoted_text_naming_git_hides_no_git() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    for command in [
        "cat > notes.md <<'X'\nRun `git commit -qam change` then `git push`, or $(git push origin main).\nX",
        "cat > notes.md <<\"X\"\nRun `git commit -qam change`.\nX",
        "cat > notes.md <<\\X\nRun `git commit -qam change`.\nX",
        "cat > notes.md <<-'X'\n\tRun `git push`.\n\tX",
        "gh issue create --title t --body \"$(cat <<'EOF'\nrepro: `git commit -qam change` (then push)\nEOF\n)\"",
        "gh issue create --title t --body 'Run `git commit -am x` first'",
        "git status # never `git push` from here",
    ] {
        let cache = TempDir::new().unwrap();
        let h = hook(repo, cache.path(), &payload(repo, command));
        assert_eq!((h.code, h.stdout.as_str()), (Some(0), ""), "{command}");
        assert!(
            !last_log_line(cache.path()).contains("unsupported-syntax"),
            "{command}"
        );
    }
    // What the shell does run still hides git: an unquoted here-document
    // expands its body, and a `-c` or `eval` script is code. The cwd's
    // refs are recorded for git's record to answer once the line ran.
    for command in [
        "cat > notes.md <<X\n$(git commit -qam change)\nX",
        "cat <<X\n`git push`\nX",
        "sh -c 'git commit -am x'",
        "eval 'git push'",
    ] {
        let cache = TempDir::new().unwrap();
        let h = hook(repo, cache.path(), &payload(repo, command));
        assert_eq!(h.stdout, "", "{command}");
        let line = last_log_line(cache.path());
        assert!(line.contains(" recorded "), "{command}: {line}");
    }
    // A commit after the here-document's opening line is still reviewed.
    let cache = TempDir::new().unwrap();
    let h = run_line(
        repo,
        cache.path(),
        "cat > msg.txt <<'EOF' && git commit -qam vat\nbody `git push` $(git push)\nEOF",
    );
    let context = context_of(&h);
    assert!(
        context.starts_with("diffctx reviewed what this commit recorded."),
        "{context}"
    );
    assert!(context.contains("shop/checkout.py::charge"), "{context}");
    // So is the usual message form, whose text names a push.
    write(repo, "shop/pricing.py", "def total(items):\n    return 0\n");
    let h = run_line(
        repo,
        cache.path(),
        "git commit -qam \"$(cat <<'EOF'\nvat: `git push` follows\nEOF\n)\"",
    );
    assert!(
        context_of(&h).starts_with("diffctx reviewed what this commit recorded."),
        "{}",
        h.stdout
    );
}

/// #575: a word the line hands to another program — a jq path, an API
/// route, a pattern — is no git verb, so a line that only reads does not
/// take a commit another session made while it ran as its own.
#[test]
fn a_commit_named_in_another_programs_argument_is_not_this_lines() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    for (i, command) in [
        "git rev-parse HEAD; gh api repos/o/r/commits/abc --jq '.commit.author.date'",
        "git rev-parse HEAD; gh api repos/o/r/commits/abc --jq .commit.author.date",
        "git log -1 --format=%s | grep -c commit",
    ]
    .into_iter()
    .enumerate()
    {
        let pre = hook(repo, cache.path(), &payload(repo, command));
        assert_eq!(pre.stdout, "", "{command}");
        write(
            repo,
            "shop/pricing.py",
            &format!("def total(items):\n    return sum(i.price for i in items) + {i}\n"),
        );
        git(repo, &["commit", "-qam", "another session"]);
        let post = hook_with(
            repo,
            cache.path(),
            "posttooluse",
            &[],
            &payload_for("PostToolUse", repo, command),
        );
        assert_eq!(
            post.stdout, "",
            "{command}: another session's commit is not this line's"
        );
    }
    write(repo, "shop/pricing.py", "def total(items):\n    return 0\n");
    let h = run_line(
        repo,
        cache.path(),
        "git commit -qam vat; echo '{\"commit\": 1}' | grep -c commit",
    );
    let context = context_of(&h);
    assert!(
        context.starts_with("diffctx reviewed what this commit recorded."),
        "{context}"
    );
}

/// #556: from a session whose cwd is the main checkout, a commit made with
/// `cd <linked worktree> && git commit` is the worktree's, and a change
/// whose only reach is a config naming it by path says so, never that it
/// reaches nothing.
#[test]
fn a_commit_in_a_linked_worktree_entered_with_cd_names_what_names_it() {
    let tmp = TempDir::new().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "."]);
    git(&main, &["config", "user.email", "test@example.com"]);
    git(&main, &["config", "user.name", "diffctx tests"]);
    write(&main, "deploy/values.yaml", "a: 1\n");
    write(&main, "refs.yaml", "values: deploy/values.yaml\n");
    commit_all(&main, "initial");
    let wt = tmp.path().join("wt");
    git(
        &main,
        &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "side"],
    );
    write(&wt, "deploy/values.yaml", "a: 2\n");
    git(&wt, &["add", "deploy/values.yaml"]);
    let cache = TempDir::new().unwrap();
    let command = format!(
        "cd '{}' && git commit -q -F - <<'EOF' > '{}' 2>&1\nvalues: a 2\nEOF\ngit log --oneline -1",
        wt.display(),
        cache.path().join("commit.log").display()
    );
    let h = run_line(&main, cache.path(), &command);
    let context = context_of(&h);
    assert!(
        context.starts_with("diffctx reviewed what this commit recorded."),
        "{context}"
    );
    assert!(
        context.contains("refs.yaml:1 names deploy/values.yaml"),
        "{context}"
    );
}

/// A transcript timestamp as Claude Code writes it: UTC, milliseconds.
fn iso(t: std::time::SystemTime) -> String {
    let ms = i64::try_from(t.duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap();
    let (days, of_day) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000));
    // H. Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600_000,
        of_day / 60_000 % 60,
        of_day / 1000 % 60,
        of_day % 1000
    )
}

/// The transcript lines of one Bash call: its `tool_use` at `from` and,
/// once it returned, its `tool_result` at `to`.
fn bash_call(id: &str, from: std::time::SystemTime, to: Option<std::time::SystemTime>) -> String {
    let mut lines = vec![serde_json::json!({"type": "assistant", "timestamp": iso(from), "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": id, "name": "Bash", "input": {"command": "python3 - <<'EOF'\n...\nEOF"}}
    ]}})
    .to_string()];
    if let Some(to) = to {
        lines.push(
            serde_json::json!({"type": "user", "timestamp": iso(to), "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": id, "content": "ok"}
            ]}})
            .to_string(),
        );
    }
    lines.join("\n") + "\n"
}

/// #453: a file the session's own shell rewrote (`sed -i`, a heredoc
/// script, `git rm`) is its edit, as one its edit tools wrote is; another
/// session's edit, made while none of its Bash calls ran, still is not
/// (#404).
#[test]
fn a_file_the_sessions_shell_wrote_is_its_own() {
    use std::time::{Duration, SystemTime};
    // `pricing.total` is another session's edit, made before any shell call
    // of this one returned or ran.
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let theirs = SystemTime::now();
    while SystemTime::now() < theirs + Duration::from_millis(1500) {
        std::thread::sleep(Duration::from_millis(50));
    }
    let from = SystemTime::now();
    add_unrelated_edit(repo);
    std::fs::remove_file(repo.join("README.md")).unwrap();
    let to = SystemTime::now() + Duration::from_millis(1);

    let dir = TempDir::new().unwrap();
    let main = dir.path().join("b.jsonl");
    std::fs::write(
        &main,
        [
            // Returned before their edit; still running, so no span yet.
            bash_call(
                "t1",
                theirs - Duration::from_secs(20),
                Some(theirs - Duration::from_secs(10)),
            ),
            bash_call("t2", theirs - Duration::from_secs(5), None),
            // The `git rm` of README.md.
            bash_call("t3", from, Some(to)),
        ]
        .concat(),
    )
    .unwrap();
    let subagents = dir.path().join("b").join("subagents");
    std::fs::create_dir_all(&subagents).unwrap();
    // A subagent's heredoc rewrite of `inventory.restock`.
    std::fs::write(
        subagents.join("agent-1.jsonl"),
        bash_call("t4", from, Some(to)),
    )
    .unwrap();

    let h = inspect_with_transcript(repo, "git diff", Some(&main));
    assert!(h.stdout.contains("shop/shelf.py::refill"), "{}", h.stdout);
    assert!(h.stdout.contains("(1 deleted)"), "{}", h.stdout);
    assert!(
        !h.stdout.contains("shop/checkout.py::charge"),
        "{}",
        h.stdout
    );
    assert!(
        h.stdout.contains(
            "1 other changed file(s) show no write by this session or its subagents and are read as committed: shop/pricing.py."
        ),
        "{}",
        h.stdout
    );
}

/// #453: a definition the session added, called from the pending text of a
/// changed file it shows no write of: that file is read as committed, so
/// the answer says the definition's callers may be missing rather than that
/// nothing calls it.
#[test]
fn a_caller_in_a_file_read_as_committed_is_named_as_an_unknown() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    write(
        repo,
        "shop/discount.py",
        "def discount(amount):\n    return amount * 0.9\n",
    );
    write(
        repo,
        "shop/pricing.py",
        "from shop.discount import discount\n\n\ndef total(items):\n    return discount(sum(i.price for i in items))\n",
    );
    // Names only a longer word.
    write(
        repo,
        "shop/inventory.py",
        "def restock(item):\n    # discounts apply later\n    return item.count + 1\n",
    );
    let dir = TempDir::new().unwrap();
    let own = transcript(dir.path(), &[repo.join("shop/discount.py")], &[]);
    let h = inspect_with_transcript(repo, "git diff", Some(&own));
    assert!(
        h.stdout.contains("2 other changed file(s) show no write by this session or its subagents and are read as committed: shop/inventory.py, shop/pricing.py."),
        "{}",
        h.stdout
    );
    assert!(
        h.stdout.contains(
            "(callers may be missing: discount named in 1 changed file(s) read as committed: shop/pricing.py)"
        ),
        "{}",
        h.stdout
    );
}

/// #549: the header names what the snapshot holds — the index for a staged
/// inspection, this session's edits for a scoped one — not a bare tree id.
#[test]
fn the_header_names_what_the_snapshot_holds() {
    let tmp = repo_with_pending_change();
    let repo = tmp.path();
    let fresh = |command: &str| {
        let cache = TempDir::new().unwrap();
        context_of(&inspect(repo, cache.path(), command))
    };
    let all = fresh("git diff");
    assert!(
        all.contains("diffctx impact for uncommitted changes:"),
        "{all}"
    );
    git(repo, &["add", "shop/pricing.py"]);
    let staged = fresh("git diff --cached");
    assert!(
        staged.contains("diffctx impact for staged changes (index tree "),
        "{staged}"
    );
    add_unrelated_edit(repo);
    let dir = TempDir::new().unwrap();
    let own = transcript(dir.path(), &[repo.join("shop/inventory.py")], &[]);
    let scoped = inspect_with_transcript(repo, "git diff", Some(&own));
    assert!(
        scoped
            .stdout
            .contains("diffctx impact for this session's edits (snapshot tree "),
        "{}",
        scoped.stdout
    );
}

/// #555: a pin at or below 1.18.4 cuts `hook.log` at 256 KiB, each time
/// overwriting what it rotated before; the runs this binary logged survive.
#[test]
fn an_older_pins_log_rotation_keeps_this_binarys_history() {
    let tmp = repo_with_pending_change();
    let cache = TempDir::new().unwrap();
    let repo = tmp.path();
    let h = hook(
        repo,
        cache.path(),
        &payload_in_session("kept-1", repo, "ls"),
    );
    assert_eq!(h.code, Some(0));
    let dir = cache.path().join("diffctx");
    for _ in 0..2 {
        let old = dir.join("hook.log");
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&old)
            .and_then(|mut f| f.write_all(&vec![b'x'; 256 * 1024 + 1]))
            .unwrap();
        std::fs::rename(&old, dir.join("hook.log.1")).unwrap();
    }
    let kept: String = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| std::fs::read_to_string(e.ok()?.path()).ok())
        .collect();
    assert!(
        kept.lines().any(|l| l.split(' ').nth(1) == Some("kept-1")),
        "the run's line is gone"
    );
}
