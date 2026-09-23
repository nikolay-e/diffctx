use std::collections::BTreeMap;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;

use _diffctx::in_memory_harness::fragment_rows;
use serde::Deserialize;
use walkdir::WalkDir;

#[derive(Deserialize, Default)]
struct Case {
    #[serde(default)]
    repo: Repo,
}

#[derive(Deserialize, Default)]
struct Repo {
    #[serde(default)]
    initial_files: BTreeMap<String, String>,
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repository root")
        .to_path_buf()
}

fn snapshot_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fragment_snapshot.txt")
}

// FNV-1a: std's hashers are not stable across Rust releases, and the gate
// file must mean the same thing on every toolchain and OS.
fn fnv1a(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn rows_of(path: &str, content: &str) -> String {
    let mut out = String::new();
    for r in fragment_rows(path, content) {
        let _ = writeln!(
            out,
            "{}-{} {} {}",
            r.start_line,
            r.end_line,
            r.kind,
            r.symbol.as_deref().unwrap_or("-")
        );
    }
    out
}

fn current_snapshot() -> BTreeMap<String, String> {
    let cases_dir = repo_root().join("tests").join("cases").join("diff");
    let mut snapshot = BTreeMap::new();
    for entry in WalkDir::new(&cases_dir).into_iter().filter_map(Result::ok) {
        let path = entry.path();
        if !path.extension().is_some_and(|e| e == "yaml" || e == "yml") {
            continue;
        }
        let case_name = path
            .strip_prefix(&cases_dir)
            .unwrap()
            .with_extension("")
            .components()
            .filter_map(|c| c.as_os_str().to_str())
            .collect::<Vec<_>>()
            .join("/");
        let raw = fs::read_to_string(path).unwrap();
        let case: Case = serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("{case_name}: {e}"));
        let rows: String = case
            .repo
            .initial_files
            .iter()
            .map(|(file, content)| format!("{file}\n{}", rows_of(file, content)))
            .collect();
        snapshot.insert(case_name, format!("{:016x}", fnv1a(&rows)));
    }
    snapshot
}

fn parse_snapshot(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (key, hash) = l.rsplit_once('\t')?;
            Some((key.to_string(), hash.to_string()))
        })
        .collect()
}

const HEADER: &str = "# case -> FNV-1a of `path` + fragment rows (`start-end kind symbol`) for \
each of its\n# initial_files, through fragment_file. One hash per case, not per file: \
per-file rows\n# exceed the repository's 1000 KB file limit. Regenerate with\n# DIFFCTX_UPDATE_FRAGMENT_SNAPSHOT=1 cargo test --test fragment_snapshot\n";

/// The corpus oracle matches anchors, which still hit inside gap chunks when a
/// grammar stops extracting definitions, so parser collapse moved it by a
/// dozen cases out of thousands. This gate pins what the fragmenter emits for
/// every file the corpus seeds, in both directions: a change here is a
/// Q-class change and must be regenerated deliberately.
#[test]
fn every_corpus_file_fragments_exactly_as_recorded() {
    let current = current_snapshot();
    if std::env::var("DIFFCTX_UPDATE_FRAGMENT_SNAPSHOT").is_ok() {
        let mut out = String::from(HEADER);
        for (key, hash) in &current {
            let _ = writeln!(out, "{key}\t{hash}");
        }
        fs::write(snapshot_path(), out).unwrap();
        return;
    }
    let recorded = parse_snapshot(&fs::read_to_string(snapshot_path()).unwrap_or_default());
    let changed: Vec<&String> = current
        .iter()
        .filter(|(k, v)| recorded.get(*k).is_some_and(|r| r != *v))
        .map(|(k, _)| k)
        .collect();
    let added: Vec<&String> = current
        .keys()
        .filter(|k| !recorded.contains_key(*k))
        .collect();
    let removed: Vec<&String> = recorded
        .keys()
        .filter(|k| !current.contains_key(*k))
        .collect();
    if changed.is_empty() && added.is_empty() && removed.is_empty() {
        return;
    }
    let mut msg = format!(
        "fragment snapshot drifted: {} changed, {} added, {} removed (of {} recorded)\n",
        changed.len(),
        added.len(),
        removed.len(),
        recorded.len()
    );
    for key in changed
        .iter()
        .chain(added.iter())
        .chain(removed.iter())
        .take(10)
    {
        let _ = writeln!(msg, "  {key}");
    }
    msg.push_str(
        "If the change is intended, regenerate with DIFFCTX_UPDATE_FRAGMENT_SNAPSHOT=1 \
         and commit the file.\n",
    );
    panic!("{msg}");
}
