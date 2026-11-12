// A pending change reads the checkout's untracked files collapsed, one level of
// directories at a time under a budget of entries (fj#170). A directory the
// budget leaves is part of the universe the run never listed: callers in it are
// unknown, so the answer is partial and names the directory, in every format.

use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::{commit_all, diffctx, init_repo, write};

/// More entries in one untracked directory than the read's budget (5000 below
/// the top level) holds.
const BULK_FILES: usize = 5_100;

fn repo_with_an_unreadable_directory() -> TempDir {
    let tmp = TempDir::new().unwrap();
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
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return cart.pay(total(cart.items))\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    );
    for n in 0..BULK_FILES {
        write(repo, &format!("bulk/f{n}.txt"), "x\n");
    }
    tmp
}

fn run(repo: &TempDir, args: &[&str]) -> Output {
    let out = diffctx()
        .current_dir(repo.path())
        .args([".", "--diff", "HEAD"])
        .args(args)
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn json(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).expect("JSON on stdout")
}

#[test]
fn an_impact_answer_names_the_untracked_directory_the_read_left() {
    let repo = repo_with_an_unreadable_directory();
    let doc = json(&run(&repo, &["--mode", "impact", "-f", "json"]));
    assert_eq!(doc["empty"], false, "{doc:#}");
    assert!(
        doc["coverage"]["limits"]
            .to_string()
            .contains("discovery_truncated"),
        "{doc:#}"
    );
    assert!(
        !doc["coverage"]["limits"]
            .to_string()
            .contains("read_failed"),
        "an unread directory is a discovery loss, not a failed read: {doc:#}"
    );
    let gaps = doc["coverage"]["scope"]["gaps"].as_array().expect("gaps");
    let gap = gaps
        .iter()
        .find(|g| g["kind"] == "unread_directories")
        .unwrap_or_else(|| panic!("no unread_directories gap: {doc:#}"));
    assert_eq!(gap["directories"], serde_json::json!(["bulk"]), "{gap}");
    assert_eq!(gap["total"], 1, "{gap}");

    let md =
        String::from_utf8_lossy(&run(&repo, &["--mode", "impact", "-f", "md"]).stdout).to_string();
    assert!(
        md.contains("1 untracked director(y/ies) not read: bulk"),
        "{md}"
    );
}

#[test]
fn a_pack_answer_names_the_untracked_directory_the_read_left() {
    let repo = repo_with_an_unreadable_directory();
    let doc = json(&run(&repo, &["-f", "json"]));
    assert_eq!(doc["coverage"]["status"], "partial", "{doc:#}");
    assert!(
        doc["coverage"]["limits"]
            .to_string()
            .contains("discovery_truncated"),
        "{doc:#}"
    );
    let files = doc["coverage"]["limited_files"].as_array().expect("files");
    assert!(
        files
            .iter()
            .any(|f| f["path"] == "bulk" && f["reason"] == "discovery_truncated"),
        "{doc:#}"
    );
}
