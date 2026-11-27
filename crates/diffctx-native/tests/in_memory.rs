use std::sync::Arc;

use diffctx_base::types::DiffHunk;
use diffctx_config::mode::ScoringMode;
use rustc_hash::FxHashMap;

mod support;
use support::memory::{MemoryRepo, build_diff_context_in_memory, diff_to_hunks};

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

/// The same deletion through both parsers: the harness's in-memory differ
/// and git's own `--unified=0` output through the production parser.
///
/// The sibling test below compares against a hand-written `DiffHunk`, which
/// cannot notice `git.rs` drifting away from it — the harness would go on
/// scoring a system nobody ships and the literal would still be green
/// (#245). This one has no literal: git produces the bytes.
#[test]
fn both_pipelines_anchor_a_pure_deletion_identically() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let old_file = tmp.path().join("old.py");
    let new_file = tmp.path().join("new.py");
    let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
    let new = "l1\nl2\nl3\nl4\nl6\nl7\n";
    std::fs::write(&old_file, old).expect("write old");
    std::fs::write(&new_file, new).expect("write new");

    // `--no-index` needs no repository and emits exactly the header and
    // hunk syntax `parse_diff` consumes in production.
    let output = diffctx_snapshot::snapshot::git_command(tmp.path())
        .args([
            "diff",
            "--no-index",
            "--unified=0",
            "-M",
            "old.py",
            "new.py",
        ])
        .output()
        .expect("git diff --no-index");
    let diff_text = String::from_utf8_lossy(&output.stdout);
    let from_git =
        diffctx_snapshot::diff_parse::parse_hunks_from_diff_output(&diff_text, tmp.path());
    assert_eq!(from_git.len(), 1, "git reported {diff_text:?}");

    let path: Arc<str> = Arc::from("f.py");
    let from_harness = diff_to_hunks(&path, old, new);
    assert_eq!(from_harness.len(), 1);

    assert_eq!(
        (
            from_harness[0].new_start,
            from_harness[0].new_len,
            from_harness[0].old_start,
            from_harness[0].old_len
        ),
        (
            from_git[0].new_start,
            from_git[0].new_len,
            from_git[0].old_start,
            from_git[0].old_len
        ),
        "the harness anchors a pure deletion where git does not"
    );
    assert_eq!(
        from_harness[0].core_selection_range(),
        from_git[0].core_selection_range(),
        "same hunk, different core window"
    );
}

/// The harness and production must anchor a pure deletion on the same
/// line. git's `--unified=0` reports the deletion of old line 5 as
/// `@@ -5 +4,0 @@` — new-side start is the line BEFORE the gap — and
/// `core_selection_range` anchors on that value; the in-memory path used
/// to start one line later, so the corpus scored a selection nobody ships.
#[test]
fn a_pure_deletion_anchors_where_git_anchors_it() {
    let path: Arc<str> = Arc::from("f.py");
    let old = "l1\nl2\nl3\nl4\nl5\nl6\nl7\n";
    let new = "l1\nl2\nl3\nl4\nl6\nl7\n";
    let hunks = diff_to_hunks(&path, old, new);
    assert_eq!(hunks.len(), 1);
    let from_git = DiffHunk {
        path: path.clone(),
        new_start: 4,
        new_len: 0,
        old_start: 5,
        old_len: 1,
    };
    assert_eq!(
        (
            hunks[0].new_start,
            hunks[0].new_len,
            hunks[0].old_start,
            hunks[0].old_len
        ),
        (
            from_git.new_start,
            from_git.new_len,
            from_git.old_start,
            from_git.old_len
        )
    );
    assert_eq!(
        hunks[0].core_selection_range(),
        from_git.core_selection_range()
    );

    // A deletion at the very top: git says `@@ -1 +0,0 @@`, and both
    // sides clamp the anchor to line 1.
    let hunks = diff_to_hunks(&path, "a\nb\n", "b\n");
    assert_eq!((hunks[0].new_start, hunks[0].new_len), (0, 0));
    assert_eq!(hunks[0].core_selection_range(), (1, 1));

    // A replacement keeps its start: no off-by-one the other way.
    let hunks = diff_to_hunks(&path, "a\nb\nc\n", "a\nB\nc\n");
    assert_eq!((hunks[0].new_start, hunks[0].new_len), (2, 1));
}
