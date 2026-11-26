//! A definition whose callers cannot all be named says why: a value handed to a callee
//! names the callee, a call the language makes without the name written says so.
//! Run on real git repositories through the binary.

mod support;
use support::{diffctx_unclocked, repo};

const HANDLERS: &str = "def on_event(e):\n    return e\n";
const HANDLERS_CHANGED: &str = "def on_event(e):\n    return e + 1\n";
const BUS: &str =
    "from app.handlers import on_event\n\n\ndef setup(bus):\n    bus.register(on_event)\n";
const APP: &str = "package app\n\nvar ready bool\n\nfunc init() {\n\tready = false\n}\n";
const APP_CHANGED: &str = "package app\n\nvar ready bool\n\nfunc init() {\n\tready = true\n}\n";
const USE: &str = "package app\n\nfunc Ready() bool {\n\treturn ready\n}\n";

fn impact(repo: &std::path::Path, format: &str) -> std::process::Output {
    impact_with(repo, format, &[])
}

fn impact_with(repo: &std::path::Path, format: &str, env: &[(&str, &str)]) -> std::process::Output {
    let out = diffctx_unclocked()
        .envs(env.iter().copied())
        .current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            format,
        ])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn gaps_of(repo: &std::path::Path, symbol: &str) -> Vec<serde_json::Value> {
    gaps_under(repo, symbol, &[])
}

fn gaps_under(
    repo: &std::path::Path,
    symbol: &str,
    env: &[(&str, &str)],
) -> Vec<serde_json::Value> {
    let doc: serde_json::Value =
        serde_json::from_slice(&impact_with(repo, "json", env).stdout).expect("impact JSON");
    let definition = doc["definitions"]
        .as_array()
        .and_then(|all| all.iter().find(|d| d["symbol"] == symbol))
        .unwrap_or_else(|| panic!("no definition {symbol}: {doc:#}"));
    definition["gaps"].as_array().cloned().unwrap_or_default()
}

#[test]
fn a_callback_handed_to_a_callee_names_the_callee() {
    let tmp = repo(
        &[
            ("app/__init__.py", ""),
            ("app/handlers.py", HANDLERS),
            ("app/bus.py", BUS),
        ],
        &[("app/handlers.py", HANDLERS_CHANGED)],
    );
    let gaps = gaps_of(tmp.path(), "on_event");
    assert_eq!(
        gaps,
        [serde_json::json!({"kind": "passed_as_value", "to": ["bus.register"]})]
    );
    let text = String::from_utf8(impact(tmp.path(), "md").stdout).expect("utf8");
    assert!(
        text.contains("passed as a value to `bus.register`"),
        "{text}"
    );
}

#[test]
fn an_initializer_a_language_calls_says_it_is_constructed_implicitly() {
    let tmp = repo(
        &[("app.go", APP), ("use.go", USE)],
        &[("app.go", APP_CHANGED)],
    );
    let gaps = gaps_of(tmp.path(), "init");
    assert_eq!(
        gaps,
        [serde_json::json!({"kind": "implicitly_constructed"})]
    );
    let text = String::from_utf8(impact(tmp.path(), "md").stdout).expect("utf8");
    assert!(text.contains("constructed implicitly"), "{text}");
}

const TOOLTIPS: &str =
    "export function renderTip(text: string): string {\n  return text.slice(0, 20);\n}\n";
const TOOLTIPS_CHANGED: &str =
    "export function renderTip(text: string): string {\n  return text.slice(0, 40);\n}\n";
const PAGE: &str = "import { renderTip } from './tooltips';\nimport { Tip } from './Tip';\n\nexport function Page() {\n  return (\n    <section>\n      <Tip content={renderTip} />\n    </section>\n  );\n}\n";

