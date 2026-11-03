//! `--shadow` (hidden): the changed definitions of the impact path beside the
//! facts layer's, per file. The shape and the set arithmetic hold whichever
//! extractors the build registers; what the facts side names is asserted only for
//! a language the report itself says has an extractor.

mod snapshot_fixture;
mod support;

use std::collections::BTreeSet;

use _diffctx::snapshot::REPO_LOCATING_ENV;
use serde_json::Value;
use snapshot_fixture::Repo;
use support::diffctx;

const PY_BASE: &str = "\
def a(x):
    return x + 1


def c(x):
    return x - 1


class Box:
    def get(self):
        return 1
";

const PY_HEAD: &str = "\
def a(x):
    return x + 2


def d(x):
    return x * 2


class Box:
    def get(self):
        return 2
";

const TS_BASE: &str = "\
export function a(x: number): number {
  return x + 1;
}

export function c(x: number): number {
  return x - 1;
}

export class Box {
  get(): number {
    return 1;
  }
}
";

const TS_HEAD: &str = "\
export function a(x: number): number {
  return x + 2;
}

export function d(x: number): number {
  return x * 2;
}

export class Box {
  get(): number {
    return 2;
  }
}
";

fn fixture() -> (Repo, String) {
    let repo = Repo::init();
    repo.write("app.py", PY_BASE);
    repo.write("app.ts", TS_BASE);
    let base = repo.commit("base");
    repo.write("app.py", PY_HEAD);
    repo.write("app.ts", TS_HEAD);
    let head = repo.commit("head");
    (repo, format!("{base}..{head}"))
}

fn shadow(repo: &Repo, args: &[&str]) -> std::process::Output {
    let mut command = diffctx();
    for name in REPO_LOCATING_ENV {
        command.env_remove(name);
    }
    command
        .current_dir(repo.path())
        .args(args)
        .output()
        .expect("run diffctx")
}

fn report(repo: &Repo, range: &str) -> Value {
    let out = shadow(repo, &["--diff", range, "--shadow", "-q"]);
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON document")
}

fn names(row: &Value, key: &str) -> Vec<String> {
    row[key]
        .as_array()
        .unwrap_or_else(|| panic!("{key} is an array in {row}"))
        .iter()
        .map(|name| name.as_str().expect("a name").to_string())
        .collect()
}

fn set(names: Vec<String>) -> BTreeSet<String> {
    names.into_iter().collect()
}

fn row<'a>(report: &'a Value, path: &str) -> &'a Value {
    report["files"]
        .as_array()
        .expect("files is an array")
        .iter()
        .find(|row| row["path"] == path)
        .unwrap_or_else(|| panic!("no row for {path} in {report}"))
}

#[test]
fn the_report_has_its_shape_and_the_differences_are_set_differences() {
    let (repo, range) = fixture();
    let report = report(&repo, &range);

    assert_eq!(report["range"], range.as_str());
    assert!(report["extractors"].is_array());
    assert!(report["losses"].is_array());
    let files = report["files"].as_array().expect("files is an array");
    assert!(!files.is_empty());

    let mut totals = [0usize; 6];
    for row in files {
        let (old, facts) = (set(names(row, "old")), set(names(row, "facts")));
        assert_eq!(
            set(names(row, "only_old")),
            old.difference(&facts).cloned().collect::<BTreeSet<_>>(),
            "only_old of {row}"
        );
        assert_eq!(
            set(names(row, "only_facts")),
            facts.difference(&old).cloned().collect::<BTreeSet<_>>(),
            "only_facts of {row}"
        );
        assert!(row["path"].is_string());
        assert!(row["facts_losses"].is_array());
        let path = row["path"].as_str().unwrap();
        for name in old.iter().chain(&facts) {
            assert!(
                name.starts_with(&format!("{path}::")),
                "{name} is not in {path}"
            );
        }
        totals[0] += 1;
        totals[1] += old.len();
        totals[2] += facts.len();
        totals[3] += names(row, "only_old").len();
        totals[4] += names(row, "only_facts").len();
        totals[5] += row["facts_losses"].as_array().unwrap().len();
    }
    let reported = &report["totals"];
    for (key, count) in [
        "files",
        "old",
        "facts",
        "only_old",
        "only_facts",
        "facts_losses",
    ]
    .into_iter()
    .zip(totals)
    {
        assert_eq!(reported[key], count, "totals.{key}");
    }
}

#[test]
fn a_language_with_an_extractor_names_what_the_facts_say_and_one_without_says_so() {
    let (repo, range) = fixture();
    let report = report(&repo, &range);
    let registered: Vec<&str> = report["extractors"]
        .as_array()
        .expect("extractors is an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();

    for (language, path) in [("python", "app.py"), ("typescript", "app.ts")] {
        let row = row(&report, path);
        let old = names(row, "old");
        for name in ["Box.get", "a"] {
            let name = format!("{path}::{name}");
            assert!(old.contains(&name), "old of {path} lacks {name}: {row}");
        }
        if registered.contains(&language) {
            let expected: Vec<String> = ["Box.get", "a", "c", "d"]
                .iter()
                .map(|name| format!("{path}::{name}"))
                .collect();
            assert_eq!(names(row, "facts"), expected, "facts of {path}");
            assert_eq!(row["facts_detail"][format!("{path}::c")], "removed");
            assert_eq!(row["facts_detail"][format!("{path}::d")], "added");
            assert!(
                row["facts_losses"].as_array().unwrap().is_empty(),
                "losses of {path}: {row}"
            );
        } else {
            assert!(names(row, "facts").is_empty(), "facts of {path}: {row}");
            let kinds: Vec<&str> = row["facts_losses"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|loss| loss["kind"].as_str())
                .collect();
            assert!(
                kinds.contains(&"unsupported"),
                "losses of {path}: {kinds:?}"
            );
        }
    }
}

#[test]
fn the_flag_needs_a_diff_and_stays_out_of_the_help() {
    let (repo, _) = fixture();
    let out = shadow(&repo, &["--shadow"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());

    let help = shadow(&repo, &["--help"]);
    assert!(!String::from_utf8_lossy(&help.stdout).contains("shadow"));
}
