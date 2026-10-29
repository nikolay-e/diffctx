// Forgejo #101: a ConfigMap holds a Grafana dashboard as JSON in a `|` block
// scalar. The whole scalar is one definition far larger than a small budget;
// the lines that changed inside it must be what a fitted pack and a locate
// list show, never the head of the scalar, and never behind unrelated rules.

use std::path::Path;

mod support;
use support::{diffctx, repo};

const PANELS: usize = 40;
const CHANGED: [usize; 3] = [3, 20, 37];

fn dashboard(expr: impl Fn(usize) -> String) -> String {
    let panels: Vec<String> = (0..PANELS)
        .map(|i| {
            format!(
                "    {{\n      \"id\": {i},\n      \"title\": \"Panel {i}\",\n      \"type\": \"timeseries\",\n      \"targets\": [\n        {{\n          \"expr\": \"{}\",\n          \"refId\": \"A\"\n        }}\n      ],\n      \"gridPos\": {{\n        \"h\": 8,\n        \"w\": 12,\n        \"x\": 0,\n        \"y\": {}\n      }}\n    }}",
                expr(i),
                i * 8
            )
        })
        .collect();
    let body = format!(
        "{{\n  \"title\": \"Pods\",\n  \"panels\": [\n{}\n  ]\n}}",
        panels.join(",\n")
    );
    let indented: Vec<String> = body.lines().map(|l| format!("    {l}")).collect();
    format!(
        "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: grafana-dashboard-pods\ndata:\n  pods.json: |\n{}\n",
        indented.join("\n")
    )
}

fn alerts() -> String {
    let rules: String = (0..12)
        .map(|i| {
            format!(
                "  - alert: ClinicalAlert{i}\n    expr: clinical_value{{ward=\"{i}\"}} > {i}\n    for: 5m\n    labels:\n      severity: warning\n"
            )
        })
        .collect();
    format!("groups:\n- name: clinical\n  rules:\n{rules}")
}

fn changed_marker(i: usize) -> String {
    format!("kube_pod_status_phase_{i}_after")
}

fn fixture() -> tempfile::TempDir {
    let before = dashboard(|i| format!("kube_pod_status_phase_{i}"));
    let after = dashboard(|i| {
        if CHANGED.contains(&i) {
            changed_marker(i)
        } else {
            format!("kube_pod_status_phase_{i}")
        }
    });
    repo(
        &[
            ("k8s/dashboard.yaml", before.as_str()),
            ("monitoring/clinical-alerts.yaml", alerts().as_str()),
        ],
        &[("k8s/dashboard.yaml", after.as_str())],
    )
}

fn run(repo: &Path, args: &[&str]) -> serde_json::Value {
    let out = diffctx()
        .current_dir(repo)
        .args([
            ".",
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            "2000",
            "-q",
            "-f",
            "json",
        ])
        .args(args)
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON")
}

#[test]
fn a_fitted_pack_shows_the_changed_lines_inside_a_block_scalar() {
    let tmp = fixture();
    let doc = run(tmp.path(), &[]);
    let shown: String = doc["fragments"]
        .as_array()
        .expect("fragments")
        .iter()
        .filter(|f| f["path"] == "k8s/dashboard.yaml")
        .map(|f| f["content"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    for i in CHANGED {
        assert!(shown.contains(&changed_marker(i)), "{i} missing in {doc:#}");
    }
    assert!(
        !doc["fragments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "monitoring/clinical-alerts.yaml"),
        "an unrelated rule file was selected: {doc:#}"
    );
}

#[test]
fn a_fitted_locate_points_at_the_changed_lines_inside_a_block_scalar() {
    let tmp = fixture();
    let text = std::fs::read_to_string(tmp.path().join("k8s/dashboard.yaml")).unwrap();
    let after = |i: usize| {
        let marker = changed_marker(i);
        text.lines()
            .position(|l| l.contains(&marker))
            .expect("line")
            + 1
    };
    let doc = run(tmp.path(), &["--mode", "locate"]);
    let item = doc["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|i| i["path"] == "k8s/dashboard.yaml")
        .unwrap_or_else(|| panic!("dashboard not listed: {doc:#}"));
    assert_eq!(item["role"], "changed");
    assert!(item["tokens"].as_u64().unwrap() < 2000, "{doc:#}");
    let (lo, hi) = item["lines"]
        .as_str()
        .and_then(|l| l.split_once('-'))
        .map(|(a, b)| (a.parse::<usize>().unwrap(), b.parse::<usize>().unwrap()))
        .expect("range");
    for i in CHANGED {
        let line = after(i);
        assert!(
            lo <= line && line <= hi,
            "{line} outside {lo}-{hi}: {doc:#}"
        );
    }
}
