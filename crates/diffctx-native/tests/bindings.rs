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
        build["unresolved"]
            .as_str()
            .unwrap()
            .contains("`build` is ambiguous: 3 rust definitions"),
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

/// #372: an app's own tsconfig (`tsconfig.app.json`, reached through
/// `extends`) maps `@pkg/*` to a shared directory; there is no root tsconfig.
#[test]
fn a_path_alias_in_the_importers_own_tsconfig_resolves() {
    let tmp = repo(
        &[
            (
                "shared/pkg/auto-update.ts",
                "export function installAutoUpdate(minutes: number): number {\n  return minutes * 60;\n}\n",
            ),
            (
                "apps/web/tsconfig.json",
                "{\n  // the app config carries the aliases\n  \"extends\": \"./tsconfig.app.json\",\n}\n",
            ),
            (
                "apps/web/tsconfig.app.json",
                "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@pkg/*\": [\"../../shared/pkg/*\"] }\n  }\n}\n",
            ),
            (
                "apps/web/src/main.ts",
                "import { installAutoUpdate } from '@pkg/auto-update';\n\nexport function boot(): number {\n  return installAutoUpdate(15);\n}\n",
            ),
        ],
        &[(
            "shared/pkg/auto-update.ts",
            "export function installAutoUpdate(minutes: number): number {\n  return minutes * 60 * 1000;\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "installAutoUpdate", "resolved"),
        vec!["apps/web/src/main.ts::boot"],
        "{doc}"
    );
}

/// A root `"baseUrl": "."` is the repository root, not "no baseUrl": a bare
/// `src/…` specifier resolves through it. Overlapping `paths` patterns pick
/// the longest prefix, as tsc does (#414).
#[test]
fn a_root_base_url_and_the_longest_paths_prefix_resolve() {
    let tmp = repo(
        &[
            (
                "tsconfig.json",
                "{\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    \"paths\": { \"@/*\": [\"src/*\"], \"@/components/*\": [\"src/ui/*\"] }\n  }\n}\n",
            ),
            (
                "src/components/Button.ts",
                "export function button(): number {\n  return 1;\n}\n",
            ),
            (
                "src/ui/Button.ts",
                "export function button(): number {\n  return 2;\n}\n",
            ),
            (
                "src/money.ts",
                "export function cents(x: number): number {\n  return x * 100;\n}\n",
            ),
            (
                "app/page.ts",
                "import { cents } from 'src/money';\nimport { button } from '@/components/Button';\n\nexport function render(): number {\n  return cents(button());\n}\n",
            ),
        ],
        &[
            (
                "src/money.ts",
                "export function cents(x: number): number {\n  return Math.round(x * 100);\n}\n",
            ),
            (
                "src/ui/Button.ts",
                "export function button(): number {\n  return 3;\n}\n",
            ),
        ],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "cents", "resolved"),
        vec!["app/page.ts::render"],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "button", "resolved"),
        vec!["app/page.ts::render"],
        "{doc}"
    );
}

