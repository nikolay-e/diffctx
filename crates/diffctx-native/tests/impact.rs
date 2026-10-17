// `--mode impact` is what the plugin hook injects before a commit (#310):
// callers outside the diff, whether a test guards each, and cross-commit
// overlap. These run the release binary on real git repositories, the way
// the hook does.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(repo: &Path) {
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
}

fn commit_all(repo: &Path, message: &str) {
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", message]);
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, content).expect("write");
}

fn impact(repo: &Path, range: &str) -> serde_json::Value {
    impact_env(repo, range, &[])
}

fn impact_env(repo: &Path, range: &str, env: &[(&str, &str)]) -> serde_json::Value {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .envs(env.iter().copied())
        .args(["--diff", range, "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "impact failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn impact_markdown(repo: &Path, range: &str) -> String {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", range, "--mode", "impact", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    String::from_utf8(out.stdout).expect("utf8")
}

/// `pricing.total` changes; `checkout.charge` calls it and is pinned by
/// `test_checkout.py`; `report.summarize` calls it and nothing tests it.
fn repo_with_callers() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return sum(i.price for i in items)\n",
    );
    write(
        repo,
        "shop/checkout.py",
        "from shop.pricing import total\n\n\ndef charge(cart):\n    amount = total(cart.items)\n    return cart.pay(amount)\n",
    );
    write(
        repo,
        "shop/report.py",
        "from shop.pricing import total\n\n\ndef summarize(orders):\n    return [total(o.items) for o in orders]\n",
    );
    write(
        repo,
        "tests/test_checkout.py",
        "from shop.checkout import charge\n\n\ndef test_charge_pays_the_total(cart):\n    assert charge(cart) == 30\n",
    );
    write(repo, "README.md", "# shop\n");
    commit_all(repo, "initial");
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    );
    commit_all(repo, "pricing: totals include VAT");
    tmp
}

#[test]
fn callers_outside_the_diff_are_listed_with_their_test_guard() {
    let tmp = repo_with_callers();
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(doc["schema"], "diffctx.impact.v1");
    assert_eq!(doc["empty"], false);
    let changed = doc["changed"].as_array().expect("changed");
    let total = changed
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("the changed function is listed");
    let callers = total["callers"].as_array().expect("callers");
    let by_symbol = |name: &str| {
        callers
            .iter()
            .find(|c| c["symbol"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {callers:?}"))
    };
    let charge = by_symbol("charge");
    assert_eq!(charge["path"], "shop/checkout.py");
    assert_eq!(charge["tested_by"], "tests/test_checkout.py");
    let summarize = by_symbol("summarize");
    assert_eq!(summarize["path"], "shop/report.py");
    assert!(
        summarize.get("tested_by").is_none(),
        "nothing tests summarize: {summarize:?}"
    );
    assert!(
        !callers.iter().any(|c| c["path"] == "shop/pricing.py"),
        "the changed file is never its own caller"
    );
}

#[test]
fn a_test_that_only_shares_the_callers_name_is_not_its_guard() {
    // A test that imports the changed module and happens to use the word
    // `render` once read as the guard of `tools/report.py::render`, which it
    // never imports (#312).
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "lib/core.py", "def core(v):\n    return v\n");
    write(
        repo,
        "tools/report.py",
        "from lib.core import core\n\n\ndef render(v):\n    return core(v) + 1\n",
    );
    write(
        repo,
        "tests/test_core.py",
        "from lib.core import core\n\n\ndef test_core_doubles():\n    render = core(2)\n    assert render == 4\n",
    );
    commit_all(repo, "initial");
    write(repo, "lib/core.py", "def core(v):\n    return v * 2\n");
    commit_all(repo, "core: doubles");
    let doc = impact(repo, "HEAD~1..HEAD");
    let changed = doc["changed"].as_array().expect("changed");
    let core = changed
        .iter()
        .find(|c| c["symbol"] == "core")
        .expect("the changed function is listed");
    let callers = core["callers"].as_array().expect("callers");
    let render = callers
        .iter()
        .find(|c| c["symbol"] == "render")
        .unwrap_or_else(|| panic!("render calls core: {callers:?}"));
    assert!(
        render.get("tested_by").is_none(),
        "a test that never imports tools/report.py is not its guard: {render:?}"
    );
}

#[test]
fn a_two_letter_module_keeps_its_callers() {
    // `src/db.py` once fell into the role-named branch (`mod.rs`, `index.ts`)
    // and every caller was dropped as never naming the module.
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "src/db.py", "def connect(url):\n    return url\n");
    write(
        repo,
        "src/app.py",
        "from src.db import connect\n\n\ndef main():\n    return connect('x')\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "src/db.py",
        "def connect(url):\n    return url.strip()\n",
    );
    commit_all(repo, "db: strip");
    let doc = impact(repo, "HEAD~1..HEAD");
    let callers = doc["changed"][0]["callers"].as_array().expect("callers");
    assert!(
        callers.iter().any(|c| c["symbol"] == "main"),
        "src/app.py::main calls connect: {doc}"
    );
}

#[test]
fn a_deref_assignment_is_a_call_not_a_comment() {
    // `*out = total(items);` starts with `*`, which only opens a comment
    // continuation when a space follows.
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "Cargo.toml",
        "[package]\nname = \"shop\"\nversion = \"0.1.0\"\n",
    );
    write(repo, "src/lib.rs", "pub mod pricing;\npub mod apply;\n");
    write(
        repo,
        "src/pricing.rs",
        "pub fn total(items: &[f64]) -> f64 {\n    items.iter().sum()\n}\n",
    );
    write(
        repo,
        "src/apply.rs",
        "use crate::pricing::total;\n\npub fn apply(out: &mut f64, items: &[f64]) {\n    *out = total(items);\n}\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "src/pricing.rs",
        "pub fn total(items: &[f64]) -> f64 {\n    items.iter().sum::<f64>() * 1.19\n}\n",
    );
    commit_all(repo, "pricing: vat");
    let doc = impact(repo, "HEAD~1..HEAD");
    let total = doc["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["symbol"] == "total")
        .unwrap_or_else(|| panic!("total is listed: {doc}"));
    assert!(
        total["callers"]
            .as_array()
            .expect("callers")
            .iter()
            .any(|c| c["symbol"] == "apply"),
        "apply calls total: {doc}"
    );
}

#[test]
fn a_wide_file_list_yields_to_the_callers() {
    // 300 changed paths once ate the cap and every caller was dropped while
    // `empty` still read false.
    let tmp = repo_with_callers();
    let repo = tmp.path();
    for i in 0..300 {
        write(repo, &format!("notes/entry_{i:03}.txt"), "note\n");
    }
    commit_all(repo, "notes");
    let doc = impact(repo, "HEAD~2..HEAD");
    assert_eq!(doc["empty"], false);
    assert!(
        doc["changed_files_omitted"].as_u64().unwrap_or(0) > 0,
        "{doc}"
    );
    let callers = doc["changed"][0]["callers"].as_array().expect("callers");
    assert!(callers.iter().any(|c| c["symbol"] == "charge"), "{doc}");
    let text = impact_markdown(repo, "HEAD~2..HEAD");
    assert!(text.contains("301 changed file(s)"), "{text}");
    assert!(text.contains("shop/checkout.py::charge"), "{text}");
}

#[test]
fn a_run_that_hit_a_limit_says_so_and_is_never_empty() {
    let tmp = repo_with_callers();
    let doc = impact_env(
        tmp.path(),
        "HEAD~1..HEAD",
        &[("DIFFCTX_MAX_CANDIDATE_FILES", "1")],
    );
    assert!(
        !doc["limits"].as_array().expect("limits").is_empty(),
        "{doc}"
    );
    assert_eq!(doc["empty"], false);
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .env("DIFFCTX_MAX_CANDIDATE_FILES", "1")
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            "md",
        ])
        .output()
        .expect("run diffctx");
    let text = String::from_utf8(out.stdout).expect("utf8");
    assert!(text.contains("(partial: candidate_limit"), "{text}");
    assert!(!text.contains("Nothing outside the diff"), "{text}");
}

#[test]
fn a_cap_every_repository_trips_does_not_make_an_empty_answer_speak() {
    // 1.18.1 listed every limit, so the per-node edge cap a real repository
    // always hits turned each commit's empty answer into a "(partial: ...)"
    // injection (#306). This used the contribution cap as its stand-in, a
    // 20M memory guard no repository measured trips, which drops edges
    // wholesale and is a real loss.
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "lone.py", "def lone():\n    return 1\n");
    write(
        repo,
        "helpers.py",
        "def a():\n    return 1\n\n\ndef b():\n    return 2\n",
    );
    write(
        repo,
        "other.py",
        "from helpers import a, b\n\n\ndef other():\n    return a() + b()\n",
    );
    commit_all(repo, "initial");
    write(repo, "lone.py", "def lone():\n    return 3\n");
    commit_all(repo, "lone: 3");
    let cap = [("DIFFCTX_MAX_EDGES_PER_NODE", "1")];
    let pack = Command::new(&*BIN)
        .current_dir(repo)
        .envs(cap)
        .args(["--diff", "HEAD~1..HEAD", "-q", "-f", "json"])
        .output()
        .expect("run diffctx");
    let pack: serde_json::Value = serde_json::from_slice(&pack.stdout).expect("pack JSON");
    assert!(
        pack["coverage"]["capped"]
            .as_array()
            .is_some_and(|c| c.iter().any(|r| r == "edge_limit")),
        "the cap must fire, or this measures nothing: {pack:#}"
    );
    let doc = impact_env(repo, "HEAD~1..HEAD", &cap);
    assert_eq!(doc["empty"], true, "{doc}");
    assert!(doc.get("limits").is_none(), "{doc}");
}

/// #446: the contribution and total-byte caps drop edges or files wholesale
/// once a run outgrows them: callers are lost, so the answer says partial
/// (pack's rule) instead of reading the loss as no caller.
#[test]
fn the_wholesale_caps_are_losses_the_answer_names() {
    let tmp = repo_with_callers();
    for (env, reason) in [
        ("DIFFCTX_MAX_EDGE_CONTRIBUTIONS", "edge_contribution_limit"),
        ("DIFFCTX_MAX_SOURCE_BYTES", "total_byte_limit"),
    ] {
        let doc = impact_env(tmp.path(), "HEAD~1..HEAD", &[(env, "1")]);
        assert_eq!(doc["empty"], false, "{env}: {doc:#}");
        assert!(
            doc["limits"]
                .as_array()
                .is_some_and(|l| l.iter().any(|r| r == reason)),
            "{env}: {doc:#}"
        );
    }
}

#[test]
fn a_test_named_for_a_schema_is_not_a_schema_contract() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "tests/test_context_schema.py",
        "def test_a():\n    assert True\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "tests/test_context_schema.py",
        "def test_a():\n    assert 1\n",
    );
    commit_all(repo, "test");
    let doc = impact(repo, "HEAD~1..HEAD");
    let contracts = doc
        .get("contracts")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(!contracts.iter().any(|c| c["kind"] == "schema"), "{doc}");
}

/// #413: the Rails schema dump and its migrations are schema contracts; a
/// package manager's `versions/` directory is not.
#[test]
fn rails_schema_and_migrations_are_contracts_and_yarn_versions_are_not() {
    let schema_kinds = |files: &[(&str, &str, &str)]| {
        let tmp = repo_of(
            &files.iter().map(|(p, a, _)| (*p, *a)).collect::<Vec<_>>(),
            &files.iter().map(|(p, _, b)| (*p, *b)).collect::<Vec<_>>(),
        );
        let doc = impact(tmp.path(), "HEAD~1..HEAD");
        doc.get("contracts")
            .and_then(|c| c.as_array())
            .map(|c| {
                c.iter()
                    .filter(|c| c["kind"] == "schema")
                    .filter_map(|c| c["path"].as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let kinds = schema_kinds(&[
        (
            "db/schema.rb",
            "create_table :users do |t|\nend\n",
            "create_table :users do |t|\n  t.string :name\nend\n",
        ),
        (
            "db/migrate/20240101_add_name.rb",
            "class AddName\nend\n",
            "class AddName\n  def change\n  end\nend\n",
        ),
        (
            ".yarn/versions/abc123.yml",
            "releases: {}\n",
            "releases:\n  a: patch\n",
        ),
    ]);
    assert!(kinds.contains(&"db/schema.rb".to_string()), "{kinds:?}");
    assert!(
        kinds.contains(&"db/migrate/20240101_add_name.rb".to_string()),
        "{kinds:?}"
    );
    assert!(!kinds.iter().any(|k| k.starts_with(".yarn/")), "{kinds:?}");
}

fn ts_repo_with_panel() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "src/engine/panel.ts",
        "export const clamp = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));\n\nexport function mono(x: number): number {\n  return x * 2;\n}\n",
    );
    for i in 1..=3 {
        write(
            repo,
            &format!("src/cards/card{i}.ts"),
            &format!(
                "import {{ clamp, mono }} from '../engine/panel';\n\nexport function draw{i}(v: number): number {{\n  return clamp(v, 0, 1) + mono(v);\n}}\n"
            ),
        );
    }
    commit_all(repo, "initial");
    tmp
}

#[test]
fn a_changed_arrow_function_const_has_its_importers_as_callers() {
    // `export const clamp = (…) => …` was a `variable` fragment, not a
    // definition, and the answer read "Nothing outside the diff" (#324).
    let tmp = ts_repo_with_panel();
    let repo = tmp.path();
    write(
        repo,
        "src/engine/panel.ts",
        "export const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));\n\nexport function mono(x: number): number {\n  return x * 2;\n}\n",
    );
    commit_all(repo, "clamp");
    let doc = impact(repo, "HEAD~1..HEAD");
    let clamp = doc["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["symbol"] == "clamp")
        .unwrap_or_else(|| panic!("clamp is a changed symbol: {doc}"));
    assert_eq!(
        clamp["callers"].as_array().expect("callers").len(),
        3,
        "{doc}"
    );
}

#[test]
fn a_body_only_change_of_an_export_is_not_a_contract() {
    // A contract is the exported surface; a body edit is what the callers
    // list already says (#325). The signature edit right after it is one.
    let tmp = ts_repo_with_panel();
    let repo = tmp.path();
    write(
        repo,
        "src/engine/panel.ts",
        "export const clamp = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));\n\nexport function mono(x: number): number {\n  return x * 3;\n}\n",
    );
    commit_all(repo, "mono body");
    let contracts = |doc: &serde_json::Value| -> Vec<String> {
        doc.get("contracts")
            .and_then(|c| c.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c["symbol"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let body = impact(repo, "HEAD~1..HEAD");
    assert!(!contracts(&body).contains(&"mono".to_string()), "{body}");
    assert!(
        body["changed"]
            .as_array()
            .expect("changed")
            .iter()
            .any(|c| c["symbol"] == "mono"),
        "the callers still name it: {body}"
    );
    write(
        repo,
        "src/engine/panel.ts",
        "export const clamp = (v: number, a: number, b: number) => Math.min(b, Math.max(a, v));\n\nexport function mono(x: number, k = 3): number {\n  return x * k;\n}\n",
    );
    commit_all(repo, "mono signature");
    let signature = impact(repo, "HEAD~1..HEAD");
    assert!(
        contracts(&signature).contains(&"mono".to_string()),
        "{signature}"
    );
}

#[test]
fn a_decorated_export_whose_signature_moved_is_a_contract() {
    // The decorator belongs to the fragment: `export` was not at its head, so
    // the class was not public, and a `{` in the decorator cut the head
    // before the signature.
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "src/panel.ts",
        "@Component({ selector: 'panel' })\nexport class Panel {\n  draw(): number {\n    return 1;\n  }\n}\n",
    );
    write(
        repo,
        "src/app.ts",
        "import { Panel } from './panel';\n\nexport function boot(): Panel {\n  return new Panel();\n}\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "src/panel.ts",
        "@Component({ selector: 'panel' })\nexport class Panel<T> {\n  draw(): number {\n    return 1;\n  }\n}\n",
    );
    commit_all(repo, "panel: generic");
    let doc = impact(repo, "HEAD~1..HEAD");
    let contracts: Vec<&str> = doc["contracts"]
        .as_array()
        .unwrap_or_else(|| panic!("no contracts: {doc}"))
        .iter()
        .filter_map(|c| c["symbol"].as_str())
        .collect();
    assert!(contracts.contains(&"Panel"), "{doc}");
}

#[test]
fn a_file_that_only_imports_the_symbol_is_not_a_caller() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/legacy.py",
        "\"\"\"Old surface, kept for one release.\"\"\"\n\nfrom shop.pricing import total  # noqa: F401\n",
    );
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "legacy import"]);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    git(repo, &["commit", "-q", "-am", "vat again"]);
    let doc = impact(repo, "HEAD~1..HEAD");
    let total = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("total listed");
    let paths: Vec<&str> = total["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"shop/checkout.py"), "{paths:?}");
    assert!(
        !paths.contains(&"shop/legacy.py"),
        "an import-only module is not a call site: {paths:?}"
    );
}

/// A one-line edit deep inside a Rust function body seeds a nested
/// fragment (the `let`), not the function; the callers still belong to the
/// function.
#[test]
fn an_edit_inside_a_function_body_still_names_the_functions_callers() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "Cargo.toml",
        "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(repo, "src/lib.rs", "pub mod pricing;\npub mod checkout;\n");
    write(
        repo,
        "src/pricing.rs",
        "pub fn total(items: &[f64]) -> f64 {\n    let vat = 1.0;\n    let sum: f64 = items.iter().sum();\n    sum * vat\n}\n",
    );
    write(
        repo,
        "src/checkout.rs",
        "use crate::pricing::total;\n\npub fn charge(items: &[f64]) -> f64 {\n    total(items) + 1.0\n}\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "src/pricing.rs",
        "pub fn total(items: &[f64]) -> f64 {\n    let vat = 1.19;\n    let sum: f64 = items.iter().sum();\n    sum * vat\n}\n",
    );
    commit_all(repo, "vat");
    let doc = impact(repo, "HEAD~1..HEAD");
    let total = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "total")
        .unwrap_or_else(|| panic!("the function, not its inner let, is the changed symbol: {doc}"));
    let callers: Vec<&str> = total["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["symbol"].as_str().unwrap_or(""))
        .collect();
    assert!(callers.contains(&"charge"), "{doc}");
}

#[test]
fn the_text_form_names_every_caller_and_its_guard() {
    let tmp = repo_with_callers();
    let text = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(text.contains("shop/checkout.py::charge"), "{text}");
    assert!(
        text.contains("reachable from tests: tests/test_checkout.py"),
        "{text}"
    );
    assert!(text.contains("shop/report.py::summarize"), "{text}");
    // No test reaches shop/report.py at all: absence of evidence, not a
    // finding (#348). "no static test link (its file has tests)" is kept for a tested file whose caller no test
    // reaches.
    assert!(
        text.contains("summarize (4-5) — no static test link found"),
        "{text}"
    );
    assert!(
        text.lines().count() <= 25,
        "the hook injects this before a commit; {} lines is not readable there",
        text.lines().count()
    );
}

#[test]
fn a_change_nothing_depends_on_is_empty() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(repo, "README.md", "# shop\n\nTotals include VAT.\n");
    commit_all(repo, "docs");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert_eq!(doc["empty"], true, "{doc}");
    assert!(doc["changed"].as_array().unwrap().is_empty());
    let text = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(
        text.contains("No resolved static callers outside the diff in the analysed scope."),
        "{text}"
    );
}

