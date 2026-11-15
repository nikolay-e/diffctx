// A changed lock file is listed and counted whatever moved in it, and the
// answer claims no dependency bump it did not check (#495, #593). These run
// the binary on a real git repository, the way the hook does.

mod support;
use support::{diffctx_unclocked, repo};

const LOCK: &str = r#"version = 1

[[package]]
name = "shared"
version = "0"
source = { editable = "../../shared" }
"#;

#[test]
fn a_lock_whose_only_change_is_a_local_path_is_listed_without_a_claimed_bump() {
    let code = ("app.py", "def f():\n    return 1\n");
    let tmp = repo(
        &[code, ("uv.lock", LOCK)],
        &[("uv.lock", &LOCK.replace("../../shared", "../shared"))],
    );
    let run = |format: &[&str]| {
        let out = diffctx_unclocked()
            .current_dir(tmp.path())
            .args(["--diff", "HEAD~1..HEAD", "--mode", "impact"])
            .args(format)
            .output()
            .expect("run diffctx");
        assert!(
            out.status.success(),
            "impact failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf-8 answer")
    };
    let doc: serde_json::Value = serde_json::from_str(&run(&["-q"])).expect("impact JSON");
    assert_eq!(
        doc["inventory"]["lockfiles"]["items"],
        serde_json::json!(["uv.lock"])
    );
    let text = run(&["-f", "md"]);
    assert!(!text.contains("No changes to analyse"), "{text}");
    assert!(!text.contains("versions moved"), "{text}");
    assert!(text.contains("uv.lock"), "{text}");
}
