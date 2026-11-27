//! A definition whose callers cannot all be named says why: a value handed to a callee
//! names the callee, a call the language makes without the name written says so.
//! Run on real git repositories through the binary.

mod support;
use support::{commit_all, diffctx_unclocked, init_repo, repo, write};
use tempfile::TempDir;

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
    impact_in(repo, None, format, env)
}

fn impact_in(
    repo: &std::path::Path,
    subtree: Option<&str>,
    format: &str,
    env: &[(&str, &str)],
) -> std::process::Output {
    let out = diffctx_unclocked()
        .envs(env.iter().copied())
        .current_dir(repo)
        .args(subtree)
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

fn limits_under_loss(repo: &std::path::Path) -> serde_json::Value {
    let doc: serde_json::Value = serde_json::from_slice(
        &impact_with(repo, "json", &[("DIFFCTX_TEST_LOSS", "read_failed:1")]).stdout,
    )
    .expect("impact JSON");
    doc["coverage"]["limits"].clone()
}

const TWO_PYTHON: [(&str, &str); 2] = [
    ("a.py", "def a():\n    return 1\n"),
    ("b.py", "def b():\n    return 2\n"),
];

#[test]
fn a_family_no_changed_file_belongs_to_is_not_read_so_it_cannot_lose_a_file() {
    let rust = ("src/lib.rs", "pub fn f(a: i64) -> i64 {\n    a\n}\n");
    let rust_changed = ("src/lib.rs", "pub fn f(a: i64) -> i64 {\n    a + 1\n}\n");
    let mut base = TWO_PYTHON.to_vec();
    base.push(rust);
    let tmp = repo(&base, &[rust_changed]);
    assert_eq!(limits_under_loss(tmp.path()), serde_json::Value::Null);

    let note = ("notes.xyz", "#!/usr/bin/env python3\nprint(1)\n");
    let mut base = TWO_PYTHON.to_vec();
    base.push(note);
    let tmp = repo(
        &base,
        &[("notes.xyz", "#!/usr/bin/env python3\nprint(2)\n")],
    );
    assert_eq!(
        limits_under_loss(tmp.path()),
        serde_json::Value::Null,
        "a shebang counts only in a file without an extension"
    );

    let script = ("bin/run", "echo 1\n");
    let mut base = TWO_PYTHON.to_vec();
    base.push(script);
    let tmp = repo(&base, &[("bin/run", "echo 2\n")]);
    assert_eq!(
        limits_under_loss(tmp.path()),
        serde_json::Value::Null,
        "a file without an extension and without a shebang is of no family"
    );
}

#[test]
fn a_run_asked_about_a_subtree_names_the_files_outside_it_and_reads_none() {
    let tmp = repo(
        &[
            ("pkg/lib.py", "def f(a):\n    return a\n"),
            ("other/app.py", "def go(o):\n    return o.f(1)\n"),
        ],
        &[("pkg/lib.py", "def f(a):\n    return a + 1\n")],
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&impact_in(tmp.path(), Some("pkg"), "json", &[]).stdout)
            .expect("impact JSON");
    let gaps = doc["definitions"][0]["gaps"].clone();
    assert_eq!(
        gaps,
        serde_json::json!([{"kind": "outside_scope", "files": 1}]),
        "{doc:#}"
    );
}

#[test]
fn a_run_asked_about_a_subtree_counts_a_file_outside_it_only_when_it_spells_every_word_of_the_name()
{
    let tmp = repo(
        &[
            ("other/a.yaml", "labels: 1\n"),
            ("other/b.yaml", "ref: pod-labels\n"),
            ("other/c.yaml", "pod: 1\n"),
            ("pkg/values.yaml", "pod-labels: a\n"),
        ],
        &[("pkg/values.yaml", "pod-labels: b\n")],
    );
    let doc: serde_json::Value =
        serde_json::from_slice(&impact_in(tmp.path(), Some("pkg"), "json", &[]).stdout)
            .expect("impact JSON");
    let outside: Vec<&serde_json::Value> = doc["definitions"][0]["gaps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|gap| gap["kind"] == "outside_scope")
        .collect();
    assert_eq!(
        outside,
        [&serde_json::json!({"kind": "outside_scope", "files": 1})],
        "{doc:#}"
    );
}

fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// The token store is a cache, never a source: a set cut short on disk is read again and not taken
/// for a set with fewer tokens, whose file outside the subtree would then go uncounted.
#[test]
fn a_token_set_cut_short_on_disk_is_read_again_and_the_outside_files_are_still_counted() {
    let tmp = repo(
        &[
            ("other/a.yaml", "labels: 1\n"),
            ("other/b.yaml", "ref: pod-labels\n"),
            ("pkg/values.yaml", "pod-labels: a\n"),
        ],
        &[("pkg/values.yaml", "pod-labels: b\n")],
    );
    let corrupted = |change: &dyn Fn(&str) -> Option<String>, why: &str| {
        let cache = TempDir::new().expect("tempdir");
        let dir = cache.path().to_str().expect("utf-8 path");
        let env = [("DIFFCTX_CACHE_DIR", dir)];
        let run = || impact_in(tmp.path(), Some("pkg"), "json", &env).stdout;
        let warm = run();
        assert!(
            String::from_utf8_lossy(&warm).contains("\"outside_scope\""),
            "the file outside the subtree is counted"
        );
        let stored = files_under(&cache.path().join("diffctx").join("facts").join("tokens"));
        assert!(!stored.is_empty(), "the run stored token sets");
        for path in &stored {
            let text = std::fs::read_to_string(path).expect("a stored set");
            if let Some(cut) = change(&text) {
                std::fs::write(path, cut).expect("write");
            }
        }
        assert_eq!(
            String::from_utf8_lossy(&run()),
            String::from_utf8_lossy(&warm),
            "{why}"
        );
    };
    corrupted(
        &|text| (!text.starts_with("0\n")).then(|| text[..text.len() - 2].to_string()),
        "last token cut mid-word",
    );
    corrupted(
        &|text| (!text.starts_with("0\n")).then(|| text[..text.len() - 1].to_string()),
        "closing newline missing",
    );
    for dropped in 0..3 {
        corrupted(
            &|text| {
                let (count, body) = text.split_once('\n')?;
                let mut tokens: Vec<&str> = body.lines().collect();
                (dropped < tokens.len() && tokens.len() > 1).then(|| {
                    tokens.remove(dropped);
                    format!("{count}\n{}\n", tokens.join("\n"))
                })
            },
            &format!("token line {dropped} missing"),
        );
    }
}

const OTHER_FILE_CALLS_INNER: (&str, &str) = ("app.py", "def go(o):\n    return o.inner(1)\n");

fn sites_of_unlanded(gaps: &[serde_json::Value]) -> Vec<String> {
    gaps.iter()
        .filter(|gap| gap["kind"] == "unlanded")
        .flat_map(|gap| gap["sites"].as_array().into_iter().flatten())
        .map(|site| format!("{}:{}", site["path"].as_str().unwrap_or(""), site["line"]))
        .collect()
}

/// A function nested in another exists only while the outer one runs: the `o.inner(1)`
/// of another file is a call of some other `inner`, and a top-level `inner` is where
/// that same call is a use the binding model cannot place.
#[test]
fn a_nested_function_lists_only_the_uses_of_its_own_file_as_unlanded() {
    let nested = repo(
        &[
            (
                "lib.py",
                "def make(prefix):\n    def inner(m):\n        return prefix + m\n\n    return inner\n",
            ),
            OTHER_FILE_CALLS_INNER,
        ],
        &[(
            "lib.py",
            "def make(prefix):\n    def inner(m):\n        return prefix + m.strip()\n\n    return inner\n",
        )],
    );
    assert_eq!(
        sites_of_unlanded(&gaps_of(nested.path(), "inner")),
        ["lib.py:5"]
    );
    let top_level = repo(
        &[
            ("lib.py", "def inner(m):\n    return m\n"),
            OTHER_FILE_CALLS_INNER,
        ],
        &[("lib.py", "def inner(m):\n    return m.strip()\n")],
    );
    assert_eq!(
        sites_of_unlanded(&gaps_of(top_level.path(), "inner")),
        ["app.py:2"]
    );
}

/// A module a deleted sibling imported has no importer left to name: the deleted
/// files are not the files that still depend on it.
#[test]
fn a_deleted_module_only_a_deleted_module_imported_has_no_importers() {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path();
    init_repo(dir);
    write(dir, "pkg/__init__.py", "");
    write(
        dir,
        "pkg/first.py",
        "class First:\n    title = \"\"\n\n    def run(self):\n        return 1\n",
    );
    write(
        dir,
        "pkg/second.py",
        "from pkg.first import First\n\n\nclass Second(First):\n    def go(self):\n        return self.run()\n",
    );
    write(dir, "other.py", "def keep():\n    return 1\n");
    commit_all(dir, "initial");
    std::fs::remove_dir_all(dir.join("pkg")).expect("delete the package");
    commit_all(dir, "drop the package");
    let doc: serde_json::Value =
        serde_json::from_slice(&impact(dir, "json").stdout).expect("impact JSON");
    let items = doc["deleted_modules"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no deleted modules: {doc:#}"));
    assert_eq!(items.len(), 2, "{doc:#}");
    assert!(
        items.iter().all(|module| module["importers"].is_null()),
        "{doc:#}"
    );
    let text = String::from_utf8(impact(dir, "md").stdout).expect("utf8");
    assert_eq!(text.matches("no importers").count(), 2, "{text}");
    assert!(!text.contains("imported by"), "{text}");
}

const GADGET: &str = "class Gadget:\n    def run(self):\n        return 1\n\n    def stop(self):\n        return 0\n";
const GADGET_WITHOUT_RUN: &str = "class Gadget:\n    def stop(self):\n        return 0\n";

/// A removed method's callers are read in the base to know that `gadget.run()` was a call
/// of `Gadget.run`. A base read that fails is a loss the row names twice: the file whose
/// read failed, and the stop of the base the row's uses were accounted against. Each base
/// read after the run's own is struck in turn; the cache is off so that every blob is read.
#[test]
fn a_removed_method_names_a_base_read_that_failed_on_its_own_row() {
    let callers: Vec<(String, String)> = (1..=3)
        .map(|k| {
            (
                format!("caller{k}.py"),
                format!(
                    "from gadget import Gadget\n\n\ndef drive{k}():\n    gadget = Gadget()\n    return gadget.run()\n"
                ),
            )
        })
        .collect();
    let mut base: Vec<(&str, &str)> = vec![("gadget.py", GADGET)];
    base.extend(
        callers
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    );
    let tmp = repo(&base, &[("gadget.py", GADGET_WITHOUT_RUN)]);
    let scratch = TempDir::new().expect("tempdir");
    let no_cache = scratch.path().join("no-cache");
    std::fs::write(&no_cache, "").expect("write");
    let no_cache = no_cache.to_str().expect("utf-8 path");
    let (mut stop_named, mut file_named) = (false, false);
    for n in 0..200 {
        let strike = format!("base:{n}");
        let out = diffctx_unclocked()
            .envs([
                ("DIFFCTX_CACHE_DIR", no_cache),
                ("DIFFCTX_TEST_READ_FAULT", strike.as_str()),
            ])
            .current_dir(tmp.path())
            .args([
                "--diff",
                "HEAD~1..HEAD",
                "--mode",
                "impact",
                "-q",
                "-f",
                "json",
            ])
            .output()
            .expect("run diffctx");
        if !String::from_utf8_lossy(&out.stderr).contains("failed on purpose") {
            break;
        }
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
            continue;
        };
        let gaps: Vec<&serde_json::Value> = doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| d["change"]["kind"] == "removed" && d["symbol"] == "run")
            .flat_map(|d| d["gaps"].as_array().into_iter().flatten())
            .collect();
        stop_named |= gaps
            .iter()
            .any(|g| g["kind"] == "stopped" && g["reasons"] == serde_json::json!(["read_failed"]));
        file_named |= gaps
            .iter()
            .any(|g| g["kind"] == "unaccounted" && g["reason"] == "read_failed");
    }
    assert!(
        stop_named,
        "no struck base read left a stop on the row of `run`"
    );
    assert!(
        file_named,
        "no struck base read left a lost file on the row of `run`"
    );
}
