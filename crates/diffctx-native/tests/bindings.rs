// Callers resolved through import bindings (#343, #344, #349, #353) and the
// honest limits of name matching (ambiguous names, trait dispatch). Each case
// pairs a reference that must resolve with a same-spelled one that must not,
// so neither precision nor recall can be bought with the other.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write");
}

/// A repository with `files` committed, then `changes` committed on top.
fn repo(files: &[(&str, &str)], changes: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    for (p, c) in files {
        write(root, p, c);
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "initial"]);
    for (p, c) in changes {
        write(root, p, c);
    }
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "change"]);
    tmp
}

fn run(repo: &Path, args: &[&str]) -> Output {
    Command::new(&*BIN)
        .current_dir(repo)
        .env("DIFFCTX_NO_MARKER", "1")
        .args(args)
        .output()
        .expect("run diffctx")
}

fn impact(repo: &Path) -> serde_json::Value {
    let out = run(
        repo,
        &[
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            "json",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn markdown(repo: &Path) -> String {
    let out = run(
        repo,
        &[
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            "md",
        ],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// `path::symbol` of every caller of `symbol` with the given evidence.
fn callers(doc: &serde_json::Value, symbol: &str, evidence: &str) -> Vec<String> {
    let mut found: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == symbol)
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .filter(|c| c["evidence"] == evidence)
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    found.sort();
    found
}

fn caller<'a>(
    doc: &'a serde_json::Value,
    symbol: &str,
    path: &str,
) -> Option<&'a serde_json::Value> {
    doc["changed"]
        .as_array()?
        .iter()
        .filter(|c| c["symbol"] == symbol)
        .flat_map(|c| c["callers"].as_array().into_iter().flatten())
        .find(|c| c["path"] == path)
}

const BOT: &str = "class Bot:\n    def message(self, text):\n        return text\n";
const BOT_CHANGED: &str = "class Bot:\n    def message(self, text):\n        return text.strip()\n";

#[test]
fn a_method_resolves_through_its_receiver_and_only_through_it() {
    let tmp = repo(
        &[
            ("pkg_b/__init__.py", ""),
            ("pkg_b/bot.py", BOT),
            (
                "pkg_b/app.py",
                "from pkg_b.bot import Bot\n\n\ndef run():\n    bot = Bot()\n    return bot.message('hi')\n",
            ),
            (
                "pkg_b/direct.py",
                "from pkg_b.bot import Bot\n\n\ndef once():\n    return Bot().message('x')\n",
            ),
            ("pkg_a/__init__.py", ""),
            (
                "pkg_a/handler.py",
                "def handle(update):\n    return update.message.text\n",
            ),
            (
                "pkg_a/mixed.py",
                "from pkg_b.bot import Bot\n\n\ndef relay(update):\n    return update.message\n\n\ndef send():\n    b = Bot()\n    return b.message('y')\n",
            ),
            (
                "pkg_a/factory.py",
                "def through(make):\n    bot = make()\n    return bot.message('z')\n",
            ),
            (
                "pkg_a/other.py",
                "from pkg_a.mail import Mail\n\n\ndef post():\n    m = Mail()\n    return m.message('w')\n",
            ),
            (
                "pkg_a/mail.py",
                "class Mail:\n    def message(self, t):\n        return t\n",
            ),
        ],
        &[("pkg_b/bot.py", BOT_CHANGED)],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "message", "resolved"),
        vec![
            "pkg_a/mixed.py::send",
            "pkg_b/app.py::run",
            "pkg_b/direct.py::once"
        ],
        "{doc}"
    );
    let resolved_or_candidate: Vec<String> = [
        callers(&doc, "message", "resolved"),
        callers(&doc, "message", "candidate"),
    ]
    .concat();
    for wrong in ["pkg_a/other.py::post", "pkg_a/mixed.py::relay"] {
        assert!(
            !callers(&doc, "message", "resolved").contains(&wrong.to_string()),
            "{wrong} must not be a resolved caller: {doc}"
        );
    }
    assert!(
        !resolved_or_candidate.contains(&"pkg_a/other.py::post".to_string()),
        "a receiver bound to another class is rejected, not a candidate: {doc}"
    );
    if let Some(c) = caller(&doc, "message", "pkg_a/factory.py") {
        assert_eq!(
            c["evidence"], "candidate",
            "a factory's product is unknown: {doc}"
        );
    }
}

#[test]
fn a_module_reference_resolves_to_the_file_the_import_names() {
    let tmp = repo(
        &[
            ("shop/__init__.py", ""),
            (
                "shop/pricing.py",
                "def total(items):\n    return sum(items)\n",
            ),
            (
                "shop/checkout.py",
                "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
            ),
            (
                "shop/aliased.py",
                "import shop.pricing as p\n\n\ndef quote(cart):\n    return p.total(cart)\n",
            ),
            ("legacy/__init__.py", ""),
            (
                "legacy/pricing.py",
                "def total(rows):\n    return len(rows)\n",
            ),
            (
                "legacy/invoice.py",
                "from legacy.pricing import total\n\n\ndef bill(rows):\n    return total(rows)\n",
            ),
        ],
        &[(
            "shop/pricing.py",
            "def total(items):\n    return sum(items) * 2\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "total", "resolved"),
        vec!["shop/aliased.py::quote", "shop/checkout.py::charge"],
        "{doc}"
    );
    assert!(
        caller(&doc, "total", "legacy/invoice.py").is_none(),
        "{doc}"
    );
}

#[test]
fn duplicated_source_roots_resolve_to_the_importers_own_root() {
    let pricing = "def total(items):\n    return sum(items)\n";
    let checkout =
        "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n";
    let tmp = repo(
        &[
            ("svc_a/shop/__init__.py", ""),
            ("svc_a/shop/pricing.py", pricing),
            ("svc_a/shop/checkout.py", checkout),
            ("svc_b/shop/__init__.py", ""),
            ("svc_b/shop/pricing.py", pricing),
            ("svc_b/shop/checkout.py", checkout),
        ],
        &[(
            "svc_a/shop/pricing.py",
            "def total(items):\n    return sum(items) + 1\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "total", "resolved"),
        vec!["svc_a/shop/checkout.py::charge"],
        "{doc}"
    );
    assert!(
        caller(&doc, "total", "svc_b/shop/checkout.py").is_none(),
        "{doc}"
    );
}

#[test]
fn a_re_export_resolves_without_naming_the_defining_module() {
    let tmp = repo(
        &[
            ("models/__init__.py", "from .wearables import Steps\n"),
            ("models/wearables.py", "class Steps:\n    pass\n"),
            (
                "api.py",
                "from models import Steps\n\n\ndef latest():\n    return Steps()\n",
            ),
        ],
        &[("models/wearables.py", "class Steps:\n    count = 0\n")],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "Steps", "resolved"),
        vec!["api.py::latest"],
        "{doc}"
    );
}

#[test]
fn two_apps_with_the_same_file_name_keep_their_own_callers() {
    let app = "export function App() {\n  return 1;\n}\n";
    let main = "import { App } from './App';\n\nexport function start() {\n  return App();\n}\n";
    let tmp = repo(
        &[
            ("appA/src/App.tsx", app),
            ("appA/src/main.tsx", main),
            ("appB/src/App.tsx", app),
            ("appB/src/main.tsx", main),
        ],
        &[(
            "appA/src/App.tsx",
            "export function App() {\n  return 2;\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "App", "resolved"),
        vec!["appA/src/main.tsx::start"],
        "{doc}"
    );
    assert!(caller(&doc, "App", "appB/src/main.tsx").is_none(), "{doc}");
}

#[test]
fn module_level_code_split_into_chunks_is_one_caller() {
    let tmp = repo(
        &[
            ("web/App.tsx", "export function App() {\n  return 1;\n}\n"),
            (
                "web/main.tsx",
                "import { App } from './App';\n\nconsole.log(App());\nwindow.start = 1;\n\nexport function helper() {\n  return 0;\n}\n\nconsole.log(App());\nwindow.done = 1;\n",
            ),
        ],
        &[("web/App.tsx", "export function App() {\n  return 2;\n}\n")],
    );
    let doc = impact(tmp.path());
    let from_main: Vec<&serde_json::Value> = doc["changed"][0]["callers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["path"] == "web/main.tsx")
        .collect();
    assert_eq!(from_main.len(), 1, "{doc}");
    assert_eq!(from_main[0]["sites"], serde_json::json!([3, 10]), "{doc}");
}

#[test]
fn a_declared_path_alias_resolves_and_an_undeclared_one_is_only_possible() {
    let files = |config: bool| {
        let mut f = vec![
            (
                "src/util/math.ts",
                "export function add(a: number, b: number) {\n  return a + b;\n}\n",
            ),
            (
                "src/page.ts",
                "import { add } from '@/util/math';\n\nexport function sum() {\n  return add(1, 2);\n}\n",
            ),
            (
                "packages/web/view.ts",
                "import { add } from '../../src/util/math';\n\nexport function show() {\n  return add(3, 4);\n}\n",
            ),
        ];
        if config {
            f.push((
                "tsconfig.json",
                "{\n  // aliases\n  \"compilerOptions\": {\"baseUrl\": \".\", \"paths\": {\"@/*\": [\"src/*\"]},},\n}\n",
            ));
        }
        f
    };
    let change = [(
        "src/util/math.ts",
        "export function add(a: number, b: number) {\n  return b + a;\n}\n",
    )];
    let with = repo(&files(true), &change);
    let doc = impact(with.path());
    assert_eq!(
        callers(&doc, "add", "resolved"),
        vec!["packages/web/view.ts::show", "src/page.ts::sum"],
        "{doc}"
    );
    let without = repo(&files(false), &change);
    let doc = impact(without.path());
    assert_eq!(
        callers(&doc, "add", "resolved"),
        vec!["packages/web/view.ts::show"],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "add", "candidate"),
        vec!["src/page.ts::sum"],
        "{doc}"
    );
    assert_eq!(
        caller(&doc, "add", "src/page.ts").unwrap()["reason"],
        "import_unresolved"
    );
}

#[test]
fn an_alias_is_a_caller_its_tests_reach_and_shadowing_breaks_it() {
    let tmp = repo(
        &[
            ("shop/__init__.py", ""),
            (
                "shop/timing.py",
                "def elapsed(start):\n    return 3 - start\n",
            ),
            (
                "shop/checkout.py",
                "from shop.timing import elapsed as took\n\n\ndef is_slow(start):\n    return took(start) > 2\n\n\ndef shadowed(took):\n    return took(1)\n\n\ndef later(register):\n    return register(took)\n",
            ),
            (
                "shop/view.py",
                "import shop.timing as tm\n\n\ndef badge(start):\n    return tm.elapsed(start)\n",
            ),
            (
                "tests/test_checkout.py",
                "from shop.checkout import is_slow\n\n\ndef test_is_slow():\n    assert is_slow(0)\n",
            ),
            (
                "tests/test_other.py",
                "def badge(x):\n    return x\n\n\ndef test_badge():\n    assert badge(1)\n",
            ),
        ],
        &[(
            "shop/timing.py",
            "def elapsed(start):\n    return 4 - start\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "elapsed", "resolved"),
        vec![
            "shop/checkout.py::is_slow",
            "shop/checkout.py::later",
            "shop/view.py::badge"
        ],
        "{doc}"
    );
    let by_symbol = |name: &str| {
        doc["changed"][0]["callers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["symbol"] == name)
            .cloned()
            .unwrap()
    };
    let is_slow = by_symbol("is_slow");
    assert_eq!(is_slow["tested_by"], "tests/test_checkout.py", "{doc}");
    let later = doc["changed"][0]["callers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "later")
        .unwrap();
    assert_eq!(
        later["relation"], "reference",
        "passing the alias on is not a call: {doc}"
    );
    let badge = caller(&doc, "elapsed", "shop/view.py").unwrap();
    assert!(
        badge.get("tested_by").is_none(),
        "a same-named helper in another test is not a test of shop/view.py::badge: {doc}"
    );
    let text = markdown(tmp.path());
    assert!(
        text.contains("reachable from tests: tests/test_checkout.py"),
        "{text}"
    );
    assert!(text.contains("not execution coverage"), "{text}");
}

#[test]
fn a_trait_method_reached_by_dynamic_dispatch_is_never_reported_as_uncalled() {
    let tmp = repo(
        &[
            (
                "src/lib.rs",
                "pub mod run;\n\npub trait Builder {\n    fn build(&self) -> u32;\n}\n\npub struct Fast;\n\nimpl Builder for Fast {\n    fn build(&self) -> u32 {\n        1\n    }\n}\n",
            ),
            (
                "src/run.rs",
                "use crate::Builder;\n\npub fn run(b: &dyn Builder) -> u32 {\n    b.build()\n}\n",
            ),
        ],
        &[(
            "src/lib.rs",
            "pub mod run;\n\npub trait Builder {\n    fn build(&self) -> u32;\n}\n\npub struct Fast;\n\nimpl Builder for Fast {\n    fn build(&self) -> u32 {\n        2\n    }\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    let build = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "build")
        .unwrap_or_else(|| panic!("the dispatched method must be listed: {doc}"));
    assert!(
        build["unresolved"]
            .as_str()
            .unwrap()
            .contains("dyn Builder"),
        "{doc}"
    );
    let text = markdown(tmp.path());
    assert!(!text.contains("No resolved static callers"), "{text}");
    assert!(text.contains("callers not resolved"), "{text}");
}

#[test]
fn a_name_defined_everywhere_says_so_and_a_qualified_query_still_resolves() {
    let def = |n: u32| format!("pub fn build() -> u32 {{\n    {n}\n}}\n");
    let tmp = repo(
        &[
            (
                "src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod user;\n",
            ),
            ("src/a.rs", &def(1)),
            ("src/b.rs", &def(2)),
            ("src/c.rs", &def(3)),
            (
                "src/user.rs",
                "use crate::a;\n\npub fn go() -> u32 {\n    a::build()\n}\n",
            ),
        ],
        &[("src/a.rs", &def(10))],
    );
    let doc = impact(tmp.path());
    let build = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "build")
        .unwrap_or_else(|| panic!("{doc}"));
    assert!(
        build["unresolved"].as_str().unwrap().contains("defined in"),
        "{doc}"
    );
    let out = run(
        tmp.path(),
        &["--symbol", "src/a.rs:build", "-q", "-f", "md"],
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("called from src/user.rs::go"), "{text}");
}

#[test]
fn an_unresolved_receiver_is_a_possible_caller_and_counted_as_one() {
    let tmp = repo(
        &[
            (
                "ledger.py",
                "class Ledger:\n    def reconcile_all(self):\n        return 1\n",
            ),
            (
                "jobs.py",
                "def nightly(ledger):\n    return ledger.reconcile_all()\n",
            ),
        ],
        &[(
            "ledger.py",
            "class Ledger:\n    def reconcile_all(self):\n        return 2\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "reconcile_all", "candidate"),
        vec!["jobs.py::nightly"],
        "{doc}"
    );
    let text = markdown(tmp.path());
    assert!(
        text.contains("possibly called from jobs.py::nightly"),
        "{text}"
    );
    assert!(text.contains("0 caller(s) outside the diff"), "{text}");
    assert!(text.contains("1 possible"), "{text}");
}

#[test]
fn a_function_nothing_references_is_reported_within_its_scope() {
    let tmp = repo(
        &[
            ("tool.py", "def helper():\n    return 1\n"),
            ("main.py", "def main():\n    return 0\n"),
        ],
        &[("tool.py", "def helper():\n    return 2\n")],
    );
    let text = markdown(tmp.path());
    assert!(
        text.contains("No resolved static callers outside the diff in the analysed scope."),
        "{text}"
    );
}
