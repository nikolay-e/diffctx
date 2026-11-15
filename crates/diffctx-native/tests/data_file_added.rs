// A data file added whole is one change, not one per key (gh#731): a generated
// JSON or YAML artifact of a few hundred kilobytes left beside a one-line source
// edit became hundreds of "changed definition" fragments that spent the budget
// and hit the per-file fragment cap, while the edit itself was three lines.

use std::fmt::Write as _;
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::{commit_all, diffctx, init_repo, write};

const APP: &str = "package demo;\n\npublic class App {\n    public int total(int a, int b) {\n        return a + b;\n    }\n}\n";
const APP_EDITED: &str = "package demo;\n\npublic class App {\n    public int total(int a, int b) {\n        return a + b + 1;\n    }\n}\n";

fn rows(count: usize) -> String {
    let mut out = String::from("{\n  \"rows\": [\n");
    for n in 0..count {
        let comma = if n + 1 == count { "" } else { "," };
        let _ = write!(
            out,
            "    {{\n      \"id\": {n},\n      \"name\": \"item-{n}\",\n      \"tags\": [\n        \"a\",\n        \"b\"\n      ],\n      \"nested\": {{\n        \"k\": {n},\n        \"v\": \"xxxxxxxxxxxxxxxxxxxx\"\n      }}\n    }}{comma}\n"
        );
    }
    out.push_str("  ]\n}\n");
    out
}

fn manifest(count: usize) -> String {
    let mut out = String::from("items:\n");
    for n in 0..count {
        let _ = write!(
            out,
            "  - name: item-{n}\n    owner:\n      team: team-{n}\n      tier: gold\n    limits:\n      cpu: {n}\n      memory: 64\n"
        );
    }
    out
}

fn base() -> TempDir {
    let tmp = TempDir::new().unwrap();
    init_repo(tmp.path());
    write(tmp.path(), "src/App.java", APP);
    commit_all(tmp.path(), "initial");
    tmp
}

fn pack(repo: &TempDir, range: &str) -> Value {
    let out: Output = diffctx()
        .current_dir(repo.path())
        .args([".", "--diff", range, "--mode", "pack", "-f", "json"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{range}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON on stdout")
}

fn fragments_of<'a>(doc: &'a Value, path: &str) -> Vec<&'a Value> {
    doc["fragments"]
        .as_array()
        .expect("fragments")
        .iter()
        .filter(|f| f["path"] == path)
        .collect()
}

fn assert_one_bounded_note(doc: &Value, path: &str) {
    let parts = fragments_of(doc, path);
    assert_eq!(parts.len(), 1, "{path} must be one fragment: {doc:#}");
    let content = parts[0]["content"].as_str().unwrap();
    assert!(
        content.contains("more lines]"),
        "the cut says how much it left out: {content}"
    );
    assert!(content.lines().count() < 60, "{content}");
    assert_eq!(
        fragments_of(doc, "src/App.java").len(),
        1,
        "the source change stays whole: {doc:#}"
    );
    assert!(
        !doc["coverage"]["limits"]
            .to_string()
            .contains("fragment_limit"),
        "nothing was cut by the cap, the file was summarised: {doc:#}"
    );
}

#[test]
fn an_untracked_json_artifact_is_one_bounded_fragment() {
    let repo = base();
    write(repo.path(), "src/App.java", APP_EDITED);
    write(repo.path(), "generated-data.json", &rows(5_000));
    assert_one_bounded_note(&pack(&repo, "HEAD"), "generated-data.json");
}

#[test]
fn a_committed_json_artifact_is_one_bounded_fragment() {
    let repo = base();
    write(repo.path(), "src/App.java", APP_EDITED);
    write(repo.path(), "generated-data.json", &rows(5_000));
    commit_all(repo.path(), "add data");
    assert_one_bounded_note(&pack(&repo, "HEAD~1..HEAD"), "generated-data.json");
}

#[test]
fn an_untracked_yaml_artifact_is_one_bounded_fragment() {
    let repo = base();
    write(repo.path(), "src/App.java", APP_EDITED);
    write(repo.path(), "inventory.yaml", &manifest(2_000));
    assert_one_bounded_note(&pack(&repo, "HEAD"), "inventory.yaml");
}

#[test]
fn a_small_new_data_file_is_kept_whole() {
    let repo = base();
    write(repo.path(), "src/App.java", APP_EDITED);
    write(repo.path(), "small.json", &rows(5));
    let doc = pack(&repo, "HEAD");
    let text: String = fragments_of(&doc, "small.json")
        .iter()
        .map(|f| f["content"].as_str().unwrap())
        .collect();
    assert!(text.contains("item-4"), "{doc:#}");
    assert!(!text.contains("more lines]"), "{doc:#}");
}

#[test]
fn an_edited_value_of_a_large_tracked_json_stays_the_one_changed_fragment() {
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    write(repo.path(), "src/App.java", APP);
    write(repo.path(), "data.json", &rows(5_000));
    commit_all(repo.path(), "initial");
    write(
        repo.path(),
        "data.json",
        &rows(5_000).replace("item-7\"", "item-seven\""),
    );
    let doc = pack(&repo, "HEAD");
    let parts = fragments_of(&doc, "data.json");
    assert!(
        parts
            .iter()
            .any(|f| f["content"].as_str().unwrap().contains("item-seven")),
        "{doc:#}"
    );
    assert!(parts.len() < 10, "{} fragments: {doc:#}", parts.len());
}
