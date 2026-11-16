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
    let out = diffctx_unclocked()
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
    let doc: serde_json::Value =
        serde_json::from_slice(&impact(repo, "json").stdout).expect("impact JSON");
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