/// `...name(...)` spreads a call's result; the dots are no receiver. Every
/// Vite config spreading `...workboxPolicy({..})` was a rejected binding.
#[test]
fn a_spread_call_of_an_imported_function_is_a_caller() {
    let tmp = repo(
        &[
            (
                "shared/pwa/workbox-policy.js",
                "export function workboxPolicy(o) {\n  return o;\n}\n",
            ),
            (
                "app/vite.config.ts",
                "import { workboxPolicy } from \"../shared/pwa/workbox-policy.js\";\n\nexport default function config() {\n  return { ...workboxPolicy({ a: 1 }) };\n}\n",
            ),
        ],
        &[(
            "shared/pwa/workbox-policy.js",
            "export function workboxPolicy(o) {\n  return { ...o };\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "workboxPolicy", "resolved"),
        vec!["app/vite.config.ts::config"],
        "{doc}"
    );
}

/// `with Server() as srv:` binds `srv` to what `Server` constructs, and no
/// binding is read across a line: `as srv:` followed by `result =
/// srv.call_tool(...)` on the next line made the call its own constructor
/// and dropped the caller (#399). An MCP client's `session.call_tool` is
/// another class's method.
#[test]
fn a_context_manager_binds_its_target_and_no_binding_spans_lines() {
    let tmp = repo(
        &[
            ("server/src/srv/__init__.py", ""),
            (
                "server/src/srv/app.py",
                "class Server:\n    def __init__(self):\n        self.tools = {}\n\n    def call_tool(self, name, arguments):\n        return self.tools[name](**arguments)\n",
            ),
            (
                "server/tests/test_app.py",
                "from srv.app import Server\n\n\ndef test_call_tool():\n    s = Server()\n    assert s.call_tool(\"echo\", {}) == \"hi\"\n",
            ),
            (
                "server/tests/test_ctx.py",
                "from srv.app import Server\n\n\ndef test_call_tool_in_context():\n    with Server() as srv:\n        result = srv.call_tool(\"echo\", {\"text\": \"hi\"})\n    assert result == \"hi\"\n\n\ndef test_call_tool_returned():\n    with Server() as srv:\n        return srv.call_tool(\"echo\", {\"text\": \"hi\"})\n",
            ),
            ("client/src/cli/__init__.py", ""),
            (
                "client/src/cli/run.py",
                "from mcp import ClientSession\n\n\nasync def ask(read, write, name):\n    async with ClientSession(read, write) as session:\n        await session.initialize()\n        return await session.call_tool(name, {})\n",
            ),
            (
                "client/tests/test_run.py",
                "from mcp import ClientSession\n\n\nasync def test_session_calls_a_tool(read, write):\n    async with ClientSession(read, write) as session:\n        result = await session.call_tool(\"echo\", {})\n    assert result\n\n\nasync def test_fixture_session(session):\n    assert await session.call_tool(\"echo\", {})\n",
            ),
        ],
        &[(
            "server/src/srv/app.py",
            "class Server:\n    def __init__(self):\n        self.tools = {}\n\n    def call_tool(self, name, arguments):\n        tool = self.tools[name]\n        return tool(**arguments)\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "call_tool", "resolved"),
        vec![
            "server/tests/test_app.py::test_call_tool",
            "server/tests/test_ctx.py::test_call_tool_in_context",
            "server/tests/test_ctx.py::test_call_tool_returned",
        ],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "call_tool", "candidate"),
        vec!["client/tests/test_run.py::test_fixture_session"],
        "a parameter nothing types stays possible: {doc}"
    );
}

const LEDGER_GO: &str = "package billing\n\n// Total doubles n.\nfunc Total(n int) int {\n\treturn n * 2\n}\n\ntype Ledger struct{ rate int }\n\nfunc (l *Ledger) Reconcile(n int) int {\n\treturn n * l.rate\n}\n";
const LEDGER_GO_CHANGED: &str = "package billing\n\n// Total doubles n.\nfunc Total(n int) int {\n\treturn n * 3\n}\n\ntype Ledger struct{ rate int }\n\nfunc (l *Ledger) Reconcile(n int) int {\n\treturn n*l.rate + 1\n}\n";

/// #506: a Go package is its directory and an import binds it by path. The
/// decoys spell the changed names without reaching them: a same-named
/// function and method of another package, a struct field, a local, a
/// string, a method of another type in the same package.
#[test]
fn go_callers_bind_through_the_package_and_the_stated_type() {
    let tmp = repo(
        &[
            ("go.mod", "module example.com/shop\n\ngo 1.22\n"),
            ("billing/ledger.go", LEDGER_GO),
            (
                "billing/report.go",
                "package billing\n\nfunc Report() int {\n\treturn Total(3)\n}\n\nfunc Close(l *Ledger) int {\n\treturn l.Reconcile(1)\n}\n",
            ),
            (
                "billing/audit.go",
                "package billing\n\ntype Audit struct{}\n\nfunc (Audit) Reconcile(n int) int { return n }\n\nfunc Check() int {\n\ta := Audit{}\n\treturn a.Reconcile(5)\n}\n",
            ),
            (
                "cmd/app/main.go",
                "package main\n\nimport \"example.com/shop/billing\"\n\nfunc main() {\n\tl := &billing.Ledger{}\n\t_ = l.Reconcile(billing.Total(1))\n}\n",
            ),
            (
                "cmd/alias/main.go",
                "package main\n\nimport b \"example.com/shop/billing\"\n\nfunc main() {\n\t_ = b.Total(2)\n}\n",
            ),
            (
                "cmd/jobs/run.go",
                "package main\n\nimport \"example.com/shop/billing\"\n\nfunc nightly(get func() *billing.Ledger) int {\n\treturn get().Reconcile(4)\n}\n",
            ),
            (
                "cmd/fields/main.go",
                "package main\n\nimport \"example.com/shop/billing\"\n\ntype Row struct{ Total int }\n\nvar _ billing.Ledger\n\nfunc main() {\n\tr := Row{Total: 1}\n\tTotal := r.Total\n\tprintln(Total, \"billing.Total(1)\")\n}\n",
            ),
            (
                "tax/tax.go",
                "package tax\n\nfunc Total(n int) int {\n\treturn n\n}\n\ntype Account struct{}\n\nfunc (a Account) Reconcile(n int) int {\n\treturn n\n}\n\nfunc Use() int {\n\tvar a Account\n\treturn Total(1) + a.Reconcile(2)\n}\n",
            ),
            (
                "cmd/other/main.go",
                "package main\n\nimport \"example.com/shop/tax\"\n\nfunc main() {\n\tprintln(tax.Total(1))\n}\n",
            ),
        ],
        &[("billing/ledger.go", LEDGER_GO_CHANGED)],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "Total", "resolved"),
        vec![
            "billing/report.go::Report",
            "cmd/alias/main.go::main",
            "cmd/app/main.go::main"
        ],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "Total", "candidate"),
        Vec::<String>::new(),
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "Reconcile", "resolved"),
        vec!["billing/report.go::Close", "cmd/app/main.go::main"],
        "{doc}"
    );
    // A value nothing types may be a Ledger; a stated Audit may embed one.
    assert_eq!(
        callers(&doc, "Reconcile", "candidate"),
        vec!["billing/audit.go::Check", "cmd/jobs/run.go::nightly"],
        "{doc}"
    );
}