#[test]
fn a_symbol_edited_by_two_commits_of_the_range_says_so() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "VAT = 1.19\n\n\ndef total(items):\n    return round(sum(i.price for i in items) * VAT, 2)\n",
    );
    commit_all(repo, "pricing: name the rate");
    let doc = impact(repo, "HEAD~2..HEAD");
    assert_eq!(doc["commit_count"], 2);
    let total = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("total listed");
    assert_eq!(total["commits"], 2, "{total}");
    let text = impact_markdown(repo, "HEAD~2..HEAD");
    assert!(text.contains("touched by 2 commits"), "{text}");
}

#[test]
fn the_working_tree_is_a_valid_range() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["range"], "HEAD");
    assert_eq!(doc["empty"], false);
    assert!(
        doc.get("commit_count").is_none(),
        "no commits for a dirty tree"
    );
}

#[test]
fn the_answer_fits_the_cap_and_says_what_it_dropped() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "svc/core.py", "def core(x):\n    return x\n");
    // Under the Python builder's 64-file confirmation cap a wider fan-out keeps
    // only import lines; sixty long-named callers are enough to pass the cap.
    for i in 0..60 {
        write(
            repo,
            &format!("callers/caller_{i:03}.py"),
            &format!(
                "from svc.core import core\n\n\ndef use_the_core_value_in_the_report_row_{i:03}(v):\n    return core(v) + {i}\n"
            ),
        );
    }
    commit_all(repo, "initial");
    write(repo, "svc/core.py", "def core(x):\n    return x * 2\n");
    commit_all(repo, "double");
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", "HEAD~1..HEAD", "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let json = String::from_utf8(out.stdout).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(
        _diffctx::tokenizer::count_tokens(&json) <= _diffctx::impact::IMPACT_TOKEN_CAP,
        "{} tokens",
        _diffctx::tokenizer::count_tokens(&json)
    );
    assert!(doc["truncated"].as_u64().unwrap() > 0, "{doc}");
    let listed = doc["changed"][0]["callers"].as_array().unwrap().len();
    assert!(listed > 5, "kept the strongest callers, not none: {listed}");
}

#[test]
fn md_is_rejected_outside_impact_mode() {
    let tmp = repo_with_callers();
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args(["--diff", "HEAD~1..HEAD", "-f", "md"])
        .output()
        .expect("run diffctx");
    assert_eq!(out.status.code(), Some(2));
}

/// #345: a name inside a string literal is not a call, and `re.search` is not
/// a call to a changed module-level `search` — but `download.search` and an
/// aliased `dl.refused` are.
#[test]
fn strings_and_foreign_receivers_are_not_callers() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "fetcher/__init__.py", "");
    write(
        repo,
        "fetcher/download.py",
        "def refused(url):\n    return url.startswith('x')\n\n\ndef search(query):\n    return [query]\n",
    );
    write(
        repo,
        "apps/ml/test_model_cache.py",
        "def download_model_files():\n    raise AssertionError(\"an unpinned model must be refused before it is fetched\")\n",
    );
    write(
        repo,
        "tests/test_download.py",
        "import re\nfrom fetcher import download\n\n\ndef test_release_matches():\n    assert re.search(r\"v1\", \"v1.2\")\n\n\ndef test_search_finds():\n    assert download.search(\"q\") == [\"q\"]\n",
    );
    write(
        repo,
        "fetcher/api.py",
        "import fetcher.download as dl\n\n\ndef handle(url):\n    # refused() is checked first, 2× per call\n    label = \"× size\"\n    return dl.refused(url)\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "fetcher/download.py",
        "def refused(url):\n    return url.startswith('x') or not url\n\n\ndef search(query):\n    return [query.strip()]\n",
    );
    commit_all(repo, "tighten");

    let doc = impact(repo, "HEAD~1..HEAD");
    let callers = |sym: &str| -> Vec<String> {
        doc["changed"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["symbol"] == sym)
            .flat_map(|c| c["callers"].as_array().unwrap().clone())
            .map(|c| {
                format!(
                    "{}::{}",
                    c["path"].as_str().unwrap(),
                    c["symbol"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    assert_eq!(
        callers("search"),
        vec!["tests/test_download.py::test_search_finds"],
        "{doc:#}"
    );
    assert_eq!(
        callers("refused"),
        vec!["fetcher/api.py::handle"],
        "{doc:#}"
    );
}

/// #351: `from models import Steps` reaches `models/wearables.py::Steps`
/// through the package index; the class passed as a value is a reference.
#[test]
fn a_package_reexport_reaches_the_defining_module() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "models/__init__.py",
        "from .wearables import Steps\n\n__all__ = [\"Steps\"]\n",
    );
    write(
        repo,
        "models/wearables.py",
        "class Steps(Base):\n    total_distance = Column(Float, default=0)\n",
    );
    write(
        repo,
        "loaders/normalize.py",
        "from models import Steps\n\n\ndef upsert_canonical_steps(db, row, user_id):\n    return upsert_data(db, Steps, row, \"date\", user_id)\n",
    );
    write(
        repo,
        "reports/summary.py",
        "from models import Sleep\n\n\ndef summarize(db):\n    return [Sleep, Steps]\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "models/wearables.py",
        "class Steps(Base):\n    total_distance = Column(Float)\n",
    );
    commit_all(repo, "no default");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(
        md.contains("loaders/normalize.py::upsert_canonical_steps"),
        "{md}"
    );
    assert!(
        !md.contains("reports/summary.py"),
        "importing another name from the package is not a reference: {md}"
    );
}

/// #348: a helper tested through the public function that calls it is
/// tested; a caller in a tested file that no test reaches says its file has tests.
#[test]
fn a_test_reaches_a_caller_through_the_function_it_calls() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "lib/core.py", "def core(v):\n    return v\n");
    write(
        repo,
        "app/tools.py",
        "from lib.core import core\n\n\ndef helper(v):\n    return core(v)\n\n\ndef public_tool(v):\n    return helper(v) + 1\n\n\ndef orphan(v):\n    return core(v) - 1\n",
    );
    write(
        repo,
        "tests/test_tools.py",
        "from app.tools import public_tool\n\n\ndef test_tool():\n    assert public_tool(1) == 2\n",
    );
    commit_all(repo, "base");
    write(repo, "lib/core.py", "def core(v):\n    return v * 2\n");
    commit_all(repo, "double");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(
        md.contains(
            "app/tools.py::helper (4-5) — reachable from tests: tests/test_tools.py via public_tool"
        ),
        "{md}"
    );
    assert!(
        md.contains("app/tools.py::orphan (12-13) — no static test link (its file has tests)"),
        "{md}"
    );
}

/// #347/#313: a deletion-only change still counts its files, and every line
/// that still names the deleted path is the impact.
#[test]
fn a_deleted_file_still_named_elsewhere_is_reported() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "scripts/build_image.sh",
        "#!/bin/sh\ndocker build .\n",
    );
    write(
        repo,
        "Dockerfile",
        "FROM alpine\nCOPY scripts/build_image.sh /app/\n",
    );
    write(repo, "Makefile", "image:\n\t./scripts/build_image.sh\n");
    write(repo, "app.py", "def main():\n    return 1\n");
    commit_all(repo, "base");
    git(repo, &["rm", "-q", "scripts/build_image.sh"]);
    commit_all(repo, "drop the script");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert_eq!(
        doc["deleted_files"],
        serde_json::json!(["scripts/build_image.sh"]),
        "{doc:#}"
    );
    let stale: Vec<String> = doc["stale_references"]
        .as_array()
        .expect("stale references")
        .iter()
        .map(|r| format!("{}:{}", r["path"].as_str().unwrap(), r["line"]))
        .collect();
    assert_eq!(stale, vec!["Dockerfile:2", "Makefile:2"], "{doc:#}");
    assert_eq!(doc["empty"], false);
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(md.contains("1 changed file(s) (1 deleted)"), "{md}");
    assert!(
        md.contains("Dockerfile:2 names scripts/build_image.sh (deleted)"),
        "{md}"
    );
}

#[test]
fn a_rename_its_importer_missed_is_reported_and_the_tree_is_labelled() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    git(repo, &["mv", "shop/pricing.py", "shop/prices.py"]);
    let md = impact_markdown(repo, "HEAD");
    assert!(
        md.starts_with("diffctx impact for uncommitted changes:"),
        "{md}"
    );
    assert!(
        md.contains("shop/checkout.py:1 names shop/pricing.py (renamed to shop/prices.py)"),
        "{md}"
    );
}

/// #319: reverse discovery reads Go `func` (with a receiver) and Java
/// methods; a local `const` shared with an unrelated file is not a definition.
#[test]
fn reverse_discovery_reads_funcs_and_methods_not_locals() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "go.mod", "module example.com/app\n\ngo 1.22\n");
    write(
        repo,
        "pkg/billing/ledger.go",
        "package billing\n\ntype Ledger struct{ rate int }\n\nfunc (l *Ledger) Reconcile(total int) int {\n\treturn total * l.rate\n}\n",
    );
    write(
        repo,
        "cmd/report/main.go",
        "package main\n\nimport \"example.com/app/pkg/billing\"\n\nfunc main() {\n\tl := &billing.Ledger{}\n\t_ = l.Reconcile(3)\n}\n",
    );
    write(
        repo,
        "src/main/java/app/Pricing.java",
        "package app;\n\npublic class Pricing {\n    public static int quoteFor(int n) {\n        return n * 2;\n    }\n}\n",
    );
    write(
        repo,
        "src/main/java/app/Checkout.java",
        "package app;\n\npublic class Checkout {\n    int pay(int n) {\n        return Pricing.quoteFor(n);\n    }\n}\n",
    );
    write(
        repo,
        "web/panel.ts",
        "export function render(rows: number[]): number {\n  const accumulated = rows.length;\n  return accumulated;\n}\n",
    );
    write(
        repo,
        "web/unrelated.ts",
        "export function other(): number {\n  const accumulated = 1;\n  return accumulated;\n}\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "pkg/billing/ledger.go",
        "package billing\n\ntype Ledger struct{ rate int }\n\nfunc (l *Ledger) Reconcile(total int) int {\n\treturn total*l.rate + 1\n}\n",
    );
    write(
        repo,
        "src/main/java/app/Pricing.java",
        "package app;\n\npublic class Pricing {\n    public static int quoteFor(int n) {\n        return n * 3;\n    }\n}\n",
    );
    write(
        repo,
        "web/panel.ts",
        "export function render(rows: number[]): number {\n  const accumulated = rows.length + 1;\n  return accumulated;\n}\n",
    );
    commit_all(repo, "change");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(md.contains("cmd/report/main.go::main"), "{md}");
    assert!(md.contains("src/main/java/app/Checkout.java"), "{md}");
    assert!(!md.contains("web/unrelated.ts"), "{md}");
}

fn header(md: &str) -> &str {
    md.lines().next().unwrap_or_default()
}

fn repo_of(files: &[(&str, &str)], changes: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for (p, c) in files {
        write(repo, p, c);
    }
    commit_all(repo, "initial");
    for (p, c) in changes {
        write(repo, p, c);
    }
    if !changes.is_empty() {
        commit_all(repo, "change");
    }
    tmp
}

