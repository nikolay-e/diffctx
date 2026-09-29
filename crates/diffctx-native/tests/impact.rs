// `--mode impact` is what the plugin hook injects before a commit (#310):
// callers outside the diff, whether a test guards each, and cross-commit
// overlap. These run the release binary on real git repositories, the way
// the hook does.

use std::path::Path;
use std::process::Command;

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

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write");
}

fn impact(repo: &Path, range: &str) -> serde_json::Value {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", range, "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "impact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn impact_markdown(repo: &Path, range: &str) -> String {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", range, "--mode", "impact", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    String::from_utf8(out.stdout).expect("utf8")
}

/// `pricing.total` changes; `checkout.charge` calls it and is pinned by
/// `test_checkout.py`; `report.summarize` calls it and nothing tests it.
fn repo_with_callers() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return sum(i.price for i in items)\n",
    );
    write(
        repo,
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    amount = total(cart.items)\n    return cart.pay(amount)\n",
    );
    write(
        repo,
        "shop/report.py",
        "from shop.pricing import total\n\n\ndef summarize(orders):\n    return [total(o.items) for o in orders]\n",
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
    commit_all(repo, "pricing: totals include VAT");
    tmp
}

#[test]
fn callers_outside_the_diff_are_listed_with_their_test_guard() {
    let tmp = repo_with_callers();
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(doc["schema"], "diffctx.impact.v1");
    assert_eq!(doc["empty"], false);
    let changed = doc["changed"].as_array().expect("changed");
    let total = changed
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("the changed function is listed");
    let callers = total["callers"].as_array().expect("callers");
    let by_symbol = |name: &str| {
        callers
            .iter()
            .find(|c| c["symbol"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {callers:?}"))
    };
    let charge = by_symbol("charge");
    assert_eq!(charge["path"], "shop/checkout.py");
    assert_eq!(charge["tested_by"], "tests/test_checkout.py");
    let summarize = by_symbol("summarize");
    assert_eq!(summarize["path"], "shop/report.py");
    assert!(
        summarize.get("tested_by").is_none(),
        "nothing tests summarize: {summarize:?}"
    );
    assert!(
        !callers.iter().any(|c| c["path"] == "shop/pricing.py"),
        "the changed file is never its own caller"
    );
}

#[test]
fn the_text_form_names_every_caller_and_its_guard() {
    let tmp = repo_with_callers();
    let text = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(text.contains("shop/checkout.py::charge"), "{text}");
    assert!(text.contains("tested by tests/test_checkout.py"), "{text}");
    assert!(text.contains("shop/report.py::summarize"), "{text}");
    assert!(text.contains("UNTESTED"), "{text}");
    assert!(
        text.lines().count() <= 25,
        "the hook injects this before a commit; {} lines is not readable there",
        text.lines().count()
    );
}

#[test]
fn a_change_nothing_depends_on_is_empty() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(repo, "README.md", "# shop\n\nTotals include VAT.\n");
    commit_all(repo, "docs");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert_eq!(doc["empty"], true, "{doc}");
    assert!(doc["changed"].as_array().unwrap().is_empty());
    let text = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(text.contains("Nothing outside the diff depends"), "{text}");
}

#[test]
fn a_symbol_edited_by_two_commits_of_the_range_says_so() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "VAT = 1.19\n\n\ndef total(items):\n    return round(sum(i.price for i in items) * VAT, 2)\n",
    );
    commit_all(repo, "pricing: name the rate");
    let doc = impact(repo, "HEAD~2..HEAD");
    assert_eq!(doc["commit_count"], 2);
    let total = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("total listed");
    assert_eq!(total["commits"], 2, "{total}");
    let text = impact_markdown(repo, "HEAD~2..HEAD");
    assert!(text.contains("touched by 2 commits"), "{text}");
}

#[test]
fn the_working_tree_is_a_valid_range() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["range"], "HEAD");
    assert_eq!(doc["empty"], false);
    assert!(
        doc.get("commit_count").is_none(),
        "no commits for a dirty tree"
    );
}

#[test]
fn the_answer_fits_the_cap_and_says_what_it_dropped() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "svc/core.py", "def core(x):\n    return x\n");
    for i in 0..120 {
        write(
            repo,
            &format!("callers/caller_{i:03}.py"),
            &format!(
                "from svc.core import core\n\n\ndef use_{i:03}(v):\n    return core(v) + {i}\n"
            ),
        );
    }
    commit_all(repo, "initial");
    write(repo, "svc/core.py", "def core(x):\n    return x * 2\n");
    commit_all(repo, "double");
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", "HEAD~1..HEAD", "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let json = String::from_utf8(out.stdout).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        _diffctx::tokenizer::count_tokens(&json) <= _diffctx::impact::IMPACT_TOKEN_CAP,
        "{} tokens",
        _diffctx::tokenizer::count_tokens(&json)
    );
    assert!(doc["truncated"].as_u64().unwrap() > 0, "{doc}");
    let listed = doc["changed"][0]["callers"].as_array().unwrap().len();
    assert!(listed > 5, "kept the strongest callers, not none: {listed}");
}

#[test]
fn md_is_rejected_outside_impact_mode() {
    let tmp = repo_with_callers();
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args(["--diff", "HEAD~1..HEAD", "-f", "md"])
        .output()
        .expect("run diffctx");
    assert_eq!(out.status.code(), Some(2));
}