/// #449: module-level calls are callers beside a function-level one in the
/// same file; only the import line that names the definition is not.
#[test]
fn module_level_calls_stay_beside_a_function_level_caller() {
    let tmp = repo(
        &[
            (
                "src/reg.ts",
                "export function register(x: string) {\n  return x + \"!\";\n}\n",
            ),
            (
                "src/boot.ts",
                "import { register } from \"./reg\";\nexport function install() {\n  register(\"a\");\n}\nif (process.env.X) {\n  register(\"cond\");\n}\nregister(\"plain\");\n",
            ),
        ],
        &[(
            "src/reg.ts",
            "export function register(x: string) {\n  return x + \"?\";\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    let sites: Vec<u64> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "register")
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .flat_map(|c| c["sites"].as_array().cloned().unwrap_or_default())
        .filter_map(|s| s.as_u64())
        .collect();
    for line in [3, 6, 8] {
        assert!(sites.contains(&line), "call on line {line}: {doc}");
    }
    assert!(
        !sites.contains(&1),
        "the import line is no call site: {doc}"
    );
}

/// #472: a long caller split into chunks is one caller whose range covers
/// every call site, not the first chunk's.
#[test]
fn a_caller_split_into_chunks_spans_all_its_call_sites() {
    let filler = "    if a:\n        pass\n    for k in range(3):\n        pass\n    w = [1, 2, 3]\n    # filler\n\n";
    let body: String = (1..=4)
        .map(|i| format!("    v{i} = stated_owner(a)\n{filler}"))
        .collect();
    let tmp = repo(
        &[
            ("owner.py", "def stated_owner(x):\n    return x\n"),
            (
                "nw.py",
                &format!(
                    "from owner import stated_owner\n\n\ndef _net_worth_terms(a):\n{body}    return v1\n"
                ),
            ),
        ],
        &[("owner.py", "def stated_owner(x):\n    return x or None\n")],
    );
    let doc = impact(tmp.path());
    let c = caller(&doc, "stated_owner", "nw.py").expect("nw.py is a caller");
    let (start, end) = c["lines"]
        .as_str()
        .and_then(|l| l.split_once('-'))
        .map(|(a, b)| (a.parse::<u64>().unwrap(), b.parse::<u64>().unwrap()))
        .expect("a line range");
    for site in c["sites"].as_array().expect("sites") {
        let site = site.as_u64().unwrap();
        assert!(
            start <= site && site <= end,
            "site {site} outside {start}-{end}: {doc}"
        );
    }
    assert_eq!(c["sites"].as_array().unwrap().len(), 4, "{doc}");
}

/// #481: an `async function` or a generator is a definition like any other,
/// so a same-file call reaches it.
#[test]
fn async_and_generator_functions_have_their_same_file_callers() {
    let base = "async function target(x) {\n  return x + 1;\n}\n\nexport async function exported(x) {\n  return x;\n}\n\nfunction* rows(n) {\n  yield n;\n}\n\nexport function callerA() {\n  return [target(1), exported(2), rows(3)];\n}\n";
    let tmp = repo(
        &[("a.js", base)],
        &[(
            "a.js",
            &base
                .replace("x + 1", "x + 2")
                .replace("return x;", "return x * 1;")
                .replace("yield n;", "yield n + 1;"),
        )],
    );
    let doc = impact(tmp.path());
    for symbol in ["target", "exported", "rows"] {
        assert_eq!(
            callers(&doc, symbol, "resolved"),
            vec!["a.js::callerA"],
            "{symbol}: {doc}"
        );
    }
}

/// #428: a tool's `selftest*.mjs` files are its tests, so a caller they
/// reach is tested.
#[test]
fn selftest_scripts_are_tests() {
    let tmp = repo(
        &[
            (
                "scripts/lib.mjs",
                "export function util(x) {\n  return x + 1;\n}\n",
            ),
            (
                "scripts/qa.mjs",
                "import { util } from \"./lib.mjs\";\n\nexport function runQa() {\n  return util(1);\n}\n",
            ),
            (
                "scripts/selftest-qa.mjs",
                "import { runQa } from \"./qa.mjs\";\n\nexport function checkQa() {\n  if (runQa() !== 2) throw new Error(\"qa\");\n}\n",
            ),
        ],
        &[(
            "scripts/lib.mjs",
            "export function util(x) {\n  return x + 2;\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    let c = caller(&doc, "util", "scripts/qa.mjs").expect("qa.mjs calls util");
    assert_eq!(c["tested_by"], "scripts/selftest-qa.mjs", "{doc}");
}

/// #497: a shell function is called by name as a command word — from its
/// own script's body or from a script that sources it; a script that does
/// not source it, or names it as an argument, is no caller.
#[test]
fn shell_functions_are_called_from_script_bodies_and_sourcing_scripts() {
    let lib = "#!/bin/sh\nrun_tool() {\n  echo \"tool $1\"\n}\n\nrun_tool x\n";
    let tmp = repo(
        &[
            ("lib.sh", lib),
            (
                "main.sh",
                "#!/bin/sh\n. ./lib.sh\nif run_tool y; then\n  exit 0\nfi\n",
            ),
            (
                "other.sh",
                "#!/bin/sh\nrun_tool() {\n  true\n}\nrun_tool z\necho run_tool\n",
            ),
        ],
        &[(
            "lib.sh",
            &lib.replace("echo \"tool $1\"", "printf '%s\\n' \"$1\""),
        )],
    );
    let doc = impact(tmp.path());
    let paths: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "run_tool")
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|c| c["path"].as_str().unwrap().to_string())
        .collect();
    assert!(paths.contains(&"lib.sh".to_string()), "{doc}");
    assert!(paths.contains(&"main.sh".to_string()), "{doc}");
    assert!(!paths.contains(&"other.sh".to_string()), "{doc}");
}

/// #452: a call inside a local `const` is its function's call; the
/// function is the caller, with the test that reaches it.
#[test]
fn a_call_in_a_local_const_belongs_to_its_function() {
    let tmp = repo(
        &[
            (
                "tools/summary.mjs",
                "export function rows(xs) {\n  return xs.map(x => x + 1);\n}\n",
            ),
            (
                "tools/report.mjs",
                "import { rows } from \"./summary.mjs\";\n\nexport function makeSummary(xs) {\n  const items = rows(xs);\n  return items.length;\n}\n",
            ),
            (
                "tests/report.test.mjs",
                "import { makeSummary } from \"../tools/report.mjs\";\n\nexport function testSummary() {\n  return makeSummary([1]);\n}\n",
            ),
        ],
        &[(
            "tools/summary.mjs",
            "export function rows(xs) {\n  return xs.map(x => x + 2);\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "rows", "resolved"),
        vec!["tools/report.mjs::makeSummary"],
        "{doc}"
    );
    let c = caller(&doc, "rows", "tools/report.mjs").unwrap();
    assert_eq!(c["tested_by"], "tests/report.test.mjs", "{doc}");
}

/// #318: a changed constructor is called by its class's name — through a
/// module alias, an imported name, `new` — and a same-named class of
/// another module is no caller.
#[test]
fn a_changed_constructor_is_called_by_its_class_name() {
    let ctor = "class ComponentModel:\n    def __init__(self, base, n):\n        self.base = base\n        self.n = n\n\n    def total(self):\n        return self.base * self.n\n";
    let store = "export class Store {\n  constructor(size) {\n    this.size = size;\n  }\n}\n";
    let tmp = repo(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/components.py", ctor),
            (
                "pkg/app.py",
                "from pkg import components as cp\n\n\ndef build(b):\n    return cp.ComponentModel(b, 1)\n",
            ),
            (
                "pkg/direct.py",
                "from pkg.components import ComponentModel\n\n\ndef make(b):\n    return ComponentModel(b, 2)\n",
            ),
            (
                "pkg/other.py",
                "class ComponentModel:\n    def __init__(self, x):\n        self.x = x\n\n\ndef decoy():\n    return ComponentModel(3)\n",
            ),
            ("web/store.js", store),
            (
                "web/app.js",
                "import { Store } from \"./store.js\";\n\nexport function boot() {\n  return new Store(4);\n}\n",
            ),
        ],
        &[
            (
                "pkg/components.py",
                &ctor.replace("self.n = n", "self.n = max(n, 1)"),
            ),
            (
                "web/store.js",
                &store.replace("this.size = size;", "this.size = size || 1;"),
            ),
        ],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "ComponentModel", "resolved"),
        vec!["pkg/app.py::build", "pkg/direct.py::make"],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "Store", "resolved"),
        vec!["web/app.js::boot"],
        "{doc}"
    );
}

/// #479: a component reached through `lazy(() => import(…).then(m => …))`
/// or `const { X } = await import(…)` is referenced there; `m.X` on a
/// plain parameter is not.
#[test]
fn a_dynamic_import_binds_its_module() {
    let player = "export function MobileFullPlayer() {\n  return 1;\n}\n";
    let tmp = repo(
        &[
            ("src/MobileFullPlayer.tsx", player),
            (
                "src/PlayerBar.tsx",
                "import { lazy } from \"react\";\nconst MobileFullPlayer = lazy(async () =>\n  import(\"./MobileFullPlayer\").then((m) => ({ default: m.MobileFullPlayer })),\n);\nexport function PlayerBar() {\n  return MobileFullPlayer;\n}\n",
            ),
            (
                "src/preload.ts",
                "export async function preload() {\n  const { MobileFullPlayer } = await import(\"./MobileFullPlayer\");\n  return MobileFullPlayer();\n}\n",
            ),
            (
                "src/decoy.ts",
                "export function pick(m: { MobileFullPlayer: () => number }) {\n  return m.MobileFullPlayer();\n}\n",
            ),
        ],
        &[(
            "src/MobileFullPlayer.tsx",
            &player.replace("return 1;", "return 2;"),
        )],
    );
    let doc = impact(tmp.path());
    let found: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .filter(|c| c["evidence"] == "resolved")
        .map(|c| c["path"].as_str().unwrap().to_string())
        .collect();
    assert!(found.contains(&"src/PlayerBar.tsx".to_string()), "{doc}");
    assert!(found.contains(&"src/preload.ts".to_string()), "{doc}");
    assert!(!found.contains(&"src/decoy.ts".to_string()), "{doc}");
}

/// #401: a pytest fixture is called by every test and fixture that names it
/// as a parameter under its conftest's directory; a test elsewhere, or one
/// naming a longer parameter, is not.
#[test]
fn a_pytest_fixture_is_called_by_the_tests_that_take_it() {
    let conftest = "import pytest\n\n\n@pytest.fixture\ndef flask_client():\n    return 1\n";
    let tmp = repo(
        &[
            ("tests/conftest.py", conftest),
            (
                "tests/test_x.py",
                "def test_x(flask_client):\n    assert flask_client == 1\n",
            ),
            (
                "tests/sub/test_y.py",
                "import pytest\n\n\n@pytest.fixture\ndef logged_in(flask_client, tmp_path):\n    return flask_client\n\n\ndef test_y(logged_in):\n    assert logged_in\n",
            ),
            (
                "tests/test_other.py",
                "def test_other(flask_client_x):\n    assert flask_client_x\n",
            ),
            (
                "other/test_z.py",
                "def test_z(flask_client):\n    assert flask_client\n",
            ),
        ],
        &[(
            "tests/conftest.py",
            &conftest.replace("return 1", "return 2"),
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "flask_client", "resolved"),
        vec!["tests/sub/test_y.py::logged_in", "tests/test_x.py::test_x"],
        "{doc}"
    );
}

/// #444, #473: a class method is called through `this` and a typed
/// instance; an object literal's method through the object; a store's
/// action, built inside a call, possibly through any receiver; a same-named
/// method of another object type is at most possible.
#[test]
fn class_object_and_store_methods_have_their_callers() {
    let store = "export class Store {\n  loadItem(id: number) {\n    return id;\n  }\n\n  private helper() {\n    return this.loadItem(2);\n  }\n}\n\nexport const cart = {\n  total() {\n    return 1;\n  },\n};\n\nexport const usePlayer = create()((set) => ({\n  updateLyrics: (lyrics: string) => {\n    set({ lyrics });\n  },\n}));\n";
    let tmp = repo(
        &[
            ("src/net/store.ts", store),
            (
                "src/views/a.ts",
                "import { Store, cart, usePlayer } from '../net/store';\n\nexport function show(store: Store) {\n  cart.total();\n  usePlayer.getState().updateLyrics('x');\n  return store.loadItem(1);\n}\n",
            ),
            (
                "src/views/decoy.ts",
                "export function other(x: { loadItem(n: number): number }) {\n  return x.loadItem(3);\n}\n",
            ),
        ],
        &[(
            "src/net/store.ts",
            &store
                .replace("return id;", "return id + 1;")
                .replace("return 1;", "return 2;")
                .replace("set({ lyrics });", "set({ lyrics: lyrics.trim() });"),
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "loadItem", "resolved"),
        vec!["src/net/store.ts::helper", "src/views/a.ts::show"],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "total", "resolved"),
        vec!["src/views/a.ts::show"],
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "updateLyrics", "candidate"),
        vec!["src/views/a.ts::show"],
        "{doc}"
    );
    assert!(
        !callers(&doc, "loadItem", "resolved").contains(&"src/views/decoy.ts::other".to_string()),
        "{doc}"
    );
}

/// #520: a call wrapped in parentheses at module level is a caller —
/// `const x = (f(y) || [])` opens an expression, not an arrow's parameters —
/// and a call inside an anonymous arrow belongs to the definition around
/// it. A real arrow parameter named like the import still shadows it.
#[test]
fn a_parenthesised_call_is_a_caller_and_an_arrow_parameter_still_shadows() {
    let lib = "export function readJson(path, fallback = null) {\n  return fallback ?? path;\n}\n";
    let imp = "import { readJson } from \"../lib/report.mjs\";\n";
    let tmp = repo(
        &[
            ("lib/report.mjs", lib),
            (
                "scripts/a.mjs",
                &format!("{imp}const paths = (readJson(\"p.json\") || []).map((p) => p.path);\n"),
            ),
            (
                "scripts/f.mjs",
                &format!("{imp}const raw = (readJson(\"p.json\"));\n"),
            ),
            (
                "scripts/e.mjs",
                &format!(
                    "{imp}export function pages() {{\n  return (readJson(\"p.json\") || []).map((p) => p.path);\n}}\n"
                ),
            ),
            (
                "scripts/shadow.mjs",
                &format!("{imp}export const apply = (readJson) => readJson(\"p.json\");\n"),
            ),
        ],
        &[(
            "lib/report.mjs",
            &lib.replace("fallback ?? path", "fallback ?? path ?? null"),
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "readJson", "resolved"),
        vec![
            "scripts/a.mjs::paths",
            "scripts/e.mjs::pages",
            "scripts/f.mjs::raw"
        ],
        "{doc}"
    );
}

/// #425: an `Any` receiver and an object from a factory classmethod
/// (`await Model.from_config(…)`) are possible callers; a receiver
/// annotated with another class is not.
#[test]
fn any_receivers_and_class_factories_are_possible_callers() {
    let model = "class Model:\n    @classmethod\n    async def from_config(cls, cfg):\n        return cls()\n\n    async def search(self, q):\n        return q\n";
    let tmp = repo(
        &[
            ("m.py", model),
            (
                "other.py",
                "class Other:\n    async def search(self, q):\n        return q\n",
            ),
            (
                "use.py",
                "from typing import Any\nfrom m import Model\nfrom other import Other\n\n\nasync def research(model: Any):\n    return await model.search(\"q\")\n\n\nasync def from_factory():\n    model = await Model.from_config({})\n    return await model.search(\"q\")\n\n\nasync def elsewhere(o: Other):\n    return await o.search(\"q\")\n",
            ),
        ],
        &[("m.py", &model.replace("return q\n", "return q.strip()\n"))],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "search", "candidate"),
        vec!["use.py::from_factory", "use.py::research"],
        "{doc}"
    );
    assert!(callers(&doc, "search", "resolved").is_empty(), "{doc}");
}

/// #474: a specifier naming the repository's own workspace package resolves
/// into its source — through `main` to a build that is not committed, then
/// `src/index` and its barrel — and the same name from an external package
/// is no caller.
#[test]
fn a_workspace_package_specifier_resolves_into_its_source() {
    let queue = "export function windowQueueSnapshot(n: number) {\n  return n;\n}\n";
    let tmp = repo(
        &[
            (
                "package.json",
                "{\"private\": true, \"workspaces\": [\"packages/*\", \"apps/*\"]}\n",
            ),
            (
                "packages/core/package.json",
                "{\"name\": \"@scope/core\", \"main\": \"dist/index.js\"}\n",
            ),
            (
                "packages/core/src/index.ts",
                "export { windowQueueSnapshot } from './player/queue';\n",
            ),
            ("packages/core/src/player/queue.ts", queue),
            (
                "apps/web/src/persist.ts",
                "import { windowQueueSnapshot } from '@scope/core';\n\nexport function save() {\n  return windowQueueSnapshot(1);\n}\n",
            ),
            (
                "apps/web/src/other.ts",
                "import { windowQueueSnapshot } from '@other/core';\n\nexport function elsewhere() {\n  return windowQueueSnapshot(2);\n}\n",
            ),
        ],
        &[(
            "packages/core/src/player/queue.ts",
            &queue.replace("return n;", "return n + 1;"),
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "windowQueueSnapshot", "resolved"),
        vec!["apps/web/src/persist.ts::save"],
        "{doc}"
    );
    assert!(
        !doc.to_string().contains("apps/web/src/other.ts"),
        "an external package's name is no caller: {doc}"
    );
}
