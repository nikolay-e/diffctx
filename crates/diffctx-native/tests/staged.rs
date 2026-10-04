// `--diff staged` (#354): the index captured once as a tree is the whole
// current side of the run — the changed files, the callers, their imports,
// the stale-reference search. The working tree is never read in its place,
// so every case here makes the disk say something different from the index.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git_out(repo: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()
        .expect("run git")
}

fn git(repo: &Path, args: &[&str]) {
    let out = git_out(repo, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(repo: &Path, extra: &[&str]) {
    let mut args = vec!["init", "-q"];
    args.extend(extra);
    args.push(".");
    git(repo, &args);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write");
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_NO_MARKER", "1")
        .args(args)
        .output()
        .expect("run diffctx")
}

fn impact(repo: &Path, range: &str) -> serde_json::Value {
    let out = run(
        repo,
        &[
            &format!("--diff={range}"),
            "--mode",
            "impact",
            "-q",
            "-f",
            "json",
        ],
    );
    assert!(
        out.status.success(),
        "impact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn markdown(repo: &Path, range: &str) -> String {
    let out = run(
        repo,
        &[
            &format!("--diff={range}"),
            "--mode",
            "impact",
            "-q",
            "-f",
            "md",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

fn callers(doc: &serde_json::Value, symbol: &str) -> Vec<String> {
    let mut found: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == symbol)
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    found.sort();
    found
}

fn changed_symbols(doc: &serde_json::Value) -> Vec<String> {
    let mut found: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["symbol"].as_str().map(str::to_string))
        .collect();
    found.sort();
    found
}

/// `shop/pricing.py::total` and `tax`; `shop/checkout.py::charge` calls
/// `total`, `shop/billing.py::invoice` calls `tax`.
fn shop() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo, &[]);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return sum(items)\n\n\ndef tax(amount):\n    return amount * 0.19\n",
    );
    write(
        repo,
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
    );
    write(
        repo,
        "shop/billing.py",
        "from shop.pricing import tax\n\n\ndef invoice(amount):\n    return tax(amount)\n",
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "initial"]);
    tmp
}

const TOTAL_CHANGED: &str = "def total(items):\n    return sum(items) * 2\n\n\ndef tax(amount):\n    return amount * 0.19\n";

#[test]
fn an_unstaged_file_is_not_part_of_the_staged_change() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    git(repo, &["add", "shop/pricing.py"]);
    write(
        repo,
        "shop/report.py",
        "from shop.pricing import total\n\n\ndef summarize(rows):\n    return total(rows)\n",
    );
    let doc = impact(repo, "staged");
    assert_eq!(doc["changed_files"], serde_json::json!(["shop/pricing.py"]));
    assert_eq!(callers(&doc, "total"), vec!["shop/checkout.py::charge"]);
}

#[test]
fn only_the_staged_half_of_a_file_is_the_change() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    git(repo, &["add", "shop/pricing.py"]);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return sum(items) * 2\n\n\ndef tax(amount):\n    return amount * 0.25\n",
    );
    let doc = impact(repo, "staged");
    assert_eq!(changed_symbols(&doc), vec!["total"], "{doc}");
    let disk = impact(repo, "HEAD");
    assert!(
        changed_symbols(&disk).contains(&"tax".to_string()),
        "the working tree does change tax: {disk}"
    );
}

#[test]
fn an_unchanged_caller_is_read_from_the_index_not_the_disk() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    git(repo, &["add", "shop/pricing.py"]);
    write(
        repo,
        "shop/checkout.py",
        "def charge(cart):\n    return sum(cart)\n",
    );
    let tree = String::from_utf8(git_out(repo, &["write-tree"]).stdout).unwrap();
    for range in [format!("HEAD..{}", tree.trim()), "staged".to_string()] {
        let doc = impact(repo, &range);
        assert_eq!(
            callers(&doc, "total"),
            vec!["shop/checkout.py::charge"],
            "{range}: the index still calls total; the disk's edit is not staged: {doc}"
        );
    }
}

#[test]
fn the_index_imports_decide_the_binding_not_the_disk() {
    let tmp = shop();
    let repo = tmp.path();
    write(
        repo,
        "shop/legacy.py",
        "def total(rows):\n    return len(rows)\n",
    );
    write(
        repo,
        "shop/refund.py",
        "from shop.pricing import total\n\n\ndef refund(cart):\n    return -total(cart)\n",
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "refunds"]);
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    git(repo, &["add", "shop/pricing.py"]);
    write(
        repo,
        "shop/refund.py",
        "from shop.legacy import total\n\n\ndef refund(cart):\n    return -total(cart)\n",
    );
    let staged = impact(repo, "staged");
    assert!(
        callers(&staged, "total").contains(&"shop/refund.py::refund".to_string()),
        "the index's refund.py imports shop.pricing: {staged}"
    );
    let disk = impact(repo, "HEAD");
    assert!(
        !callers(&disk, "total").contains(&"shop/refund.py::refund".to_string()),
        "the working tree's refund.py imports shop.legacy: {disk}"
    );
}

