// The crate graph is a contract (fj#197): a crate edge is a layer boundary the
// compiler enforces, so a new one is the owner's word, not a convenience.
// `cargo metadata` names the crates of the workspace and what each depends on;
// the edges between them are exactly the ones listed here, both ways.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

/// `(dependent, dependency)` by package name. The base depends on no workspace crate.
const EDGES: &[(&str, &str)] = &[
    ("diffctx-answer", "diffctx-select"),
    ("diffctx-answer", "diffctx-graph"),
    ("diffctx-answer", "diffctx-occurrence"),
    ("diffctx-answer", "diffctx-resolve"),
    ("diffctx-answer", "diffctx-facts"),
    ("diffctx-answer", "diffctx-snapshot"),
    ("diffctx-answer", "diffctx-config"),
    ("diffctx-answer", "diffctx-base"),
    ("diffctx-select", "diffctx-graph"),
    ("diffctx-select", "diffctx-snapshot"),
    ("diffctx-select", "diffctx-config"),
    ("diffctx-select", "diffctx-base"),
    ("diffctx-graph", "diffctx-facts"),
    ("diffctx-graph", "diffctx-snapshot"),
    ("diffctx-graph", "diffctx-config"),
    ("diffctx-graph", "diffctx-base"),
    ("diffctx", "diffctx-answer"),
    ("diffctx", "diffctx-select"),
    ("diffctx", "diffctx-graph"),
    ("diffctx-occurrence", "diffctx-resolve"),
    ("diffctx-occurrence", "diffctx-facts"),
    ("diffctx-occurrence", "diffctx-snapshot"),
    ("diffctx-occurrence", "diffctx-base"),
    ("diffctx-resolve", "diffctx-facts"),
    ("diffctx-resolve", "diffctx-snapshot"),
    ("diffctx-resolve", "diffctx-config"),
    ("diffctx-resolve", "diffctx-base"),
    ("diffctx", "diffctx-occurrence"),
    ("diffctx", "diffctx-resolve"),
    ("diffctx-facts", "diffctx-snapshot"),
    ("diffctx-facts", "diffctx-config"),
    ("diffctx-facts", "diffctx-base"),
    ("diffctx", "diffctx-facts"),
    ("diffctx-config", "diffctx-base"),
    ("diffctx-snapshot", "diffctx-base"),
    ("diffctx-snapshot", "diffctx-config"),
    ("diffctx", "diffctx-snapshot"),
    ("diffctx", "diffctx-config"),
    ("diffctx", "diffctx-base"),
];

fn workspace_edges() -> BTreeSet<(String, String)> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("run cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let metadata: Value = serde_json::from_slice(&out.stdout).expect("metadata is JSON");
    let packages = metadata["packages"].as_array().expect("packages");
    let members: BTreeSet<&str> = packages
        .iter()
        .map(|p| p["name"].as_str().expect("package name"))
        .collect();
    packages
        .iter()
        .flat_map(|package| {
            let from = package["name"].as_str().expect("package name");
            package["dependencies"]
                .as_array()
                .expect("dependencies")
                .iter()
                .filter_map(|d| d["name"].as_str())
                .filter(|to| members.contains(to))
                .map(move |to| (from.to_string(), to.to_string()))
        })
        .collect()
}

#[test]
fn the_workspace_crates_depend_on_each_other_only_as_listed() {
    let found = workspace_edges();
    let listed: BTreeSet<(String, String)> = EDGES
        .iter()
        .map(|(from, to)| ((*from).to_string(), (*to).to_string()))
        .collect();
    let mut problems = Vec::new();
    for (from, to) in found.difference(&listed) {
        problems.push(format!(
            "new crate edge {from} -> {to}: depend downward, or the owner's word lists it in EDGES"
        ));
    }
    for (from, to) in listed.difference(&found) {
        problems.push(format!(
            "crate edge {from} -> {to} is gone: delete it from EDGES"
        ));
    }
    assert!(
        problems.is_empty(),
        "the crate graph moved:\n  {}",
        problems.join("\n  ")
    );
}
