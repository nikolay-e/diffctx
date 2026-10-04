use _diffctx::in_memory_harness::{MemoryRepo, build_diff_context_in_memory};
use _diffctx::mode::ScoringMode;
use rustc_hash::FxHashMap;

#[test]
fn a_changed_file_without_fragments_is_marked_as_such() {
    let initial: FxHashMap<String, String> = [
        (
            "src/app.py".to_string(),
            "def run():\n    return 1\n".to_string(),
        ),
        ("docs/blank.txt".to_string(), "x\n".to_string()),
    ]
    .into_iter()
    .collect();
    let changed: FxHashMap<String, String> = [
        (
            "src/app.py".to_string(),
            "def run():\n    return 2\n".to_string(),
        ),
        ("docs/blank.txt".to_string(), "\n\n\n".to_string()),
    ]
    .into_iter()
    .collect();
    let repo = MemoryRepo {
        name: "fragmentless".to_string(),
        initial_files: initial,
        changed_files: changed,
    };
    let out = build_diff_context_in_memory(&repo, Some(4000), 0.6, None, false, ScoringMode::Ego);
    let doc = serde_json::to_value(&out).unwrap();
    let changes = doc["changes"].as_array().expect("changes listed");
    let entry = |path: &str| changes.iter().find(|c| c["path"] == path).cloned();
    assert_eq!(entry("docs/blank.txt").unwrap()["no_fragments"], true);
    assert!(
        entry("src/app.py")
            .unwrap()
            .get("no_fragments")
            .is_none_or(|v| v == false)
    );
}