fn changed_symbols(doc: &serde_json::Value) -> Vec<String> {
    doc["changed"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c["symbol"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn contract_symbols(doc: &serde_json::Value) -> Vec<String> {
    doc.get("contracts")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        c["kind"].as_str().unwrap_or(""),
                        c["symbol"]
                            .as_str()
                            .unwrap_or(c["path"].as_str().unwrap_or(""))
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// #386: the header and the per-line guard come from one predicate.
#[test]
fn the_header_counts_every_caller_the_lines_call_untested() {
    let tmp = repo_with_callers();
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("no static test link found"), "{md}");
    assert!(
        header(&md).contains(", 1 caller(s) without a static test link"),
        "{md}"
    );
}

/// Owner request on #355: a changed definition no test reaches is counted.
#[test]
fn a_changed_definition_no_test_reaches_is_counted() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/report.py",
        "from shop.pricing import total\n\n\ndef summarize(orders):\n    return [round(total(o.items)) for o in orders]\n",
    );
    commit_all(repo, "report rounds");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(
        header(&md).contains("1 changed definition(s) without a static test link"),
        "{md}"
    );
    // #493: nothing calls it, so no untested caller is counted beside it.
    assert_eq!(
        header(&md).matches("without a static test link").count(),
        1,
        "{md}"
    );
    // #398: what the header counts, the answer names.
    assert!(md.contains("  - shop/report.py::summarize ("), "{md}");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert_eq!(
        doc["untested_unlisted"][0]
            .as_str()
            .map(|s| s.starts_with("shop/report.py::summarize")),
        Some(true),
        "{doc:#}"
    );
}

/// #441: a helper whose only caller is a definition the same diff changes is
/// live code. It is named as called inside the diff, never as one nothing
/// calls; the definition nothing calls stays in that clause.
#[test]
fn a_definition_called_only_inside_the_diff_is_not_one_nothing_calls() {
    let rates = |k: u32| {
        format!(
            "def _scale(x):\n    return x * {k}\n\n\ndef quote(x):\n    return _scale(x) + {k}\n"
        )
    };
    let tmp = repo_of(
        &[
            ("app/__init__.py", ""),
            ("app/rates.py", &rates(2)),
            ("app/other.py", "def ping():\n    return 1\n"),
            (
                "tests/test_other.py",
                "from app.other import ping\n\n\ndef test_ping():\n    assert ping() == 1\n",
            ),
        ],
        &[("app/rates.py", &rates(3))],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let names = |key: &str| -> Vec<String> {
        doc[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.split(" (").next().unwrap_or(s).to_string())
                    .collect()
            })
            .unwrap_or_default()
    };
    assert_eq!(
        names("untested_unlisted"),
        ["app/rates.py::quote"],
        "{doc:#}"
    );
    assert_eq!(
        names("untested_called_in_diff"),
        ["app/rates.py::_scale"],
        "{doc:#}"
    );
    assert_eq!(doc["untested_definitions"], 2, "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    let (nothing, inside) = md
        .split_once("Changed, called only from inside the diff, no static test link:")
        .unwrap_or_else(|| panic!("{md}"));
    assert!(nothing.contains("  - app/rates.py::quote ("), "{md}");
    assert!(!nothing.contains("_scale"), "{md}");
    assert!(inside.contains("  - app/rates.py::_scale ("), "{md}");
}

/// The withhold policy holds at the read, so no output surface can name a
/// withheld file: a canary under `.diffctx/ignore` and a secret-named one
/// call every changed and removed name and name the deleted path. Before
/// the policy moved into the `git grep` readers, impact printed both canaries
/// as callers and stale-reference holders while pack and tree mode hid them.
#[test]
fn no_mode_names_a_withheld_file_that_references_the_change() {
    let canary = "from lib import kept, gone\n# see scripts/rotate.sh\nkept(); gone()\n";
    let tmp = repo_of(
        &[
            (".diffctx/ignore", "private/\n"),
            (
                "lib.py",
                "def kept():\n    return 1\n\n\ndef gone():\n    return 2\n",
            ),
            ("scripts/rotate.sh", "echo rotate\n"),
            (
                "app.py",
                "from lib import kept\n\n\ndef run():\n    return kept()\n",
            ),
            ("private/payroll_canary.py", canary),
            ("config/canary.key", canary),
        ],
        &[("lib.py", "def kept():\n    return 3\n")],
    );
    let repo = tmp.path();
    std::fs::remove_file(repo.join("scripts/rotate.sh")).expect("rm");
    commit_all(repo, "drop gone and the script");
    for args in [
        &["--diff", "HEAD~1..HEAD", "--mode", "impact", "-q"][..],
        &[
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            "md",
        ],
        &["--symbol", "kept", "-q", "-f", "md"],
        &["--symbol", "kept", "-q"],
        &["--diff", "HEAD~1..HEAD", "-q", "-f", "json"],
        &["--diff", "HEAD~1..HEAD", "--mode", "locate", "-q"],
        &["--diff", "HEAD~1..HEAD", "--full", "-q", "-f", "md"],
        &["-q", "-f", "json"],
    ] {
        let out = Command::new(&*BIN)
            .current_dir(repo)
            .arg(".")
            .args(args)
            .env("DIFFCTX_NO_MARKER", "1")
            .output()
            .expect("run diffctx");
        let text = String::from_utf8_lossy(&out.stdout);
        for leaked in ["payroll_canary", "canary.key"] {
            assert!(!text.contains(leaked), "{args:?} names {leaked}:\n{text}");
        }
    }
    let doc = impact(repo, "HEAD~1..HEAD");
    assert!(
        doc["withheld_files"].as_u64().unwrap_or(0) >= 1,
        "the withheld referrers are counted: {doc:#}"
    );
}

/// #446: the per-node edge cap keeps a fragment's heaviest out-edges, so a
/// caller that references many names can lose its edge to the changed
/// definition, and the answer read "0 callers". A capped fragment naming the
/// definition faces the same tests an edge would have, and the caller stays.
#[test]
fn a_caller_the_edge_cap_cut_is_still_a_caller() {
    let target = |k: u32| format!("def target(x):\n    return x * {k}\n");
    let mut files: Vec<(String, String)> = ["alpha", "beta", "gamma", "delta"]
        .iter()
        .map(|n| (format!("app/{n}.py"), format!("def {n}():\n    return 1\n")))
        .collect();
    files.push(("app/__init__.py".into(), String::new()));
    files.push(("app/target.py".into(), target(2)));
    files.push((
        "app/run.py".into(),
        "from app.alpha import alpha\nfrom app.beta import beta\nfrom app.gamma import gamma\n\
         from app.delta import delta\nfrom app.target import target\n\n\n\
         def run():\n    return alpha() + beta() + gamma() + delta() + target(3)\n"
            .into(),
    ));
    let initial: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let changed = target(3);
    let tmp = repo_of(&initial, &[("app/target.py", &changed)]);
    let callers = |doc: &serde_json::Value| -> Vec<String> {
        doc["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["symbol"] == "target")
            .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
            .map(|c| {
                format!(
                    "{}::{}",
                    c["path"].as_str().unwrap_or(""),
                    c["symbol"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    let full = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(callers(&full), ["app/run.py::run"], "{full:#}");
    let cut = impact_env(
        tmp.path(),
        "HEAD~1..HEAD",
        &[("DIFFCTX_MAX_EDGES_PER_NODE", "1")],
    );
    assert_eq!(callers(&cut), ["app/run.py::run"], "{cut:#}");
}

/// A changed file over the size cap was never read: its definitions, their
/// callers and its contracts are unknown, and the answer said
/// `"empty": true`, "No resolved static callers", while pack on the same
/// range reported `file_too_large`.
#[test]
fn a_changed_file_over_the_size_cap_is_named_never_empty() {
    let body = |k: u32| {
        let mut text = format!("def target(x):\n    return x * {k}\n\n\n");
        let filler = "# padding line that keeps the file over the changed-file size cap\n";
        while text.len() < 5_100_000 {
            text.push_str(filler);
        }
        text
    };
    let (before, after) = (body(2), body(3));
    let tmp = repo_of(
        &[
            ("big.py", &before),
            (
                "main.py",
                "from big import target\n\n\ndef run():\n    return target(1)\n",
            ),
        ],
        &[("big.py", &after)],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(doc["empty"], false, "{doc:#}");
    assert_eq!(doc["unanalysed_files"][0]["path"], "big.py", "{doc:#}");
    assert_eq!(doc["unanalysed_files"][0]["reason"], "too_large", "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("No resolved static callers"), "{md}");
    assert!(md.contains("over the size cap"), "{md}");
}

/// #498: a comment-only edit is skipped on purpose (a comment changes
/// nothing a caller sees, #370), and the answer says that, instead of
/// "No resolved static callers" about a function with a caller.
#[test]
fn a_comment_only_edit_says_its_callers_were_not_traced() {
    let tmp = repo_of(
        &[
            (
                "pay.py",
                "def load_x(session, user_id):\n    total = 1\n    return total\n",
            ),
            (
                "view.py",
                "from pay import load_x\n\n\ndef show():\n    return load_x(None, 1)\n",
            ),
        ],
        &[(
            "pay.py",
            "def load_x(session, user_id):\n    # rounded later\n    total = 1\n    return total\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let layout: Vec<&str> = doc["layout_only"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(layout, ["pay.py::load_x (1-4)"], "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("No resolved static callers"), "{md}");
    assert!(md.contains("callers not traced"), "{md}");
}

/// A change with 60 callers and a schema migration, past the default cap.
fn wide_change() -> TempDir {
    let mut files: Vec<(String, String)> = (0..60)
        .map(|i| {
            (
                format!("app/c{i}.py"),
                format!("from app.pricing import total\n\n\ndef f{i}(x):\n    return total(x)\n"),
            )
        })
        .collect();
    files.push(("app/__init__.py".into(), String::new()));
    files.push((
        "app/pricing.py".into(),
        "def total(items):\n    return sum(items)\n".into(),
    ));
    files.push((
        "db/migrations/0001_init.sql".into(),
        "CREATE TABLE t (id int);\n".into(),
    ));
    let initial: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    repo_of(
        &initial,
        &[
            (
                "app/pricing.py",
                "def total(items):\n    return sum(items) * 2\n",
            ),
            (
                "db/migrations/0001_init.sql",
                "CREATE TABLE t (id int, n int);\n",
            ),
        ],
    )
}

fn omitted_in(md: &str) -> u64 {
    md.lines()
        .find_map(|l| l.strip_prefix('(')?.split_once(" weaker caller(s) omitted"))
        .and_then(|(n, _)| n.parse().ok())
        .unwrap_or(0)
}

/// #451: markdown is shed against its own size, not the JSON's, so it keeps
/// the callers and the contract block the JSON measure dropped.
#[test]
fn markdown_is_fitted_on_markdown_and_keeps_its_contracts() {
    let tmp = wide_change();
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let json_shed = doc["truncated"].as_u64().unwrap_or(0);
    assert!(json_shed > 0, "the fixture must pass the cap: {doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(omitted_in(&md) < json_shed, "{md}");
    assert!(md.contains("Contracts changed:"), "{md}");
    assert!(md.contains("db/migrations/0001_init.sql"), "{md}");
}

/// #466: `--budget` sets the cap, and a shed answer names the flag.
#[test]
fn the_budget_sets_the_impact_cap() {
    let tmp = wide_change();
    let small = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            "md",
            "--budget",
            "300",
        ])
        .output()
        .expect("run diffctx");
    let md = String::from_utf8_lossy(&small.stdout);
    assert!(omitted_in(&md) > 0, "{md}");
    assert!(md.contains("a larger budget (--budget"), "{md}");
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "--budget",
            "20000",
        ])
        .output()
        .expect("run diffctx");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("impact JSON");
    assert!(doc.get("truncated").is_none(), "{doc:#}");
    assert_eq!(
        doc["changed"][0]["callers"].as_array().map(Vec::len),
        Some(60),
        "{doc:#}"
    );
}

/// #413, #457: a removed definition named like a common word is read from a
/// bounded grep and answered as unresolved, not traced through every line
/// that spells the word (on a monorepo that never finished).
#[test]
fn a_removed_name_too_common_to_trace_is_unresolved_not_listed() {
    let mut files: Vec<(String, String)> = (0..40)
        .map(|i| {
            (
                format!("app/m{i}.py"),
                (0..20).map(|j| format!("x{j} = obj.call({j})\n")).collect(),
            )
        })
        .collect();
    files.push((
        "app/defs.py".into(),
        "def call(x):\n    return x\n\n\ndef keep():\n    return 1\n".into(),
    ));
    let initial: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let tmp = repo_of(&initial, &[("app/defs.py", "def keep():\n    return 1\n")]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let removed: Vec<&serde_json::Value> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "call")
        .collect();
    assert_eq!(removed.len(), 1, "{doc:#}");
    assert!(
        removed[0]["unresolved"]
            .as_str()
            .is_some_and(|u| u.contains("too common to trace")),
        "{doc:#}"
    );
    assert!(
        removed[0]["callers"].as_array().is_none_or(Vec::is_empty),
        "{doc:#}"
    );
}

/// #495: a dependency bump is a change: the lock files are named and the
/// answer is never empty, where it read "No changes to analyse".
#[test]
fn a_lock_file_change_is_named_never_empty() {
    let tmp = repo_of(
        &[
            ("app.py", "def f():\n    return 1\n"),
            ("uv.lock", "x==1\n"),
            ("Cargo.lock", "[package]\nname = \"x\"\n"),
        ],
        &[
            ("uv.lock", "x==2\n"),
            ("Cargo.lock", "[package]\nname = \"y\"\n"),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(doc["empty"], false, "{doc:#}");
    let locks: Vec<&str> = doc["lockfile_changes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(locks, ["Cargo.lock", "uv.lock"], "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("No changes to analyse"), "{md}");
    assert!(md.contains("  - uv.lock"), "{md}");
}

/// #387: callers that could not be resolved are not "0 callers".
#[test]
fn unresolved_callers_are_not_reported_as_none() {
    let def = |n: u32| format!("pub fn build() -> u32 {{\n    {n}\n}}\n");
    let tmp = repo_of(
        &[
            ("src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\n"),
            ("src/a.rs", &def(1)),
            ("src/b.rs", &def(2)),
            ("src/c.rs", &def(3)),
        ],
        &[("src/a.rs", &def(10))],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(header(&md).contains("callers unresolved"), "{md}");
    assert!(!header(&md).contains(" 0 caller(s)"), "{md}");
}

/// A file `.diffctx/ignore` withholds is never named by impact: not by a
/// definition its hunk removed, not as a deleted file.
#[test]
fn a_withheld_file_is_never_named_by_its_removed_definitions_or_deletion() {
    let tmp = repo_of(
        &[
            (".diffctx/ignore", "internal/\n"),
            (
                "internal/acquisition.js",
                "export function priceForAcmeTakeover(x) {\n  return x * 2;\n}\n\nexport function other(x) {\n  return x;\n}\n",
            ),
            ("internal/plan.js", "export const plan = 1;\n"),
            (
                "src/app.js",
                "import { priceForAcmeTakeover } from '../internal/acquisition.js';\nexport function run(x) {\n  return priceForAcmeTakeover(x);\n}\n",
            ),
            (
                "src/util.js",
                "export function util(x) {\n  return x + 2;\n}\n",
            ),
        ],
        &[
            (
                "internal/acquisition.js",
                "export function other(x) {\n  return x;\n}\n",
            ),
            (
                "src/util.js",
                "export function util(x) {\n  return x + 3;\n}\n",
            ),
        ],
    );
    let repo = tmp.path();
    std::fs::remove_file(repo.join("internal/plan.js")).expect("rm");
    commit_all(repo, "drop the plan");
    for range in ["HEAD~2..HEAD~1", "HEAD~1..HEAD", "HEAD~2..HEAD"] {
        let doc = impact(repo, range);
        let text = doc.to_string();
        assert!(!text.contains("internal/"), "{range}: {doc:#}");
        assert!(!text.contains("priceForAcmeTakeover"), "{range}: {doc:#}");
        let md = impact_markdown(repo, range);
        assert!(!md.contains("internal/"), "{range}: {md}");
    }
}

/// git quotes a non-ASCII path in a diff header; the quoted spelling used to
/// match no file, so a removal there reported nothing.
#[test]
fn a_definition_removed_from_a_non_ascii_path_is_reported() {
    let tmp = repo_of(
        &[
            (
                "src/caf\u{e9}.js",
                "export function price(x) {\n  return x * 2;\n}\n\nexport function legacyPrice(x) {\n  return x * 3;\n}\n",
            ),
            (
                "src/shop.js",
                "import { price, legacyPrice } from './caf\u{e9}.js';\nexport function total(x) {\n  return price(x) + legacyPrice(x);\n}\n",
            ),
        ],
        &[(
            "src/caf\u{e9}.js",
            "export function price(x) {\n  return x * 4;\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        changed_symbols(&doc).contains(&"legacyPrice".to_string()),
        "{doc:#}"
    );
    assert!(
        contract_symbols(&doc).contains(&"removed_api legacyPrice".to_string()),
        "{doc:#}"
    );
}

/// A statement moved out of its `if` is a change, whatever `diff.context`
/// the user configured: with it at 0 the hunk lost the lines that show the
/// move and read as layout.
#[test]
fn a_dedented_statement_is_a_change_under_any_diff_context() {
    let core = |body: &str| {
        format!(
            "import os\n\n\ndef charge(user, amount):\n    if user.blocked:\n        log(user)\n{body}    return amount\n\n\ndef log(u):\n    print(u)\n\n\ndef refund(u, a):\n    print(u, a)\n"
        )
    };
    let tmp = repo_of(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/core.py", &core("        refund(user, amount)\n")),
            (
                "pkg/app.py",
                "from pkg.core import charge\n\n\ndef handler(req):\n    return charge(req.user, req.amount)\n",
            ),
        ],
        &[("pkg/core.py", &core("    refund(user, amount)\n"))],
    );
    git(tmp.path(), &["config", "diff.context", "0"]);
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("called from pkg/app.py::handler"), "{md}");
}

/// Deleting a call statement from a method removes no member.
#[test]
fn a_deleted_call_inside_a_method_is_no_removed_member() {
    let form = |body: &str| {
        format!(
            "import {{ validate }} from './validate.js';\n\nexport class Form {{\n  submit(x) {{\n    validate(x);\n{body}    return x;\n  }}\n}}\n"
        )
    };
    let tmp = repo_of(
        &[
            ("src/form.js", &form("    notify(x);\n")),
            (
                "src/notify.js",
                "export function warnUser(m) {\n  notify(m);\n}\n",
            ),
            (
                "src/validate.js",
                "export function validate(x) {\n  return x;\n}\n",
            ),
        ],
        &[("src/form.js", &form(""))],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        !changed_symbols(&doc).contains(&"notify".to_string()),
        "{doc:#}"
    );
    assert!(contract_symbols(&doc).is_empty(), "{doc:#}");
}

/// A control statement removed from a method body declares nothing:
/// `match (a, b) {` inside a Rust `impl` read as a removed member `match`,
/// and every file spelling the keyword became its caller.
#[test]
fn a_removed_control_statement_is_no_removed_member() {
    let shape = |body: &str| {
        format!(
            "pub struct Shape;\n\nimpl Shape {{\n    pub fn area(&self, a: u32, b: u32) -> u32 {{\n{body}        a * b\n    }}\n}}\n"
        )
    };
    let tmp = repo_of(
        &[
            (
                "src/shape.rs",
                &shape(
                    "        match (a, b) {\n            (0, _) => return 0,\n            _ => {}\n        }\n",
                ),
            ),
            (
                "src/other.rs",
                "pub fn pick(x: u32) -> u32 {\n    match (x, 1) {\n        (0, _) => 0,\n        _ => 1,\n    }\n}\n",
            ),
        ],
        &[("src/shape.rs", &shape(""))],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        !changed_symbols(&doc).contains(&"match".to_string()),
        "{doc:#}"
    );
}

/// Deleting the function right under a caller leaves that caller outside
/// the diff: a pure removal is anchored on the line above it.
#[test]
fn a_caller_right_above_a_deleted_function_is_still_a_caller() {
    let core = |rate: &str, tail: &str| {
        format!(
            "def rate(amount):\n    return amount * {rate}\n\n\ndef invoice(amount):\n    total = rate(amount)\n    return total\n{tail}\n\ndef last():\n    return 1\n"
        )
    };
    let tmp = repo_of(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/core.py", &core("2", "def obsolete():\n    return 0\n")),
        ],
        &[("pkg/core.py", &core("3", ""))],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("pkg/core.py::invoice"), "{md}");
}

/// Lines removed from inside a definition are its change at any
/// indentation: a dedented SQL string reads like a deletion beside it.
#[test]
fn a_column_zero_line_removed_inside_a_body_is_a_change() {
    let users = |filter: &str| {
        format!(
            "def active_users(db):\n    return db.query(\"\"\"\nSELECT id FROM users\n{filter}ORDER BY id\n\"\"\")\n"
        )
    };
    let tmp = repo_of(
        &[
            ("app/__init__.py", ""),
            ("app/queries.py", &users("WHERE active\n")),
            (
                "app/report.py",
                "from app.queries import active_users\n\n\ndef monthly(db):\n    return len(active_users(db))\n",
            ),
        ],
        &[("app/queries.py", &users(""))],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("called from app/report.py::monthly"), "{md}");
}

/// A Rust function moved to another module: the call still naming the old
/// module is broken, the one naming the new module is not.
#[test]
fn a_rust_call_still_naming_the_old_module_of_a_move_is_reported() {
    let rate = "pub fn tax_rate() -> u32 {\n    19\n}\n";
    let tmp = repo_of(
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"k\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "pub mod old;\npub mod new;\npub mod main_use;\npub mod fixed;\n",
            ),
            ("src/old.rs", rate),
            ("src/new.rs", "pub fn other() -> u32 {\n    1\n}\n"),
            (
                "src/main_use.rs",
                "pub fn price() -> u32 {\n    crate::old::tax_rate() * 2\n}\n",
            ),
            (
                "src/fixed.rs",
                "pub fn cost() -> u32 {\n    crate::new::tax_rate()\n}\n",
            ),
        ],
        &[
            ("src/old.rs", "\n"),
            (
                "src/new.rs",
                "pub fn other() -> u32 {\n    1\n}\n\npub fn tax_rate() -> u32 {\n    19\n}\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let removed = doc["changed"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["path"] == "src/old.rs"))
        .unwrap_or_else(|| panic!("{doc:#}"));
    let callers: Vec<&str> = removed["callers"]
        .as_array()
        .map(|c| c.iter().filter_map(|c| c["path"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(callers, vec!["src/main_use.rs"], "{doc:#}");
}

/// `Owner.name` answers only for that owner's definition, and a path may
/// prefix `Owner::name`.
#[test]
fn a_qualified_symbol_names_its_owner_and_takes_a_path() {
    let tmp = repo_of(
        &[
            (
                "shop.py",
                "class Cart:\n    def add(self, x):\n        return x\n\n\nclass Ledger:\n    def total(self):\n        return 1\n",
            ),
            (
                "use.py",
                "from shop import Ledger\n\n\ndef run():\n    return Ledger().total()\n",
            ),
        ],
        &[],
    );
    let symbol = |query: &str| {
        Command::new(&*BIN)
            .current_dir(tmp.path())
            .args(["--symbol", query, "-q", "-f", "md"])
            .output()
            .expect("run diffctx")
    };
    let wrong = symbol("Cart.total");
    assert!(
        !wrong.status.success(),
        "{}",
        String::from_utf8_lossy(&wrong.stdout)
    );
    assert!(
        String::from_utf8_lossy(&wrong.stderr).contains("no definition"),
        "{}",
        String::from_utf8_lossy(&wrong.stderr)
    );
    for query in [
        "Ledger.total",
        "shop.py:Ledger.total",
        "shop.py:Ledger::total",
    ] {
        let out = symbol(query);
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{query}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(text.contains("shop.py::total"), "{query}: {text}");
    }
}

/// #392: files the repository's own ignore rules withhold are counted in
/// the header, in a diff and for a symbol, instead of reading as nothing.
#[test]
fn files_withheld_by_ignore_rules_are_counted_not_hidden() {
    let tmp = repo_of(
        &[
            (".diffctx/ignore", "app/tests/\n"),
            ("app/__init__.py", ""),
            ("app/conf.py", "def workers(cpu):\n    return cpu * 2\n"),
        ],
        &[
            ("app/conf.py", "def workers(cpu):\n    return cpu * 2 + 1\n"),
            (
                "app/tests/test_conf.py",
                "from app.conf import workers\n\n\ndef test_workers():\n    assert workers(1) == 3\n",
            ),
        ],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        header(&md).contains("1 changed file(s), 1 file(s) withheld by ignore rules"),
        "{md}"
    );
    assert!(md.contains("were not read"), "{md}");
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args(["--symbol", "workers", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        header(&text).contains("1 file(s) naming it withheld by ignore rules"),
        "{text}"
    );
}

/// A class moved within its file, text unchanged, changes nothing a caller
/// sees; moved and edited, it does.
#[test]
fn a_definition_moved_verbatim_is_no_change_and_moved_edited_is() {
    let conf = |token: &str, moved: bool| {
        let backend =
            format!("@dataclass\nclass Backend:\n    url: str\n    token: str = \"{token}\"\n");
        let rest = "def backend():\n    return Backend(\"http://x\")\n\n\ndef client(b):\n    a = 1\n    c = 2\n    d = 3\n    return b.url\n";
        if moved {
            format!("from dataclasses import dataclass\n\n\n{rest}\n\n{backend}")
        } else {
            format!("from dataclasses import dataclass\n\n\n{backend}\n\n{rest}")
        }
    };
    let caller = (
        "use.py",
        "from pkg.conf import Backend\n\n\ndef make():\n    return Backend(\"u\")\n",
    );
    for (token, listed) in [("", false), ("t", true)] {
        let after = conf(token, true);
        let tmp = repo_of(
            &[
                ("pkg/__init__.py", ""),
                ("pkg/conf.py", &conf("", false)),
                caller,
            ],
            &[("pkg/conf.py", &after)],
        );
        let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
        assert_eq!(md.contains("called from use.py::make"), listed, "{md}");
    }
}

/// A working tree whose only change is withheld is no clean tree: the
/// answer says the change was withheld, never "the working tree matches HEAD".
#[test]
fn a_change_entirely_withheld_is_not_called_a_clean_tree() {
    let tmp = repo_of(
        &[
            (".diffctx/ignore", "tests/\n"),
            ("tests/conftest.py", "X = 1\n"),
            ("app.py", "def run():\n    return 1\n"),
        ],
        &[],
    );
    write(tmp.path(), "tests/conftest.py", "X = 2\n");
    let md = impact_markdown(tmp.path(), "HEAD");
    assert!(!md.contains("matches HEAD"), "{md}");
    assert!(
        header(&md).contains("1 file(s) withheld by ignore rules"),
        "{md}"
    );
}

/// One grep serves every removed definition; each still-called name keeps
/// exactly its own callers.
#[test]
fn each_removed_definition_of_a_deleted_module_keeps_its_own_callers() {
    let module: String = (0..40)
        .map(|i| format!("export function fn_{i}(x) {{\n  return x + {i};\n}}\n\n"))
        .collect();
    let tmp = repo_of(
        &[
            ("src/many.js", &module),
            (
                "src/a.js",
                "import { fn_3 } from './many.js';\nexport function useA(x) {\n  return fn_3(x);\n}\n",
            ),
            (
                "src/b.js",
                "import { fn_17 } from './many.js';\nexport function useB(x) {\n  return fn_17(x);\n}\n",
            ),
        ],
        &[],
    );
    std::fs::remove_file(tmp.path().join("src/many.js")).expect("rm");
    commit_all(tmp.path(), "delete the module");
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let callers_of = |name: &str| -> Vec<String> {
        doc["changed"]
            .as_array()
            .and_then(|c| c.iter().find(|c| c["symbol"] == name))
            .and_then(|c| c["callers"].as_array())
            .map(|c| {
                let mut paths: Vec<String> = c
                    .iter()
                    .filter_map(|c| c["path"].as_str().map(str::to_string))
                    .collect();
                paths.dedup();
                paths
            })
            .unwrap_or_default()
    };
    assert_eq!(callers_of("fn_3"), vec!["src/a.js"], "{doc:#}");
    assert_eq!(callers_of("fn_17"), vec!["src/b.js"], "{doc:#}");
}

/// #414: in Python `//` is floor division; the call after it is a caller.
#[test]
fn a_call_after_python_floor_division_is_a_caller() {
    let tmp = repo_of(
        &[
            (
                "shop/paging.py",
                "def page_size(cfg):\n    return cfg.get(\"page\", 20)\n",
            ),
            (
                "shop/report.py",
                "from shop.paging import page_size\n\n\ndef pages(rows, cfg):\n    return rows // page_size(cfg)\n",
            ),
        ],
        &[(
            "shop/paging.py",
            "def page_size(cfg):\n    return max(1, cfg.get(\"page\", 20))\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let changed = doc["changed"].as_array().expect("changed");
    let page_size = changed
        .iter()
        .find(|c| c["symbol"] == "page_size")
        .unwrap_or_else(|| panic!("page_size missing: {doc:#}"));
    assert!(
        page_size["callers"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["symbol"] == "pages")),
        "{doc:#}"
    );
}

/// #403: a type and its impl blocks are one definition, and a trait method
/// is reached through its trait, not by the other `fmt`s in the repository.
#[test]
fn a_type_with_trait_impls_is_one_definition_reached_by_its_users() {
    let other = |t: &str| {
        format!(
            "pub struct {t};\n\nimpl std::fmt::Display for {t} {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        f.write_str(\"{t}\")\n    }}\n}}\n"
        )
    };
    let tmp = repo_of(
        &[
            (
                "src/lib.rs",
                "pub mod a;\npub mod b;\npub mod c;\npub mod error;\npub mod run;\n",
            ),
            ("src/a.rs", &other("A")),
            ("src/b.rs", &other("B")),
            ("src/c.rs", &other("C")),
            ("src/error.rs", "pub fn helper() {}\n"),
            (
                "src/run.rs",
                "use crate::error::QueryError;\n\npub fn run(q: &str) -> Result<(), QueryError> {\n    Err(QueryError { message: q.to_string() })\n}\n",
            ),
        ],
        &[(
            "src/error.rs",
            "pub fn helper() {}\n\npub struct QueryError {\n    pub message: String,\n}\n\nimpl std::fmt::Display for QueryError {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        f.write_str(&self.message)\n    }\n}\n\nimpl std::error::Error for QueryError {}\n\nimpl<T: Into<String>> From<T> for QueryError {\n    fn from(t: T) -> Self {\n        QueryError { message: t.into() }\n    }\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let symbols = changed_symbols(&doc);
    assert_eq!(
        symbols.iter().filter(|s| *s == "QueryError").count(),
        1,
        "{doc:#}"
    );
    let changed = doc["changed"].as_array().expect("changed");
    let entry = |name: &str| changed.iter().find(|c| c["symbol"] == name);
    let ty = entry("QueryError").expect("QueryError listed");
    assert!(ty.get("unresolved").is_none(), "{doc:#}");
    assert!(
        ty["callers"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["path"] == "src/run.rs")),
        "{doc:#}"
    );
    for (method, tr) in [("fmt", "Display"), ("from", "From")] {
        let m = entry(method).expect("trait method listed");
        assert!(
            m["unresolved"]
                .as_str()
                .is_some_and(|u| u.starts_with(&format!("implements `{tr}`"))),
            "{doc:#}"
        );
    }
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("rust files"), "{md}");
}

fn registry_repo(port_after: &str) -> TempDir {
    repo_of(
        &[
            (
                "flow/registry.py",
                "REGISTRY = {}\n\n\ndef register_node(cls):\n    REGISTRY[cls.__name__] = cls\n    return cls\n\n\ndef create(name):\n    return REGISTRY[name]()\n",
            ),
            (
                "flow/port.py",
                "from dataclasses import dataclass\n\n\n@dataclass\nclass Port:\n    name: str\n    kind: str = \"text\"\n",
            ),
            (
                "flow/nodes.py",
                "from flow.port import Port\nfrom flow.registry import register_node\n\n\n@register_node\nclass ImproveNode:\n    inputs = [Port(\"draft\")]\n\n    def run(self):\n        return self.inputs\n",
            ),
            (
                "tests/test_flow.py",
                "from flow.registry import create\n\n\ndef test_the_flow_runs():\n    assert create(\"ImproveNode\").run()\n",
            ),
        ],
        &[("flow/port.py", port_after)],
    )
}

/// #388: a defaulted field changes no construction site.
#[test]
fn a_defaulted_field_keeps_every_constructor_working() {
    let tmp = registry_repo(
        "from dataclasses import dataclass\n\n\n@dataclass\nclass Port:\n    name: str\n    kind: str = \"text\"\n    loop_back: bool = False\n",
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("called from flow/nodes.py"), "{md}");
    assert!(md.contains("signature-compatible"), "{md}");
    assert!(header(&md).contains(" 0 caller(s)"), "{md}");
}

/// #388: a class registered by a decorator is reached through the registry,
/// which static test links cannot follow.
#[test]
fn a_caller_registered_by_a_decorator_is_not_called_untested() {
    let tmp = registry_repo(
        "from dataclasses import dataclass\n\n\n@dataclass\nclass Port:\n    name: str\n    kind: int = 0\n",
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("flow/nodes.py::ImproveNode"), "{md}");
    assert!(md.contains("@register_node"), "{md}");
    assert!(!md.contains("no static test link found"), "{md}");
    assert!(
        header(&md).contains(", 0 caller(s) without a static test link"),
        "{md}"
    );
}

/// #414: a decorator that shapes its definition registers it nowhere, so an
/// untested setter or `attr.s` class is untested, not "registered by".
#[test]
fn shaping_decorators_are_not_registries() {
    let tmp = repo_of(
        &[
            ("util.py", "def clamp(v):\n    return max(0, v)\n"),
            (
                "meter.py",
                "import attr\nfrom util import clamp\n\n\nclass Meter:\n    @property\n    def value(self):\n        return self._v\n\n    @value.setter\n    def value(self, v):\n        self._v = clamp(v)\n\n\n@attr.s\nclass Gauge:\n    def read(self, v):\n        return clamp(v)\n",
            ),
            (
                "tests/test_other.py",
                "def test_nothing_here():\n    assert True\n",
            ),
        ],
        &[("util.py", "def clamp(v):\n    return max(0, min(v, 100))\n")],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("meter.py"), "{md}");
    assert!(!md.contains("registered by"), "{md}");
    assert!(md.contains("no static test link"), "{md}");
}

/// #370: a formatter run and a comment are no contract and reach no caller.
#[test]
fn a_formatting_only_change_is_neither_a_contract_nor_a_caller_list() {
    let tmp = ts_repo_with_panel();
    let repo = tmp.path();
    write(
        repo,
        "src/engine/panel.ts",
        "export const clamp = (v: number, a: number, b: number) =>\n  Math.min(b, Math.max(a, v));\n\n// Doubles its input.\nexport function mono(\n  x: number,\n): number {\n  return x * 2\n}\n",
    );
    for i in 1..=3 {
        write(
            repo,
            &format!("src/cards/card{i}.ts"),
            &format!(
                "import {{ mono, clamp }} from \"../engine/panel\";\n\nexport function draw{i}(v: number): number {{\n  return clamp(v, 0, 1) + mono(v);\n}}\n"
            ),
        );
    }
    commit_all(repo, "biome");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert!(contract_symbols(&doc).is_empty(), "{doc}");
    assert!(changed_symbols(&doc).is_empty(), "{doc}");
    assert_eq!(doc["empty"], true, "{doc}");

    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    \"\"\"The VAT-inclusive total.\"\"\"\n    # rounded to cents\n    return round(sum(i.price for i in items) * 1.19, 2)\n",
    );
    commit_all(repo, "docs");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert!(changed_symbols(&doc).is_empty(), "{doc}");
}

/// #362: an unchanged caller in a changed file is outside the diff.
#[test]
fn an_unchanged_caller_in_the_changed_file_is_listed() {
    let tmp = repo_of(
        &[(
            "tool.py",
            "def diagnose(xs):\n    return len(xs)\n\n\ndef main():\n    return diagnose([1, 2])\n",
        )],
        &[(
            "tool.py",
            "def diagnose(xs):\n    return len(xs) + 1\n\n\ndef main():\n    return diagnose([1, 2])\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("called from tool.py::main"), "{md}\n{doc}");
}

/// #359: `pub` inside a `pub(crate)` module is crate-internal.
#[test]
fn pub_items_of_a_crate_private_module_are_not_public_api() {
    let tmp = repo_of(
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"k\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "pub(crate) mod m;\npub mod n;\npub mod r#type;\n#[cfg(feature = \"k\")] pub mod kind;\n",
            ),
            ("src/m.rs", "pub fn f(x: u32) -> u32 {\n    x\n}\n"),
            ("src/n.rs", "pub fn g(x: u32) -> u32 {\n    x\n}\n"),
            ("src/type.rs", "pub fn t(x: u32) -> u32 {\n    x\n}\n"),
            ("src/kind.rs", "pub fn k(x: u32) -> u32 {\n    x\n}\n"),
        ],
        &[
            (
                "src/m.rs",
                "pub fn f(x: u32, y: u32) -> u32 {\n    x + y\n}\n",
            ),
            (
                "src/n.rs",
                "pub fn g(x: u32, y: u32) -> u32 {\n    x + y\n}\n",
            ),
            (
                "src/type.rs",
                "pub fn t(x: u32, y: u32) -> u32 {\n    x + y\n}\n",
            ),
            (
                "src/kind.rs",
                "pub fn k(x: u32, y: u32) -> u32 {\n    x + y\n}\n",
            ),
        ],
    );
    let contracts = contract_symbols(&impact(tmp.path(), "HEAD~1..HEAD"));
    for public in ["public_api g", "public_api t", "public_api k"] {
        assert!(contracts.contains(&public.to_string()), "{contracts:?}");
    }
    assert!(
        !contracts.contains(&"public_api f".to_string()),
        "{contracts:?}"
    );
}

/// #408: a range's module visibility is read from the range, not the disk:
/// an uncommitted `a/mod.rs` -> `a.rs` move made a private module public.
#[test]
fn module_visibility_of_a_range_ignores_the_working_tree() {
    let tmp = repo_of(
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"k\"\nversion = \"0.1.0\"\n",
            ),
            ("src/lib.rs", "pub mod a;\n"),
            ("src/a/mod.rs", "mod b;\n"),
            ("src/a/b.rs", "pub fn f(x: u32) -> u32 {\n    x\n}\n"),
        ],
        &[(
            "src/a/b.rs",
            "pub fn f(x: u32, y: u32) -> u32 {\n    x + y\n}\n",
        )],
    );
    let repo = tmp.path();
    git(repo, &["mv", "src/a/mod.rs", "src/a.rs"]);
    write(repo, "src/a.rs", "pub mod b;\n");
    let contracts = contract_symbols(&impact(repo, "HEAD~1..HEAD"));
    assert!(
        !contracts.contains(&"public_api f".to_string()),
        "{contracts:?}"
    );
}

/// #374: a script named for a schema is not a schema.
#[test]
fn a_script_named_for_a_schema_is_not_a_schema_contract() {
    let tmp = repo_of(
        &[
            ("scripts/ci/schema-compat.sh", "#!/bin/sh\ndocker build .\n"),
            ("db/schema.sql", "create table a (id int);\n"),
        ],
        &[
            (
                "scripts/ci/schema-compat.sh",
                "#!/bin/sh\ndocker build --pull .\n",
            ),
            ("db/schema.sql", "create table a (id bigint);\n"),
        ],
    );
    let contracts = contract_symbols(&impact(tmp.path(), "HEAD~1..HEAD"));
    assert_eq!(contracts, vec!["schema db/schema.sql".to_string()]);
}

/// #369, #383: a deleted path is named by its path, not by its basename
/// where another file still has it, and never by a bare word.
#[test]
fn a_stale_reference_names_the_deleted_path_itself() {
    let tmp = repo_of(
        &[
            ("biome.jsonc", "{}\n"),
            ("child/biome.json", "{}\n"),
            ("child/pkg/probes.py", "def ping():\n    return 1\n"),
            ("child/pkg/__init__.py", ""),
            (".pre-commit-config.yaml", "files: biome.jsonc\n"),
            ("AGENTS.md", "The sweep probes every child.\n"),
            ("mise.toml", "install = \"mise install\"\n"),
            (
                "child/app.py",
                "from pkg.probes import ping\n\n\ndef go():\n    return ping()\n",
            ),
            (
                "child/README.md",
                "Edit child/biome.json to change the rules.\n",
            ),
        ],
        &[],
    );
    let repo = tmp.path();
    git(
        repo,
        &["rm", "-q", "child/biome.json", "child/pkg/probes.py"],
    );
    commit_all(repo, "drop");
    let doc = impact(repo, "HEAD~1..HEAD");
    let mut stale: Vec<String> = doc["stale_references"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| format!("{}:{}", r["path"].as_str().unwrap(), r["line"]))
        .collect();
    stale.sort();
    assert_eq!(
        stale,
        vec!["child/README.md:1", "child/app.py:1"],
        "{doc:#}"
    );
}

/// #367: a removed member of an exported interface is a contract, and an
/// optional call to it outside the diff is its caller.
#[test]
fn a_removed_optional_member_names_its_remaining_call_site() {
    let tmp = repo_of(
        &[
            (
                "src/engine.ts",
                "export interface Engine {\n  play(): void;\n  setGain?(db: number): void;\n}\n",
            ),
            (
                "src/web.ts",
                "import type { Engine } from './engine';\n\nexport class WebEngine implements Engine {\n  play(): void {}\n  setGain(db: number): void {}\n}\n",
            ),
            (
                "src/store.ts",
                "import type { Engine } from './engine';\n\nexport function apply(engine: Engine, db: number): void {\n  engine.setGain?.(db);\n}\n",
            ),
        ],
        &[
            (
                "src/engine.ts",
                "export interface Engine {\n  play(): void;\n}\n",
            ),
            (
                "src/web.ts",
                "import type { Engine } from './engine';\n\nexport class WebEngine implements Engine {\n  play(): void {}\n}\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("setGain"), "{md}");
    assert!(md.contains("src/store.ts::apply"), "{md}\n{doc}");
    assert!(
        contract_symbols(&doc)
            .iter()
            .any(|c| c.starts_with("removed_api") && c.ends_with("setGain")),
        "{doc}"
    );
}

fn change_class_of(repo: &Path, path: &str) -> String {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", "HEAD~1..HEAD", "-f", "json", "-q"])
        .output()
        .expect("run diffctx");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("json");
    doc["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .find(|c| c["path"] == path)
        .map(|c| c["class"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// A layout verdict hides the symbol's callers, so only what a formatter
/// can change may earn it: `//` is code in Python, a swapped import binds
/// different names, a template literal is not a plain string, a statement
/// moved between hunks is a reorder.
#[test]
fn code_that_only_looks_like_formatting_is_not_layout() {
    let floor = repo_of(
        &[
            ("m.py", "def mid(lo, hi):\n    return (lo + hi) // 2\n"),
            (
                "u.py",
                "from m import mid\n\n\ndef use(a):\n    return mid(a, 9)\n",
            ),
        ],
        &[("m.py", "def mid(lo, hi):\n    return (lo + hi) // 3\n")],
    );
    assert_ne!(change_class_of(floor.path(), "m.py"), "layout");
    assert!(
        impact_markdown(floor.path(), "HEAD~1..HEAD").contains("called from u.py::use"),
        "a floor-division change keeps its callers"
    );
    let swapped = repo_of(
        &[(
            "m.py",
            "from a import x\nfrom b import y\n\n\ndef f():\n    return x() + y()\n",
        )],
        &[(
            "m.py",
            "from a import y\nfrom b import x\n\n\ndef f():\n    return x() + y()\n",
        )],
    );
    assert_ne!(change_class_of(swapped.path(), "m.py"), "layout");
    let template = repo_of(
        &[(
            "t.ts",
            "export function greet(n: string): string {\n  return 'hi ${n}';\n}\n",
        )],
        &[(
            "t.ts",
            "export function greet(n: string): string {\n  return `hi ${n}`;\n}\n",
        )],
    );
    assert_ne!(change_class_of(template.path(), "t.ts"), "layout");
    let sql = repo_of(
        &[(
            "q.py",
            "def active(cur):\n    return cur.execute(\n        \"\"\"SELECT id FROM users WHERE active\"\"\"\n    )\n",
        )],
        &[(
            "q.py",
            "def active(cur):\n    return cur.execute(\n        \"\"\"SELECT id FROM users WHERE NOT active\"\"\"\n    )\n",
        )],
    );
    assert_ne!(change_class_of(sql.path(), "q.py"), "layout");
    let reordered = repo_of(
        &[("r.py", "A = 1\nB = 2\nC = 3\nD = 4\nE = 5\nF = 6\nG = 7\n")],
        &[("r.py", "B = 2\nC = 3\nD = 4\nE = 5\nF = 6\nG = 7\nA = 1\n")],
    );
    assert_ne!(change_class_of(reordered.path(), "r.py"), "layout");
}

/// Only an appended parameter or field that brings its own default leaves
/// call sites working (#388); anything else lists them.
#[test]
fn breaking_insertions_are_not_signature_compatible() {
    let mid = repo_of(
        &[
            ("f.py", "def f(\n    a,\n    c=2,\n):\n    return a + c\n"),
            (
                "u.py",
                "from f import f\n\n\ndef use():\n    return f(1, 5)\n",
            ),
        ],
        &[(
            "f.py",
            "def f(\n    a,\n    b=1,\n    c=2,\n):\n    return a + b + c\n",
        )],
    );
    let md = impact_markdown(mid.path(), "HEAD~1..HEAD");
    assert!(!md.contains("signature-compatible"), "{md}");
    assert!(md.contains("called from u.py::use"), "{md}");
    let callback = repo_of(
        &[
            ("p.ts", "export interface Props {\n  label: string;\n}\n"),
            (
                "c.ts",
                "import type { Props } from './p';\n\nexport function render(p: Props): string {\n  return p.label;\n}\n",
            ),
        ],
        &[(
            "p.ts",
            "export interface Props {\n  label: string;\n  onChange: (v: string) => void;\n}\n",
        )],
    );
    assert!(!impact_markdown(callback.path(), "HEAD~1..HEAD").contains("signature-compatible"));
    let required = repo_of(
        &[
            (
                "m.py",
                "from pydantic import BaseModel, Field\n\n\nclass Order(BaseModel):\n    sku: str\n",
            ),
            (
                "u.py",
                "from m import Order\n\n\ndef make():\n    return Order(sku='a')\n",
            ),
        ],
        &[(
            "m.py",
            "from pydantic import BaseModel, Field\n\n\nclass Order(BaseModel):\n    sku: str\n    qty: int = Field(...)\n",
        )],
    );
    let md = impact_markdown(required.path(), "HEAD~1..HEAD");
    assert!(!md.contains("signature-compatible"), "{md}");
    // A call in the body that gains a keyword argument is no parameter list.
    let body = repo_of(
        &[
            (
                "pkg/core.py",
                "def handle(a):\n    return compute(a)\n\n\ndef compute(a, strict=False):\n    return a\n",
            ),
            (
                "pkg/app.py",
                "from pkg.core import handle\n\n\ndef handler(req):\n    return handle(req)\n",
            ),
        ],
        &[(
            "pkg/core.py",
            "def handle(a):\n    return compute(a, strict=True)\n\n\ndef compute(a, strict=False):\n    return a\n",
        )],
    );
    let md = impact_markdown(body.path(), "HEAD~1..HEAD");
    assert!(!md.contains("signature-compatible"), "{md}");
    assert!(md.contains("called from pkg/app.py::handler"), "{md}");
    // `__slots__` changes how every instance is built.
    let slots = repo_of(
        &[
            ("pt.py", "class Point:\n    x = 0\n"),
            (
                "u.py",
                "from pt import Point\n\n\ndef make():\n    p = Point()\n    p.y = 1\n    return p\n",
            ),
        ],
        &[(
            "pt.py",
            "class Point:\n    x = 0\n    __slots__ = (\"x\",)\n",
        )],
    );
    let md = impact_markdown(slots.path(), "HEAD~1..HEAD");
    assert!(!md.contains("signature-compatible"), "{md}");
    assert!(md.contains("u.py::make"), "{md}");
}

/// Relative spellings name the deleted file too: `./utils`, `from .probes
/// import`; a `../lib/foo` that resolves elsewhere does not.
#[test]
fn relative_references_to_a_moved_file_are_stale_and_foreign_ones_are_not() {
    let tmp = repo_of(
        &[
            ("src/utils.ts", "export const x = 1;\n"),
            (
                "src/app.ts",
                "import { x } from './utils';\nexport const y = x;\n",
            ),
            ("pkg/probes.py", "def ping():\n    return 1\n"),
            ("pkg/__init__.py", ""),
            (
                "pkg/run.py",
                "from .probes import ping\n\n\ndef go():\n    return ping()\n",
            ),
            ("packages/a/lib/foo.ts", "export const f = 1;\n"),
            ("packages/b/lib/foo.ts", "export const g = 2;\n"),
            (
                "packages/b/src/x.ts",
                "import { g } from '../lib/foo';\nexport const h = g;\n",
            ),
        ],
        &[],
    );
    let repo = tmp.path();
    git(repo, &["mv", "src/utils.ts", "src/helpers.ts"]);
    git(
        repo,
        &["rm", "-q", "pkg/probes.py", "packages/a/lib/foo.ts"],
    );
    commit_all(repo, "move");
    let doc = impact(repo, "HEAD~1..HEAD");
    let mut stale: Vec<String> = doc["stale_references"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| format!("{}:{}", r["path"].as_str().unwrap(), r["line"]))
        .collect();
    stale.sort();
    assert_eq!(stale, vec!["pkg/run.py:1", "src/app.ts:1"], "{doc:#}");
}

/// A definition moved to another module leaves the old module's importers
/// broken; the removal is reported with them (#367 class).
#[test]
fn a_moved_function_names_the_importers_of_its_old_module() {
    let tmp = repo_of(
        &[
            (
                "a.py",
                "def foo():\n    return 1\n\n\ndef keep():\n    return 2\n",
            ),
            ("b.py", "def bar():\n    return 3\n"),
            (
                "c.py",
                "from a import foo\n\n\ndef use():\n    return foo()\n",
            ),
        ],
        &[
            ("a.py", "def keep():\n    return 2\n"),
            (
                "b.py",
                "def bar():\n    return 3\n\n\ndef foo():\n    return 1\n",
            ),
        ],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("c.py::use"), "{md}");
}

/// Removed members are found wherever the parser split their class, and
/// `def self.x` names `x`, never `self`.
#[test]
fn removed_methods_of_split_classes_and_singleton_methods_are_named() {
    let ts = repo_of(
        &[
            (
                "store.ts",
                "export class Store {\n  load(): number {\n    return 1;\n  }\n\n  save(): number {\n    return 2;\n  }\n}\n",
            ),
            (
                "use.ts",
                "import { Store } from './store';\n\nexport function persist(s: Store): number {\n  return s.save();\n}\n",
            ),
        ],
        &[(
            "store.ts",
            "export class Store {\n  load(): number {\n    return 1;\n  }\n}\n",
        )],
    );
    let md = impact_markdown(ts.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("save") && md.contains("use.ts::persist"),
        "{md}"
    );
    let rb = repo_of(
        &[
            (
                "svc.rb",
                "class Svc\n  def self.call(x)\n    x\n  end\n\n  def self.keep\n    1\n  end\nend\n",
            ),
            (
                "job.rb",
                "require_relative 'svc'\n\ndef run\n  Svc.call(1)\nend\n",
            ),
        ],
        &[("svc.rb", "class Svc\n  def self.keep\n    1\n  end\nend\n")],
    );
    let doc = impact(rb.path(), "HEAD~1..HEAD");
    assert!(
        !changed_symbols(&doc).contains(&"self".to_string()),
        "{doc}"
    );
}

/// Deleting the functions below one leaves that one unchanged: only the
/// removed definitions are the change.
#[test]
fn a_deletion_below_a_definition_does_not_change_it() {
    let tmp = repo_of(
        &[
            (
                "lib.py",
                "import os\n\n\ndef keep(x):\n    return x\n\n\ndef drop(x):\n    return -x\n\n\ndef tail():\n    return 0\n",
            ),
            (
                "use.py",
                "from lib import keep\n\n\ndef run():\n    return keep(1)\n",
            ),
        ],
        // An edit elsewhere in the file, as an import fix beside such a
        // deletion usually is.
        &[(
            "lib.py",
            "import sys\n\n\ndef keep(x):\n    return x\n\n\ndef tail():\n    return 0\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        !changed_symbols(&doc).contains(&"keep".to_string()),
        "{doc}"
    );
    // Removing the body's last line is a change to it.
    let tail = repo_of(
        &[
            (
                "lib.py",
                "def keep(x):\n    x = x + 1\n    return x\n\n\ndef tail():\n    return 0\n",
            ),
            (
                "use.py",
                "from lib import keep\n\n\ndef run():\n    return keep(1)\n",
            ),
        ],
        &[(
            "lib.py",
            "def keep(x):\n    x = x + 1\n\n\ndef tail():\n    return 0\n",
        )],
    );
    let doc = impact(tail.path(), "HEAD~1..HEAD");
    assert!(changed_symbols(&doc).contains(&"keep".to_string()), "{doc}");
}

/// Two children of a monorepo that never import each other share words,
/// not tests: a lexical test link stops at the project a manifest marks.
#[test]
fn a_sibling_project_s_test_does_not_guard_a_caller() {
    // One runtime with no binding model, so the link is the lexical one the
    // manifest stop governs (a test in another runtime links nothing, #487).
    // `quiz` names its own `Config`, never importing `web.Config`.
    let files = |root_manifest: bool| {
        let mut f = vec![
            (
                "web/pom.xml",
                "<project><artifactId>web</artifactId></project>\n",
            ),
            (
                "web/src/main/java/web/Contracts.java",
                "package web;\n\npublic class Contracts {\n    public static String theme() {\n        return \"dark\";\n    }\n}\n",
            ),
            (
                "web/src/main/java/web/Config.java",
                "package web;\n\npublic class Config {\n    public static String generated() {\n        return Contracts.theme();\n    }\n}\n",
            ),
            (
                "quiz/src/test/java/quiz/ConfigTest.java",
                "package quiz;\n\npublic class ConfigTest {\n    public void testGenerated() {\n        assert Config.generated().equals(\"dark\");\n    }\n}\n",
            ),
        ];
        if root_manifest {
            f.push((
                "quiz/pom.xml",
                "<project><artifactId>quiz</artifactId></project>\n",
            ));
        }
        f
    };
    let change = [(
        "web/src/main/java/web/Contracts.java",
        "package web;\n\npublic class Contracts {\n    public static String theme() {\n        return \"light\";\n    }\n}\n",
    )];
    // One project: the lexical link stands, so the probe below is not vacuous.
    let together = repo_of(&files(false), &change);
    let md = impact_markdown(together.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("quiz/src/test/java/quiz/ConfigTest.java"),
        "{md}"
    );
    let apart = repo_of(&files(true), &change);
    let md = impact_markdown(apart.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("web/src/main/java/web/Config.java::generated"),
        "{md}"
    );
    assert!(!md.contains("ConfigTest.java"), "{md}");
}

/// A sibling module of one language that imports the definition by its
/// qualified name is a real dependency: a Maven `it/` module tests `core/`.
#[test]
fn a_sibling_module_importing_the_qualified_name_still_guards_it() {
    let pricing = |rate: &str| {
        format!(
            "package com.acme.core;\n\npublic class Pricing {{\n    public static int gross(int a) {{\n        return a * {rate};\n    }}\n}}\n"
        )
    };
    let tmp = repo_of(
        &[
            (
                "pom.xml",
                "<project><modules><module>core</module><module>it</module></modules></project>\n",
            ),
            (
                "core/pom.xml",
                "<project><artifactId>core</artifactId></project>\n",
            ),
            (
                "it/pom.xml",
                "<project><artifactId>it</artifactId></project>\n",
            ),
            (
                "core/src/main/java/com/acme/core/Pricing.java",
                &pricing("2"),
            ),
            (
                "it/src/test/java/com/acme/it/PricingTest.java",
                "package com.acme.it;\n\nimport com.acme.core.Pricing;\n\npublic class PricingTest {\n    public void testGross() {\n        assert Pricing.gross(2) == 6;\n    }\n}\n",
            ),
        ],
        &[(
            "core/src/main/java/com/acme/core/Pricing.java",
            &pricing("3"),
        )],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("PricingTest.java"), "{md}");
    assert!(
        !md.contains("Pricing.java::gross (4-6) — no static test link"),
        "{md}"
    );
}

/// Where indentation, a tuple's comma, a mid-word `#` or a semicolon before
/// a `[` line is syntax, changing it is not formatting; a consistent
/// reindent, a re-wrapped call and a call's trailing comma still are.
#[test]
fn syntax_that_formatting_rules_would_erase_is_not_layout() {
    let class = |path: &str, old: &str, new: &str| {
        let repo = repo_of(&[(path, old)], &[(path, new)]);
        change_class_of(repo.path(), path)
    };
    for (path, old, new) in [
        (
            "p.py",
            "def f(x):\n    if x:\n        a()\n        b()\n    c()\n",
            "def f(x):\n    if x:\n        a()\n    b()\n    c()\n",
        ),
        ("k.yaml", "a:\n  b: 1\n  c: 2\n", "a:\n  b: 1\nc: 2\n"),
        (
            "u.yaml",
            "url: http://a.example\n",
            "url: http://b.example\n",
        ),
        ("t.py", "T = (1,)\n", "T = (1)\n"),
        ("s.sh", "echo ${#a}\n", "echo ${#b}\n"),
        (
            "j.js",
            "let a = 1\n[1, 2].map(f)\n",
            "let a = 1;\n[1, 2].map(f)\n",
        ),
    ] {
        assert_ne!(class(path, old, new), "layout", "{path}: {new:?}");
    }
    for (path, old, new) in [
        (
            "y.yaml",
            "a:\n  b: 1\n  c:\n    d: 2\n",
            "a:\n    b: 1\n    c:\n        d: 2\n",
        ),
        (
            "r.py",
            "def f():\n    return g(a, b)\n",
            "def f():\n    return g(\n        a,\n        b,\n    )\n",
        ),
        ("c.sh", "echo a # old\n", "echo a # new\n"),
        ("k.py", "f(a,)\n", "f(a)\n"),
        ("s.js", "let a = 1\nfoo()\n", "let a = 1;\nfoo()\n"),
    ] {
        assert_eq!(class(path, old, new), "layout", "{path}: {new:?}");
    }
}

/// A function nested in another is a local name, and reading an attribute
/// of an object nothing types is no call: neither collects possible
/// callers from unrelated code.
#[test]
fn local_functions_and_untyped_attribute_reads_are_not_callers() {
    let tmp = repo_of(
        &[
            (
                "log.py",
                "class RequestLog:\n    def headers(self):\n        return {}\n\n    def __call__(self, out):\n        def write(line):\n            out.append(line)\n\n        write('a')\n        return out\n",
            ),
            (
                "server.py",
                "def respond(handler, response):\n    handler.wfile.write(b'x')\n    return response.headers\n",
            ),
            (
                "client.py",
                "def fetch(session):\n    return session.headers()\n",
            ),
        ],
        &[(
            "log.py",
            "class RequestLog:\n    def headers(self):\n        return {'x': '1'}\n\n    def __call__(self, out):\n        def write(line):\n            out.append(line.strip())\n\n        write('a')\n        return out\n",
        )],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("server.py"), "{md}");
    assert!(
        md.contains("client.py::fetch"),
        "an untyped call stays possible: {md}"
    );
}

/// #454: a change this build cannot read for definitions — no grammar for
/// its language, or a parse that broke — is named as not analysed, and the
/// answer is never the all-clear a hook stays silent on. A script that
/// parses cleanly and defines nothing is analysed: there was nothing to miss.
#[test]
fn a_code_change_without_a_parse_is_not_analysed_not_an_all_clear() {
    let solver = |k: &str| {
        format!(
            "module solver\ncontains\n  subroutine step(x)\n    real :: x\n    x = x * {k}\n  end subroutine step\nend module solver\n"
        )
    };
    let tmp = repo_of(
        &[
            ("num/solver.f90", &solver("2.0")),
            ("tools/run.py", "print(\"run\")\n"),
            ("tools/broken.py", "value = (1,\n"),
            ("tools/gone.f90", "program gone\nend program gone\n"),
        ],
        &[
            ("num/solver.f90", &solver("3.0")),
            ("tools/run.py", "print(\"run again\")\n"),
            ("tools/broken.py", "value = (2,\n"),
        ],
    );
    git(tmp.path(), &["rm", "-q", "tools/gone.f90"]);
    commit_all(tmp.path(), "drop gone");
    let doc = impact(tmp.path(), "HEAD~2..HEAD");
    assert_eq!(doc["empty"], false, "{doc}");
    let unanalysed: Vec<(String, String)> = doc["unanalysed_files"]
        .as_array()
        .expect("unanalysed_files")
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_string(),
                f["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        unanalysed,
        vec![
            ("num/solver.f90".to_string(), "no_grammar".to_string()),
            ("tools/broken.py".to_string(), "parse_failed".to_string()),
        ],
        "{doc}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~2..HEAD");
    assert!(header(&md).contains("2 not analysed"), "{md}");
    assert!(header(&md).contains("in the analysed files"), "{md}");
    assert!(
        md.contains("1 fortran (no grammar in this build): num/solver.f90"),
        "{md}"
    );
    assert!(
        md.contains("1 python (parse failed): tools/broken.py"),
        "{md}"
    );
    assert!(!md.contains("No resolved static callers"), "{md}");
}

/// #454: "no definition found" reads as "the name does not exist". When the
/// only files naming it were read as text, the answer says which and why;
/// a name nobody spells keeps the plain answer.
#[test]
fn a_symbol_named_only_in_unparsed_files_is_not_said_to_be_missing() {
    let tmp = repo_of(
        &[
            (
                "num/solver.f90",
                "module solver\ncontains\n  subroutine relax_step(x)\n    real :: x\n  end subroutine relax_step\nend module solver\n",
            ),
            (
                "num/use.f90",
                "program use\n  call relax_step(1.0)\nend program use\n",
            ),
            ("app.py", "def main():\n    return 1\n"),
        ],
        &[],
    );
    let symbol = |name: &str| {
        Command::new(&*BIN)
            .current_dir(tmp.path())
            .args(["--symbol", name, "-q", "-f", "md"])
            .output()
            .expect("run diffctx")
    };
    let unparsed = symbol("relax_step");
    assert!(!unparsed.status.success());
    let err = String::from_utf8_lossy(&unparsed.stderr);
    assert!(
        err.contains("2 file(s) naming it were read as text"),
        "{err}"
    );
    assert!(err.contains("fortran: no grammar in this build"), "{err}");
    assert!(
        err.contains("num/solver.f90") && err.contains("num/use.f90"),
        "{err}"
    );

    let missing = symbol("nowhere_spelled");
    let err = String::from_utf8_lossy(&missing.stderr);
    assert!(err.contains("no definition"), "{err}");
    assert!(!err.contains("read as text"), "{err}");
}

/// A Kotlin class whose header is exactly its signature — a multi-line
/// primary constructor ending `) {` — shares its id with the signature
/// variant; the definition is still the class, with its callers.
#[test]
fn a_class_whose_header_is_its_signature_keeps_its_definition() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Turns.kt",
                "package a\n\nclass Turns(\n    private val api: String,\n    private val namespace: String,\n    private val token: () -> String,\n    private val poll: Long = 10,\n) {\n    fun turn(): Boolean = true\n}\n",
            ),
            (
                "src/main/kotlin/a/User.kt",
                "package a\n\nclass User {\n    fun make() = Turns(\"x\", \"y\", { \"t\" })\n}\n",
            ),
        ],
        &[],
    );
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args(["--symbol", "Turns", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(header(&md).contains("1 definition(s)"), "{md}");
    assert!(md.contains("Turns.kt::Turns (3-8)"), "{md}");
    assert!(md.contains("User.kt::make"), "{md}");
}

/// #487: no runtime crosses from Kotlin to Swift. A Swift function that
/// shares a name with the Kotlin client's is not reachable from the Kotlin
/// test that reaches the Kotlin one.
#[test]
fn a_test_in_another_runtime_does_not_guard_a_namesake() {
    let tmp = repo_of(
        &[
            (
                "android/app/src/main/java/a/ApiClient.kt",
                "package a\n\ndata class UserPreferences(val theme: String)\n\nclass ApiClient {\n    fun fetchUserPreferences(): UserPreferences = UserPreferences(\"dark\")\n}\n",
            ),
            (
                "ios/Sources/APIClient.swift",
                "import Foundation\n\nstruct UserPreferences {\n    let theme: String\n}\n\nclass APIClient {\n    func fetchUserPreferences() -> UserPreferences {\n        return UserPreferences(theme: \"dark\")\n    }\n}\n",
            ),
            (
                "android/app/src/test/java/a/ApiClientAdminTest.kt",
                "package a\n\nclass ApiClientAdminTest {\n    fun prefs() {\n        val apiClient = ApiClient()\n        check(apiClient.fetchUserPreferences().theme == \"dark\")\n    }\n}\n",
            ),
        ],
        &[],
    );
    let out = Command::new(&*BIN)
        .current_dir(tmp.path())
        .args(["--symbol", "UserPreferences", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    let md = String::from_utf8_lossy(&out.stdout);
    let line = |path: &str| {
        md.lines()
            .find(|l| l.contains(path) && l.contains("fetchUserPreferences"))
            .unwrap_or_else(|| panic!("{path}: {md}"))
            .to_string()
    };
    assert!(
        line("ApiClient.kt").contains("ApiClientAdminTest.kt"),
        "{md}"
    );
    assert!(
        !line("APIClient.swift").contains("ApiClientAdminTest"),
        "{md}"
    );
}

/// `main` moved on after `feature` forked from it: `main..feature` is what
/// the branch changed, measured from the fork point. As two trees compared,
/// `main`'s own later commit read as a revert inside the branch (#485, #462).
#[test]
fn a_range_whose_base_moved_on_is_measured_from_the_fork() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/stock.py",
        "def level(item):\n    return item.count\n",
    );
    commit_all(repo, "stock");
    git(repo, &["branch", "-M", "main"]);
    git(repo, &["checkout", "-q", "-b", "feature"]);
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    commit_all(repo, "feature: new rate");
    git(repo, &["checkout", "-q", "main"]);
    write(
        repo,
        "shop/stock.py",
        "def level(item):\n    return item.count - item.reserved\n",
    );
    commit_all(repo, "main: reserved stock");
    git(repo, &["checkout", "-q", "feature"]);

    for range in ["main..feature", "main...feature"] {
        let out = impact(repo, range);
        assert_eq!(
            out["changed_files"],
            serde_json::json!(["shop/pricing.py"]),
            "{range}: {out}"
        );
        for mode in [&["--mode", "pack"][..], &["--mode", "locate"], &["--full"]] {
            let out = Command::new(&*BIN)
                .current_dir(repo)
                .args(["--diff", range, "-q", "-f", "json"])
                .args(mode)
                .output()
                .expect("run diffctx");
            assert!(out.status.success(), "{range} {mode:?}");
            let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
            assert_eq!(
                doc["changed_files"],
                serde_json::json!(["shop/pricing.py"]),
                "{range} {mode:?}: main's own commit is not the branch's change"
            );
        }
    }
}

/// A range is answered from its snapshot alone: the same `A..B` with the
/// working tree at `B` and with the working tree stripped of the chart's
/// `Chart.yaml` and of a caller, plus junk, gives the same bytes in every
/// mode. The Helm chart root used to be probed on the disk (#408).
#[test]
fn a_range_reads_its_snapshot_whatever_the_disk_holds() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "charts/app/Chart.yaml",
        "apiVersion: v2\nname: app\nversion: 0.1.0\n",
    );
    write(
        repo,
        "charts/app/values.yaml",
        "replicaCount: 1\nimage:\n  tag: v1\n",
    );
    write(
        repo,
        "charts/app/templates/sub/deployment.yaml",
        "apiVersion: apps/v1\nkind: Deployment\nspec:\n  replicas: {{ .Values.replicaCount }}\n  template:\n    spec:\n      containers:\n        - image: \"app:{{ .Values.image.tag }}\"\n",
    );
    write(
        repo,
        "src/pricing.py",
        "def total(items):\n    return sum(items)\n",
    );
    write(
        repo,
        "src/checkout.py",
        "from src.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
    );
    commit_all(repo, "A");
    write(
        repo,
        "charts/app/values.yaml",
        "replicaCount: 3\nimage:\n  tag: v2\n",
    );
    write(
        repo,
        "src/pricing.py",
        "def total(items):\n    return sum(items) * 2\n",
    );
    commit_all(repo, "B");
    let modes = ["impact", "locate", "pack"];
    let answer = |mode: &str| {
        let out = Command::new(&*BIN)
            .current_dir(repo)
            .args(["--diff", "HEAD~1..HEAD", "-q", "-f", "json", "--mode", mode])
            .output()
            .expect("run diffctx");
        assert!(out.status.success(), "{mode}");
        out.stdout
    };
    let at_b: Vec<Vec<u8>> = modes.iter().map(|m| answer(m)).collect();
    std::fs::remove_file(repo.join("charts/app/Chart.yaml")).expect("rm");
    std::fs::remove_file(repo.join("src/checkout.py")).expect("rm");
    write(repo, "junk.py", "def total():\n    return 0\n");
    for (mode, before) in modes.iter().zip(&at_b) {
        assert!(
            answer(mode) == *before,
            "{mode}: the disk changed the answer for a committed range"
        );
    }
}

/// #423, #418: two tracked files modified beside two untracked databases and
/// an untracked notes file. The count is what git would commit; the notes
/// file is named apart as untracked; a binary is no source change at all.
/// A new file staged with `git add` is a change like any other.
#[test]
fn untracked_files_are_named_apart_and_binaries_are_not_changes() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.2, 2)\n",
    );
    write(repo, "README.md", "# shop\n\nprices include VAT\n");
    for db in ["orders.db", "cache.db"] {
        std::fs::write(repo.join(db), b"SQLite format 3\0\x10\0\x01\x01\0@  ").expect("db");
    }
    write(repo, "# Codex conversation", "notes from a chat\n");
    let out = impact(repo, "HEAD");
    assert_eq!(
        out["changed_files"],
        serde_json::json!(["README.md", "shop/pricing.py"]),
        "{out}"
    );
    assert_eq!(
        out["untracked_files"],
        serde_json::json!(["# Codex conversation"]),
        "{out}"
    );
    let text = impact_markdown(repo, "HEAD");
    assert!(text.contains("2 changed file(s), 1 untracked"), "{text}");
    // Untracked work alone is still work, never "the working tree matches HEAD".
    git(repo, &["stash", "-q"]);
    let alone = impact_markdown(repo, "HEAD");
    assert!(!alone.contains("No changes to analyse"), "{alone}");
    assert!(alone.contains("0 changed file(s), 1 untracked"), "{alone}");
    git(repo, &["stash", "pop", "-q"]);
    write(
        repo,
        "shop/coupon.py",
        "def discount(cart):\n    return 0.9\n",
    );
    git(repo, &["add", "shop/coupon.py"]);
    let out = impact(repo, "HEAD");
    assert!(
        out["changed_files"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("shop/coupon.py")),
        "a staged new file is a change: {out}"
    );
}

/// What an impact answer claims: each changed symbol's callers, and the
/// lines still naming a deleted path.
fn claims(doc: &serde_json::Value) -> Vec<String> {
    let mut out: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| {
            let symbol = c["symbol"].as_str().unwrap_or("").to_string();
            c["callers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(move |k| format!("{symbol} <- {}", k["path"].as_str().unwrap_or("")))
        })
        .chain(
            doc["stale_references"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|r| format!("stale {}", r["path"].as_str().unwrap_or(""))),
        )
        .collect();
    out.sort();
    out
}

/// #510: stopped at its `n`th deadline poll, for every `n` until it runs to
/// the end, a run either answers in full or names the deadline, and never
/// claims what the whole run does not. The logical clock replaces the
/// sleeps that made deadline tests race the runner.
#[test]
fn a_run_stopped_at_any_poll_names_the_deadline_or_answers_in_full() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(repo, "shop/legacy.py", "def old_rate():\n    return 1\n");
    write(
        repo,
        "shop/old_user.py",
        "from shop.legacy import old_rate\n\n\ndef use_old():\n    return old_rate()\n",
    );
    write(repo, "scripts/rotate.sh", "echo rotate\n");
    write(repo, "ops.md", "Run scripts/rotate.sh weekly.\n");
    commit_all(repo, "legacy and a script");
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items) * 1.3, 2)\n",
    );
    std::fs::remove_file(repo.join("shop/legacy.py")).expect("rm");
    std::fs::remove_file(repo.join("scripts/rotate.sh")).expect("rm");
    commit_all(repo, "drop legacy and the script");
    let truth = claims(&impact(repo, "HEAD~1..HEAD"));
    for expected in ["stale ops.md", "total <- shop/checkout.py"] {
        assert!(truth.iter().any(|c| c == expected), "{expected}: {truth:?}");
    }
    let mut n: u64 = 0;
    loop {
        let doc = impact_env(
            repo,
            "HEAD~1..HEAD",
            &[("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", &n.to_string())],
        );
        let got = claims(&doc);
        let stopped = doc["limits"].to_string().contains("deadline");
        assert!(
            got.iter().all(|c| truth.contains(c)),
            "poll {n}: claims beyond the whole run: {got:?} vs {truth:?}"
        );
        if !stopped {
            assert_eq!(got, truth, "poll {n}: a smaller answer that looks complete");
            break;
        }
        assert!(n < 5_000, "the run never finished");
        n += 1;
    }
}

/// #510: a run whose deadline has passed stops its input-sized loops. The
/// stale-reference loop, targets times lines, had no poll: stopped before
/// it began, it still read every line git grep returned, and ran a
/// one-second run to 4.5 s. Counted, not timed, so a loaded runner cannot
/// make it flake.
#[test]
fn a_stopped_run_reads_no_line_of_its_stale_reference_search() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for i in 1..=20 {
        write(repo, &format!("old/mod_{i}.py"), &format!("x = {i}\n"));
    }
    for j in 1..=10 {
        let page: String = (1..=20)
            .map(|k| format!("see old/mod_{}.py\n", (j * k) % 20 + 1))
            .collect();
        write(repo, &format!("docs/page_{j}.md"), &page);
    }
    commit_all(repo, "base");
    git(repo, &["rm", "-q", "-r", "old"]);
    git(repo, &["commit", "-q", "-m", "drop old"]);
    let whole = impact(repo, "HEAD~1..HEAD");
    let read = |doc: &serde_json::Value| {
        doc["stale_references"].as_array().map_or(0, Vec::len) as u64
            + doc["stale_references_omitted"].as_u64().unwrap_or(0)
    };
    assert_eq!(read(&whole), 200, "{whole}");
    let stopped = impact_env(
        repo,
        "HEAD~1..HEAD",
        &[("DIFFCTX_TEST_DEADLINE_EXPIRED", "1")],
    );
    assert!(
        stopped["limits"].to_string().contains("deadline"),
        "{stopped}"
    );
    assert_eq!(read(&stopped), 0, "a stopped run read on: {stopped}");
}

/// #460, #475: a call inside an f-string replacement field or a template
/// literal substitution is code, and is a caller; the same name in a plain
/// string or a comment is not. Strings and comments are read off the
/// grammar, interpolations kept, instead of a hand lexer that blanked
/// every quoted byte.
#[test]
fn a_call_inside_an_interpolation_is_a_caller_and_a_string_is_not() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "app/fmt.py", "def fence(text):\n    return text\n");
    write(
        repo,
        "app/bot.py",
        "from app.fmt import fence\n\n\ndef reply(text):\n    return f\"Reader's message:\\n{fence(text)}\"\n",
    );
    write(
        repo,
        "app/decoy.py",
        "def note():\n    # fence(text) is described elsewhere\n    return \"fence(text)\"\n",
    );
    write(
        repo,
        "src/lib/fmt.ts",
        "export function formatBytes(bytes: number): string {\n  return `${bytes} B`;\n}\n",
    );
    write(
        repo,
        "src/lib/plain.ts",
        "import { formatBytes } from './fmt';\nexport function plainTemplate(n: number): string {\n  return `size ${formatBytes(n)}`;\n}\n",
    );
    write(
        repo,
        "src/lib/decoy.ts",
        "import { formatBytes } from './fmt';\n// formatBytes(n) is documented\nexport const label = 'formatBytes(n)';\nexport const bound = formatBytes;\n",
    );
    commit_all(repo, "initial");
    write(
        repo,
        "app/fmt.py",
        "def fence(text):\n    return f\"```\\n{text}\\n```\"\n",
    );
    write(
        repo,
        "src/lib/fmt.ts",
        "export function formatBytes(bytes: number): string {\n  return `${bytes} bytes`;\n}\n",
    );
    commit_all(repo, "change");
    let out = impact(repo, "HEAD~1..HEAD");
    let callers_of = |symbol: &str| -> Vec<String> {
        out["changed"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["symbol"] == symbol)
            .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
            .map(|k| {
                format!(
                    "{}::{}",
                    k["path"].as_str().unwrap(),
                    k["symbol"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    assert_eq!(callers_of("fence"), vec!["app/bot.py::reply"], "{out}");
    let ts = callers_of("formatBytes");
    assert!(
        ts.contains(&"src/lib/plain.ts::plainTemplate".to_string()),
        "{out}"
    );
    assert!(
        !ts.iter().any(|c| c.starts_with("src/lib/decoy.ts::label")),
        "{out}"
    );
}

/// #515, #458: a removed definition is one the base declared and the head
/// does not, read off the grammar. A match arm in a deleted Rust function
/// below an `impl` was a removed member named `Err`; a removed `try/except`
/// around `int(...)` read `int` and `except` as definitions; a private
/// function was a removed API.
#[test]
fn only_declarations_the_head_lost_are_removed_definitions() {
    let tmp = repo_of(
        &[
            (
                "src/lib.rs",
                "pub struct Gate;\n\nimpl Gate {\n    pub fn open(&self) {}\n}\n\nfn record(x: u8) -> Result<u8, &'static str> {\n    if x > 0 {\n        return Ok(x);\n    }\n    Err(\"zero\")\n}\n\npub fn shown() -> u8 {\n    1\n}\n",
            ),
            (
                "app/parse.py",
                "class Parser:\n    def amount(self, text):\n        int(text)\n        return text\n",
            ),
            (
                "src/main.rs",
                "fn main() -> Result<(), u8> {\n    let n = lib::shown();\n    if n == 0 {\n        return Err(n);\n    }\n    Ok(())\n}\n",
            ),
            (
                "app/cli.py",
                "def count(raw):\n    try:\n        return int(raw)\n    except KeyError:\n        return 0\n",
            ),
        ],
        &[
            (
                "src/lib.rs",
                "pub struct Gate;\n\nimpl Gate {\n    pub fn open(&self) {}\n}\n",
            ),
            (
                "app/parse.py",
                "class Parser:\n    def amount(self, text):\n        return text\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let removed: Vec<String> = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "removed")
        .map(|c| c["symbol"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(removed, ["shown"], "{doc}");
    let contracts = contract_symbols(&doc);
    assert!(
        contracts.contains(&"removed_api shown".to_string()),
        "{doc}"
    );
    assert!(
        !contracts.contains(&"removed_api record".to_string()),
        "{doc}"
    );
}

/// #470, #471, #458, #450: a caller of a removed definition is code that can
/// still reach it — through an import the binding model resolves to the
/// file it left, else from the same runtime. The import line itself is no
/// caller. By name alone, `subprocess.run`, another file's own `run`, a
/// shell `die` and a TypeScript `test.describe` were callers.
#[test]
fn a_removed_definition_is_called_only_from_code_that_can_reach_it() {
    let tmp = repo_of(
        &[
            (
                "shop/helpers.py",
                "def helper():\n    return 1\n\n\ndef describe():\n    return 'h'\n\n\ndef keep():\n    return 2\n",
            ),
            (
                "shop/cart.py",
                "from shop.helpers import helper\n\n\ndef total():\n    return helper()\n",
            ),
            (
                "scripts/rotate-key.py",
                "import sys\n\n\ndef die(msg):\n    sys.exit(msg)\n\n\ndef run():\n    die('x')\n\n\ndef main():\n    run()\n",
            ),
            (
                "scripts/other.py",
                "import subprocess\n\n\ndef run():\n    return subprocess.run(['true'])\n",
            ),
            ("scripts/set.sh", "die() {\n  exit 1\n}\ndie\n"),
            (
                "web/app.spec.ts",
                "import { test } from '@playwright/test';\ntest.describe('app', () => {});\n",
            ),
        ],
        &[("shop/helpers.py", "def keep():\n    return 2\n")],
    );
    git(tmp.path(), &["rm", "-q", "scripts/rotate-key.py"]);
    git(tmp.path(), &["commit", "-q", "--amend", "--no-edit"]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let removed: Vec<(String, Vec<String>)> = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "removed")
        .map(|c| {
            (
                c["symbol"].as_str().unwrap().to_string(),
                c["callers"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .map(|k| {
                        format!(
                            "{}::{}",
                            k["path"].as_str().unwrap(),
                            k["symbol"].as_str().unwrap_or("")
                        )
                    })
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        removed,
        [(
            "helper".to_string(),
            vec!["shop/cart.py::total".to_string()]
        )],
        "{doc}"
    );
}

/// A file that declares a removed definition's name itself calls its own:
/// deleting a private `helper` in one Rust file listed another file's calls
/// to its own `helper` as broken callers.
#[test]
fn a_file_with_its_own_definition_of_the_name_is_no_caller_of_the_removed_one() {
    let tmp = repo_of(
        &[
            (
                "src/a.rs",
                "pub fn helper() -> u8 {\n    1\n}\n\npub fn keep() -> u8 {\n    helper()\n}\n",
            ),
            (
                "src/b.rs",
                "pub fn helper() -> u8 {\n    2\n}\n\npub fn other() -> u8 {\n    helper()\n}\n",
            ),
            (
                "src/c.rs",
                "pub fn third() -> u8 {\n    crate::a::helper()\n}\n",
            ),
        ],
        &[("src/a.rs", "pub fn keep() -> u8 {\n    1\n}\n")],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let callers: Vec<String> = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "removed" && c["symbol"] == "helper")
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|k| k["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(callers, ["src/c.rs"], "{doc}");
}

fn impact_budget_md(repo: &Path, budget: &str) -> String {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            "md",
            "--budget",
            budget,
        ])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    String::from_utf8(out.stdout).expect("utf8")
}

/// #413 (2): a caller of a removed definition is tested like any caller —
/// by a test that reaches it, or by being the test.
#[test]
fn callers_of_a_removed_definition_keep_their_test_links() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "lib.py",
        "def gone():\n    return 1\n\n\ndef kept():\n    return 2\n",
    );
    write(
        repo,
        "app.py",
        "from lib import gone\n\n\ndef use():\n    return gone()\n",
    );
    write(
        repo,
        "tests/test_app.py",
        "from app import use\n\n\ndef test_use():\n    assert use() == 1\n",
    );
    write(
        repo,
        "tests/test_lib.py",
        "from lib import gone\n\n\ndef test_gone():\n    assert gone() == 1\n",
    );
    commit_all(repo, "base");
    write(repo, "lib.py", "def kept():\n    return 2\n");
    commit_all(repo, "drop gone");
    let doc = impact(repo, "HEAD~1..HEAD");
    let callers: Vec<&serde_json::Value> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "gone")
        .flat_map(|c| c["callers"].as_array().into_iter().flatten())
        .collect();
    let tested = |path: &str| {
        callers
            .iter()
            .find(|c| c["path"] == path)
            .map(|c| c["tested_by"].clone())
    };
    assert_eq!(tested("app.py"), Some("tests/test_app.py".into()), "{doc}");
    assert_eq!(
        tested("tests/test_lib.py"),
        Some("tests/test_lib.py".into()),
        "{doc}"
    );
}

/// #413 (3): a `pub fn` removed from a private module is no removed API; one
/// removed from a public module is.
#[test]
fn a_removed_pub_fn_of_a_private_module_is_no_api() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(repo, "src/lib.rs", "mod inner;\npub mod api;\n");
    write(
        repo,
        "src/inner.rs",
        "pub fn hidden() -> u8 {\n    1\n}\n\npub fn stay() -> u8 {\n    2\n}\n",
    );
    write(
        repo,
        "src/api.rs",
        "pub fn shown() -> u8 {\n    1\n}\n\npub fn stay() -> u8 {\n    2\n}\n",
    );
    commit_all(repo, "base");
    write(repo, "src/inner.rs", "pub fn stay() -> u8 {\n    2\n}\n");
    write(repo, "src/api.rs", "pub fn stay() -> u8 {\n    2\n}\n");
    commit_all(repo, "drop both");
    let contracts = impact(repo, "HEAD~1..HEAD")["contracts"].to_string();
    assert!(contracts.contains("src/api.rs"), "{contracts}");
    assert!(!contracts.contains("src/inner.rs"), "{contracts}");
}

/// #413 (4): a possible caller yields before a changed contract, and the
/// header counts every caller found, not the ones that fit.
#[test]
fn shedding_drops_guesses_before_contracts_and_the_header_counts_all() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "ledger.py",
        "class Ledger:\n    def reconcile_all(self):\n        return 1\n",
    );
    for i in 0..12 {
        write(
            repo,
            &format!("jobs/job_{i}.py"),
            &format!("def nightly_{i}(ledger):\n    return ledger.reconcile_all()\n"),
        );
    }
    write(
        repo,
        "migrations/0002_add.sql",
        "ALTER TABLE a ADD b int;\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "ledger.py",
        "class Ledger:\n    def reconcile_all(self):\n        return 2\n",
    );
    write(
        repo,
        "migrations/0002_add.sql",
        "ALTER TABLE a ADD c int;\n",
    );
    commit_all(repo, "change");
    let md = impact_budget_md(repo, "100");
    assert!(md.contains("migrations/0002_add.sql"), "{md}");
    assert!(md.contains("12 possible"), "{md}");
}

/// #456: prose and comments the same change wrote about a deletion are its
/// record of it; a mention outside the change, or a code line it wrote, is
/// still a stale reference.
#[test]
fn the_changes_own_record_of_a_deletion_is_no_stale_reference() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "plugin/constraints.txt", "x==1\n");
    write(repo, "CHANGELOG.md", "# Changes\n");
    write(repo, "OUTSIDE.md", "Pin with plugin/constraints.txt.\n");
    write(
        repo,
        "tests/test_pub.py",
        "def test_pub():\n    assert True\n",
    );
    write(repo, "tools/load.py", "def load():\n    return 1\n");
    commit_all(repo, "base");
    std::fs::remove_file(repo.join("plugin/constraints.txt")).unwrap();
    write(
        repo,
        "CHANGELOG.md",
        "# Changes\n\n- `plugin/constraints.txt` is gone.\n",
    );
    write(
        repo,
        "tests/test_pub.py",
        "# plugin/constraints.txt was the launcher lock\ndef test_pub():\n    assert True\n",
    );
    write(
        repo,
        "tools/load.py",
        "def load():\n    return open(\"plugin/constraints.txt\").read()\n",
    );
    commit_all(repo, "drop the constraints");
    let stale: Vec<String> = impact(repo, "HEAD~1..HEAD")["stale_references"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| r["path"].as_str().unwrap().to_string())
        .collect();
    assert!(stale.contains(&"OUTSIDE.md".to_string()), "{stale:?}");
    assert!(stale.contains(&"tools/load.py".to_string()), "{stale:?}");
    assert!(!stale.contains(&"CHANGELOG.md".to_string()), "{stale:?}");
    assert!(
        !stale.contains(&"tests/test_pub.py".to_string()),
        "{stale:?}"
    );
}

/// #440: deleting a module of 30 000 definitions searches for every name
/// at once; passed as arguments they overflowed ARG_MAX, the search failed
/// and the one real caller was missed.
#[test]
fn a_module_of_thirty_thousand_definitions_keeps_its_caller() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    // Thirty modules of a thousand: every parse stays well inside its time
    // budget on a loaded runner, and the one removed-name search still
    // carries all 30 000 names.
    write(repo, "pkg/__init__.py", "");
    for m in 0..30 {
        let module: String = (m * 1000..(m + 1) * 1000)
            .map(|i| format!("def generated_function_number_{i:06}():\n    return {i}\n\n"))
            .collect();
        write(repo, &format!("pkg/gen_{m:02}.py"), &module);
    }
    write(
        repo,
        "pkg/consumer.py",
        "from pkg.gen_29 import generated_function_number_029997\n\n\ndef use():\n    return generated_function_number_029997()\n",
    );
    commit_all(repo, "base");
    for m in 0..30 {
        std::fs::remove_file(repo.join(format!("pkg/gen_{m:02}.py"))).unwrap();
    }
    commit_all(repo, "drop the generated modules");
    let doc = impact(repo, "HEAD~1..HEAD");
    assert!(
        !doc["limits"].to_string().contains("read_failed"),
        "{}",
        doc["limits"]
    );
    let callers: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(callers, vec!["pkg/consumer.py::use"], "{}", doc["limits"]);
}

/// A private Rust duplicate removed in favour of an imported shared one: its
/// file and another that import the shared one call that, not the removed
/// one, and a `use` line is an import, not a reference. A removed `pub fn`
/// still imported from its own module keeps its caller.
#[test]
fn a_removed_private_duplicate_has_no_callers_once_the_shared_one_is_imported() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        repo,
        "src/lib.rs",
        "mod a;\nmod b;\npub mod gone_mod;\nmod shared;\nmod user;\n",
    );
    write(
        repo,
        "src/shared.rs",
        "pub(crate) fn absorb(x: u8) -> u8 {\n    x\n}\n",
    );
    write(
        repo,
        "src/a.rs",
        "fn absorb(x: u8) -> u8 {\n    x\n}\n\npub fn classify() -> u8 {\n    absorb(1)\n}\n",
    );
    write(
        repo,
        "src/b.rs",
        "use crate::shared::absorb;\n\npub fn other() -> u8 {\n    absorb(2)\n}\n",
    );
    write(
        repo,
        "src/gone_mod.rs",
        "pub fn gone() -> u8 {\n    3\n}\n\npub fn kept() -> u8 {\n    4\n}\n",
    );
    write(
        repo,
        "src/user.rs",
        "use crate::gone_mod::gone;\n\npub fn needs() -> u8 {\n    gone()\n}\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "src/a.rs",
        "use crate::shared::absorb;\n\npub fn classify() -> u8 {\n    absorb(1)\n}\n",
    );
    write(repo, "src/gone_mod.rs", "pub fn kept() -> u8 {\n    4\n}\n");
    commit_all(repo, "share absorb, drop gone");
    let doc = impact(repo, "HEAD~1..HEAD");
    let callers_of = |symbol: &str| -> Vec<String> {
        doc["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["symbol"] == symbol)
            .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
            .map(|c| {
                format!(
                    "{}:{}",
                    c["path"].as_str().unwrap(),
                    c["lines"].as_str().unwrap()
                )
            })
            .collect()
    };
    assert_eq!(callers_of("absorb"), Vec::<String>::new(), "{doc}");
    assert_eq!(callers_of("gone"), vec!["src/user.rs:3-5"], "{doc}");
}

/// Shedding to fit a budget changes what is listed, never the counts the
/// header states: callers and their test links are counted whole.
#[test]
fn the_header_counts_the_same_at_any_budget() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "core.py", "def rate(x):\n    return x * 2\n");
    for i in 0..8 {
        write(
            repo,
            &format!("use_{i}.py"),
            &format!("from core import rate\n\n\ndef caller_{i}(x):\n    return rate(x) + {i}\n"),
        );
    }
    for i in 0..4 {
        write(
            repo,
            &format!("tests/test_use_{i}.py"),
            &format!(
                "from use_{i} import caller_{i}\n\n\ndef test_caller_{i}():\n    assert caller_{i}(1)\n"
            ),
        );
    }
    commit_all(repo, "base");
    write(repo, "core.py", "def rate(x):\n    return x * 3\n");
    commit_all(repo, "change");
    let header = |budget: &str| {
        impact_budget_md(repo, budget)
            .lines()
            .next()
            .unwrap()
            .to_string()
    };
    let whole = header("20000");
    assert!(
        whole.contains("8 caller(s)") && whole.contains("4 caller(s) without a static test link"),
        "{whole}"
    );
    assert_eq!(header("90"), whole);
}

/// #496: a Java method calling a sibling of its own class with no
/// qualifier is a caller; a comment naming it and a call through another
/// receiver are not.
#[test]
fn an_unqualified_same_class_java_call_is_a_caller() {
    let mapper = "package app;\n\npublic class Mapper {\n    static int map(int x) {\n        return x * 2;\n    }\n\n    public int toDto(int x) {\n        return map(x) + 1;\n    }\n\n    public int noted(int x) {\n        // map(x) is not called here\n        return other.map(x);\n    }\n}\n";
    let tmp = repo_of(
        &[
            ("src/main/java/app/Mapper.java", mapper),
            (
                "src/test/java/app/MapperTest.java",
                "package app;\n\npublic class MapperTest {\n    void testIt() {\n        new Mapper().toDto(1);\n    }\n}\n",
            ),
        ],
        &[(
            "src/main/java/app/Mapper.java",
            &mapper.replace("x * 2", "x * 3"),
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let callers: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "map")
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|c| c["symbol"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(callers, vec!["toDto"], "{doc}");
}

/// #484: a file over the size cap is never read, so its calls are not seen;
/// when it names the changed definition, the answer says so instead of
/// reading as complete.
#[test]
fn a_namer_over_the_size_cap_is_named_as_a_possible_caller() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    let body: String = (0..25)
        .map(|j| format!("    let v{j} = x + {j};\n"))
        .collect();
    let filler: String = (0..300)
        .map(|i| format!("pub fn filler_{i}(x: u32) -> u32 {{\n{body}    x\n}}\n\n"))
        .collect();
    write(
        repo,
        "Cargo.toml",
        "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        repo,
        "src/lib.rs",
        "pub mod big;\npub mod small;\npub mod testfiles;\n",
    );
    write(
        repo,
        "src/testfiles.rs",
        "pub fn is_test_path(p: &str) -> bool {\n    p.contains(\"test\")\n}\n",
    );
    write(
        repo,
        "src/big.rs",
        &format!(
            "{filler}pub fn uses_it(p: &str) -> bool {{\n    crate::testfiles::is_test_path(p)\n}}\n"
        ),
    );
    write(
        repo,
        "src/small.rs",
        "pub fn small_use(p: &str) -> bool {\n    crate::testfiles::is_test_path(p)\n}\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "src/testfiles.rs",
        "pub fn is_test_path(p: &str) -> bool {\n    p.contains(\"tests\")\n}\n",
    );
    commit_all(repo, "change");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(md.contains("src/big.rs (over the size cap)"), "{md}");
    assert!(md.contains("called from src/small.rs::small_use"), "{md}");
}

/// #521: a shell script the grammar cannot parse is no hedge on a JS
/// function it cannot call; a JS file that breaks the parse still is.
#[test]
fn a_broken_parse_hedges_only_callers_of_its_own_runtime() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "lib.mjs",
        "export function helper() {\n  return 1;\n}\n",
    );
    write(
        repo,
        "a.sh",
        "#!/bin/bash\n: \"${SEL:=input[type=\\\"text\\\"], input[type=\\\"email\\\"]}\"\necho \"$SEL\" helper\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "lib.mjs",
        "export function helper() {\n  return 2;\n}\n",
    );
    commit_all(repo, "change");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(!md.contains("a.sh"), "{md}");

    write(
        repo,
        "broken.mjs",
        "import { helper } from \"./lib.mjs\";\nexport function use( {\n  return helper();\n}\n",
    );
    commit_all(repo, "a broken caller");
    write(
        repo,
        "lib.mjs",
        "export function helper() {\n  return 3;\n}\n",
    );
    commit_all(repo, "change again");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(md.contains("broken.mjs"), "{md}");
}

/// #391: a local closure named like a changed function is called by its
/// own fragment, not the function; a call that names the function still is.
#[test]
fn a_local_closure_named_like_a_definition_is_no_caller() {
    let a = "pub fn read() -> u8 {\n    1\n}\n";
    let tmp = repo_of(
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod a;\npub mod b;\npub mod c;\n"),
            ("src/a.rs", a),
            (
                "src/b.rs",
                "pub fn is_pub(x: u8) -> u8 {\n    let read = |p: u8| p + 1;\n    read(x)\n}\n",
            ),
            (
                "src/c.rs",
                "pub fn real() -> u8 {\n    crate::a::read()\n}\n",
            ),
        ],
        &[("src/a.rs", &a.replace("    1\n", "    2\n"))],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let callers: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "read")
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|c| c["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(callers, vec!["src/c.rs"], "{doc}");
}
