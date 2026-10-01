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
    assert!(text.contains("(partial: the run stopped at"), "{text}");
    assert!(!text.contains("Nothing outside the diff"), "{text}");
}

#[test]
fn a_cap_every_repository_trips_does_not_make_an_empty_answer_speak() {
    // 1.18.1 listed every limit, so the edge and fragment caps a real
    // repository always hits turned each commit's empty answer into a
    // "(partial: ...)" injection (#306).
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "lone.py", "def lone():\n    return 1\n");
    write(repo, "other.py", "def other():\n    return 2\n");
    commit_all(repo, "initial");
    write(repo, "lone.py", "def lone():\n    return 3\n");
    commit_all(repo, "lone: 3");
    let doc = impact_env(
        repo,
        "HEAD~1..HEAD",
        &[("DIFFCTX_MAX_EDGE_CONTRIBUTIONS", "1")],
    );
    assert_eq!(doc["empty"], true, "{doc}");
    assert!(doc.get("limits").is_none(), "{doc}");
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
    assert!(text.contains("tested by tests/test_checkout.py"), "{text}");
    assert!(text.contains("shop/report.py::summarize"), "{text}");
    assert!(text.contains("UNTESTED"), "{text}");
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
    assert!(text.contains("Nothing outside the diff depends"), "{text}");
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