#[test]
fn staged_deletes_and_renames_are_what_stale_references_search() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "Dockerfile", "COPY shop/billing.py /app/\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "docker"]);
    git(repo, &["rm", "-q", "shop/billing.py"]);
    write(repo, "Dockerfile", "COPY shop/ /app/\n");
    let doc = impact(repo, "staged");
    assert_eq!(doc["deleted_files"], serde_json::json!(["shop/billing.py"]));
    let stale = doc["stale_references"].to_string();
    assert!(
        stale.contains("Dockerfile"),
        "the index's Dockerfile still names the deleted file; the disk's does not: {doc}"
    );
}

#[test]
fn the_first_commit_is_reviewed_against_the_empty_tree() {
    for format in [None, Some("sha256")] {
        let tmp = TempDir::new().expect("tempdir");
        let repo = tmp.path();
        let extra: Vec<String> = format
            .map(|f| vec![format!("--object-format={f}")])
            .unwrap_or_default();
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        init_repo(repo, &extra);
        write(repo, "lib.py", "def area(r):\n    return 3.14 * r * r\n");
        write(
            repo,
            "app.py",
            "from lib import area\n\n\ndef main():\n    return area(2)\n",
        );
        git(repo, &["add", "-A"]);
        let doc = impact(repo, "staged");
        let files = doc["changed_files"].to_string();
        assert!(
            files.contains("lib.py") && files.contains("app.py"),
            "{format:?}: {doc}"
        );
        assert!(
            markdown(repo, "staged").starts_with("diffctx impact for staged changes (index tree "),
            "{format:?}"
        );
    }
}

#[test]
fn an_empty_index_diff_is_an_empty_change_set() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    let doc = impact(repo, "staged");
    assert_eq!(doc["changed_files"], serde_json::json!([]));
    assert_eq!(doc["empty"], true);
    let text = markdown(repo, "staged");
    assert!(text.contains("No changes to analyse."), "{text}");
    assert!(!text.contains("No resolved static callers"), "{text}");
}

#[test]
fn an_unmerged_index_is_an_error_not_the_working_tree() {
    let tmp = shop();
    let repo = tmp.path();
    git(repo, &["checkout", "-q", "-b", "other"]);
    write(repo, "shop/pricing.py", "def total(items):\n    return 1\n");
    git(repo, &["commit", "-qam", "other"]);
    git(repo, &["checkout", "-q", "-"]);
    write(repo, "shop/pricing.py", "def total(items):\n    return 2\n");
    git(repo, &["commit", "-qam", "main"]);
    let merge = git_out(repo, &["merge", "-q", "other"]);
    assert!(!merge.status.success(), "the fixture needs a conflict");
    let out = run(repo, &["--diff", "staged", "--mode", "impact", "-q"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unmerged"), "{err}");
    assert!(out.stdout.is_empty());
}

#[test]
fn the_snapshot_identity_is_the_index_tree_and_every_spelling_agrees() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    git(repo, &["add", "shop/pricing.py"]);
    let tree = String::from_utf8(git_out(repo, &["write-tree"]).stdout)
        .unwrap()
        .trim()
        .to_string();
    let staged = impact(repo, "staged");
    assert_eq!(staged["index_tree"], tree.as_str());
    let cached = impact(repo, "--cached");
    let hook_form = impact(repo, &format!("HEAD..{tree}"));
    for other in [&cached, &hook_form] {
        assert_eq!(other["index_tree"], staged["index_tree"]);
        assert_eq!(other["changed"], staged["changed"]);
    }
    let text = markdown(repo, "staged");
    assert!(
        text.starts_with(&format!(
            "diffctx impact for staged changes (index tree {}",
            &tree[..10]
        )),
        "{text}"
    );
}

#[test]
fn a_caller_deleted_only_on_disk_is_still_a_caller_of_the_snapshot() {
    let tmp = shop();
    let repo = tmp.path();
    write(repo, "shop/pricing.py", TOTAL_CHANGED);
    git(repo, &["add", "shop/pricing.py"]);
    std::fs::remove_file(repo.join("shop/checkout.py")).unwrap();
    let tree = String::from_utf8(git_out(repo, &["write-tree"]).stdout).unwrap();
    for range in [format!("HEAD..{}", tree.trim()), "staged".to_string()] {
        let doc = impact(repo, &range);
        assert_eq!(
            callers(&doc, "total"),
            vec!["shop/checkout.py::charge"],
            "{range}: the index holds checkout.py whatever the disk lost: {doc}"
        );
    }
}