#[test]
fn a_value_handed_to_a_component_in_a_returned_group_names_no_return() {
    let tmp = repo(
        &[("src/tooltips.ts", TOOLTIPS), ("src/Page.tsx", PAGE)],
        &[("src/tooltips.ts", TOOLTIPS_CHANGED)],
    );
    let gaps = gaps_of(tmp.path(), "renderTip");
    assert_eq!(gaps, [serde_json::json!({"kind": "passed_as_value"})]);
    let text = String::from_utf8(impact(tmp.path(), "md").stdout).expect("utf8");
    assert!(
        text.contains("passed as a value to a callee the text does not name"),
        "{text}"
    );
    assert!(!text.contains("`return`"), "{text}");
}

#[test]
fn uses_no_binding_settles_are_counted_with_the_ones_beside_a_resolved_call() {
    let tmp = repo(
        &[
            ("lib.py", "def f(a):\n    return a\n"),
            (
                "app.py",
                "from lib import f\n\n\ndef main(y):\n    f(1); y.f(2)\n    y.f(3)\n",
            ),
        ],
        &[("lib.py", "def f(a):\n    return a + 1\n")],
    );
    assert_eq!(
        gaps_of(tmp.path(), "f"),
        [serde_json::json!({
            "kind": "unlanded", "name": "f", "uses": 2,
            "sites": [{"path": "app.py", "line": 6, "via": "call"}]
        })]
    );
}

#[test]
fn files_that_did_not_parse_are_one_gap_counting_them() {
    let go_mod = "module example.com/p\n\ngo 1.22\n";
    let tmp = repo(
        &[
            ("go.mod", go_mod),
            ("p.go", "package p\n\nfunc F() int { return 1 }\n"),
            (
                "q.go",
                "package p\n\nfunc G() int {\n    return F()\n}\n\nfunc Broken( {\n",
            ),
            (
                "r.go",
                "package p\n\nfunc H() int {\n    return F()\n}\n\nfunc Broken2( {\n",
            ),
        ],
        &[("p.go", "package p\n\nfunc F() int { return 2 }\n")],
    );
    assert_eq!(
        gaps_of(tmp.path(), "F"),
        [serde_json::json!({"kind": "unaccounted", "reason": "unparsed", "files": 2})]
    );
}

const LIB: &str = "def f(a):\n    return a\n";
const LIB_CHANGED: &str = "def f(a):\n    return a + 1\n";
const CALLER: &str = "from lib import f\n\n\ndef main():\n    return f(1)\n";

#[test]
fn a_file_the_run_could_not_read_stops_the_search_for_that_reason() {
    let tmp = repo(
        &[("lib.py", LIB), ("app.py", CALLER)],
        &[("lib.py", LIB_CHANGED)],
    );
    assert_eq!(
        gaps_under(tmp.path(), "f", &[("DIFFCTX_TEST_LOSS", "read_failed:1")]),
        [
            serde_json::json!({"kind": "unaccounted", "reason": "read_failed", "files": 1}),
            serde_json::json!({"kind": "stopped", "reasons": ["read_failed"]}),
        ]
    );
}

#[test]
fn a_deadline_stops_the_search_for_that_reason() {
    let tmp = repo(
        &[("lib.py", LIB), ("app.py", CALLER)],
        &[("lib.py", LIB_CHANGED)],
    );
    assert_eq!(
        gaps_under(tmp.path(), "f", &[("DIFFCTX_TEST_LOSS", "deadline:1")]),
        [serde_json::json!({"kind": "stopped", "reasons": ["deadline"]})]
    );
}

#[test]
fn changed_statements_no_definition_holds_are_named_as_runs_of_lines() {
    let tmp = repo(
        &[(
            "cfg.py",
            "def f():\n    return 1\n\n\nregister(f)\nregister(f, 2)\n\nsetup(1)\n",
        )],
        &[(
            "cfg.py",
            "def f():\n    return 1\n\n\nregister(f, 3)\nregister(f, 4)\n\nsetup(2)\n",
        )],
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&impact(tmp.path(), "json").stdout).expect("impact JSON");
    assert_eq!(
        doc["coverage"]["unanalysed"]["items"],
        serde_json::json!([{
            "path": "cfg.py", "language": "python", "reason": "outside_definitions",
            "lines": [5, 6, 8, 8]
        }])
    );
}
