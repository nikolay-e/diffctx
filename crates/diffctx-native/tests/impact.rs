// `--mode impact` is what the plugin hook injects before a commit (#310):
// callers outside the diff, whether a test guards each, and cross-commit
// overlap. These run the release binary on real git repositories, the way
// the hook does.

use std::path::Path;

use tempfile::TempDir;

mod support;
use support::{claims, commit_all, diffctx_unclocked, git, init_repo, repo, write};

fn impact(repo: &Path, range: &str) -> serde_json::Value {
    impact_env(repo, range, &[])
}

fn impact_env(repo: &Path, range: &str, env: &[(&str, &str)]) -> serde_json::Value {
    let out = diffctx_unclocked()
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

/// What a `{total, items}` listing holds and does not show.
fn omitted_of(listing: &serde_json::Value) -> u64 {
    listing["total"].as_u64().unwrap_or(0) - listing["items"].as_array().map_or(0, Vec::len) as u64
}

fn impact_markdown(repo: &Path, range: &str) -> String {
    let out = diffctx_unclocked()
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
    assert_eq!(doc["schema"], "diffctx.impact.v2");
    assert_eq!(doc["empty"], false);
    let changed = doc["definitions"].as_array().expect("changed");
    let total = changed
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("the changed function is listed");
    let callers = total["rows"].as_array().expect("callers");
    let by_symbol = |name: &str| {
        callers
            .iter()
            .find(|c| c["symbol"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {callers:?}"))
    };
    let charge = by_symbol("charge");
    assert_eq!(charge["path"], "shop/checkout.py");
    assert_eq!(charge["guard"]["test"], "tests/test_checkout.py");
    let summarize = by_symbol("summarize");
    assert_eq!(summarize["path"], "shop/report.py");
    assert!(
        summarize["guard"]["test"].is_null(),
        "nothing tests summarize: {summarize:?}"
    );
    assert!(
        !callers.iter().any(|c| c["path"] == "shop/pricing.py"),
        "the changed file is never its own caller"
    );
}

/// Every test that reaches a caller at the fewest hops is named, in path then
/// name order and three at most; the rest are counted, never dropped silently,
/// and each test is named by its function.
#[test]
fn the_guard_names_the_tests_at_the_fewest_hops_and_counts_the_rest() {
    let tests: Vec<(String, String)> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|n| {
            (
                format!("tests/test_{n}.py"),
                format!(
                    "from shop.checkout import charge\n\n\ndef test_{n}():\n    assert charge(2, 3) == 6\n"
                ),
            )
        })
        .collect();
    let mut base: Vec<(&str, &str)> = vec![
        ("shop/__init__.py", ""),
        ("shop/pricing.py", "def total(p, q):\n    return p * q\n"),
        (
            "shop/checkout.py",
            "from shop.pricing import total\n\n\ndef charge(p, q):\n    return total(p, q)\n",
        ),
    ];
    base.extend(tests.iter().map(|(p, c)| (p.as_str(), c.as_str())));
    let tmp = repo(
        &base,
        &[(
            "shop/pricing.py",
            "def total(p, q):\n    return round(p * q, 2)\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let row = &doc["definitions"][0]["rows"][0];
    let guard = &row["guard"];
    assert_eq!(guard["state"], "linked", "{doc}");
    assert_eq!(guard["test"], "tests/test_a.py", "{doc}");
    assert_eq!(guard["name"], "test_a", "{doc}");
    let also: Vec<(&str, &str)> = guard["also"]
        .as_array()
        .expect("also")
        .iter()
        .map(|t| (t["test"].as_str().unwrap(), t["name"].as_str().unwrap()))
        .collect();
    assert_eq!(
        also,
        [("tests/test_b.py", "test_b"), ("tests/test_c.py", "test_c")],
        "{doc}"
    );
    assert_eq!(guard["more"], 2, "{doc}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("reachable from tests: tests/test_a.py::test_a, tests/test_b.py::test_b, tests/test_c.py::test_c and 2 more"),
        "{md}"
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
    let changed = doc["definitions"].as_array().expect("changed");
    let core = changed
        .iter()
        .find(|c| c["symbol"] == "core")
        .expect("the changed function is listed");
    let callers = core["rows"].as_array().expect("callers");
    let render = callers
        .iter()
        .find(|c| c["symbol"] == "render")
        .unwrap_or_else(|| panic!("render calls core: {callers:?}"));
    assert!(
        render["guard"]["test"].is_null(),
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
    let callers = doc["definitions"][0]["rows"].as_array().expect("callers");
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
    let total = doc["definitions"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["symbol"] == "total")
        .unwrap_or_else(|| panic!("total is listed: {doc}"));
    assert!(
        total["rows"]
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
    assert!(omitted_of(&doc["inventory"]["changed"]) > 0, "{doc}");
    let callers = doc["definitions"][0]["rows"].as_array().expect("callers");
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
        !doc["coverage"]["limits"]
            .as_array()
            .expect("limits")
            .is_empty(),
        "{doc}"
    );
    assert_eq!(doc["empty"], false);
    let out = diffctx_unclocked()
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
    let pack = diffctx_unclocked()
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
    assert!(doc["coverage"].get("limits").is_none(), "{doc}");
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
            doc["coverage"]["limits"]
                .as_array()
                .is_some_and(|l| l.iter().any(|r| r == reason)),
            "{env}: {doc:#}"
        );
    }
}

/// #447, closed on its own shape: under the edge-contribution cap the
/// caller list does not change from run to run, and a cap that fires is named.
#[test]
fn a_run_under_the_contribution_cap_lists_the_same_callers_each_time() {
    let mut files: Vec<(String, String)> = vec![(
        "core/compute.py".to_string(),
        "def compute(x):\n    return x\n".to_string(),
    )];
    for i in 0..40 {
        files.push((
            format!("users/user_{i}.py"),
            format!("from core.compute import compute\n\n\ndef use_{i}(x):\n    return compute(x) + {i}\n"),
        ));
    }
    let base: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let tmp = repo(
        &base,
        &[("core/compute.py", "def compute(x):\n    return x * 2\n")],
    );
    let listed = |doc: &serde_json::Value| -> Vec<String> {
        let mut found: Vec<String> = doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|c| c["rows"].as_array().into_iter().flatten())
            .filter_map(|c| c["path"].as_str().map(str::to_string))
            .collect();
        found.sort();
        found
    };
    let mut fired = false;
    for cap in ["20", "200"] {
        let env = [("DIFFCTX_MAX_EDGE_CONTRIBUTIONS", cap)];
        // At once: the variation needed concurrent load to show.
        let runs: Vec<serde_json::Value> = std::thread::scope(|s| {
            let runs: Vec<_> = (0..4)
                .map(|_| s.spawn(|| impact_env(tmp.path(), "HEAD~1..HEAD", &env)))
                .collect();
            runs.into_iter().map(|r| r.join().unwrap()).collect()
        });
        let first = &runs[0];
        fired |= first["coverage"]["limits"]
            .as_array()
            .is_some_and(|l| l.iter().any(|r| r == "edge_contribution_limit"));
        for again in &runs[1..] {
            assert_eq!(listed(again), listed(first), "cap {cap}: {again:#}");
            assert_eq!(
                again["coverage"]["limits"], first["coverage"]["limits"],
                "cap {cap}: {again:#}"
            );
        }
    }
    assert!(fired, "no cap fired, so this measures nothing");
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
    let contracts = doc["contracts"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(!contracts.iter().any(|c| c["kind"] == "schema"), "{doc}");
}

/// #413: the Rails schema dump and its migrations are schema contracts; a
/// package manager's `versions/` directory is not.
#[test]
fn rails_schema_and_migrations_are_contracts_and_yarn_versions_are_not() {
    let schema_kinds = |files: &[(&str, &str, &str)]| {
        let tmp = repo(
            &files.iter().map(|(p, a, _)| (*p, *a)).collect::<Vec<_>>(),
            &files.iter().map(|(p, _, b)| (*p, *b)).collect::<Vec<_>>(),
        );
        let doc = impact(tmp.path(), "HEAD~1..HEAD");
        doc["contracts"]["items"]
            .as_array()
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
    let clamp = doc["definitions"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["symbol"] == "clamp")
        .unwrap_or_else(|| panic!("clamp is a changed symbol: {doc}"));
    assert_eq!(clamp["rows"].as_array().expect("callers").len(), 3, "{doc}");
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
        doc["contracts"]["items"]
            .as_array()
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
        body["definitions"]
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
    let contracts: Vec<&str> = doc["contracts"]["items"]
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
    let total = doc["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "total")
        .expect("total listed");
    let paths: Vec<&str> = total["rows"]
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
    let total = doc["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "total")
        .unwrap_or_else(|| panic!("the function, not its inner let, is the changed symbol: {doc}"));
    let callers: Vec<&str> = total["rows"]
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
    assert!(doc["definitions"].as_array().unwrap().is_empty());
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
    assert_eq!(doc["range"]["commits"], 2);
    let total = doc["definitions"]
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
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args(["--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["range"]["kind"], "worktree");
    assert_eq!(doc["range"]["spec"], "HEAD");
    assert_eq!(doc["empty"], false);
    assert!(
        doc["range"].get("commits").is_none(),
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
    let out = diffctx_unclocked()
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
    assert!(doc["summary"]["shed"].as_u64().unwrap() > 0, "{doc}");
    let listed = doc["definitions"][0]["rows"].as_array().unwrap().len();
    assert!(listed > 5, "kept the strongest callers, not none: {listed}");
}

#[test]
fn md_is_rejected_outside_impact_mode() {
    let tmp = repo_with_callers();
    let out = diffctx_unclocked()
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
        doc["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["symbol"] == sym)
            .flat_map(|c| c["rows"].as_array().unwrap().clone())
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
        !md.contains("reports/summary.py::"),
        "importing another name from the package is not a reference: {md}"
    );
    assert!(
        md.contains("1 read(s) of `Steps` no binding in scope settles (reports/summary.py:5)"),
        "the unbound `Steps` stays a named read, worded as the bare name it is: {md}"
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
            "app/tools.py::helper (4-5) — reachable from tests: tests/test_tools.py::test_tool via public_tool"
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
        doc["inventory"]["deleted"]["items"],
        serde_json::json!(["scripts/build_image.sh"]),
        "{doc:#}"
    );
    let stale: Vec<String> = doc["stale"]["items"]
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

fn changed_symbols(doc: &serde_json::Value) -> Vec<String> {
    doc["definitions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c["symbol"].as_str().unwrap_or("").to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The rows a shed ledger holds: `untested` is a part of them, not more.
fn shed_total(ledger: &serde_json::Value) -> usize {
    ["resolved", "candidate", "name"]
        .iter()
        .map(|k| ledger[k].as_u64().unwrap_or(0) as usize)
        .sum()
}

/// The `path::symbol` of each definition in one `unlisted` list.
fn unlisted_names(doc: &serde_json::Value, list: &str) -> Vec<String> {
    doc["unlisted"][list]["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|d| {
                    format!(
                        "{}::{}",
                        d["path"].as_str().unwrap_or(""),
                        d["symbol"].as_str().unwrap_or("")
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn contract_symbols(doc: &serde_json::Value) -> Vec<String> {
    doc["contracts"]["items"]
        .as_array()
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
    let unlisted = &doc["unlisted"]["no_caller"]["items"][0];
    assert_eq!(unlisted["path"], "shop/report.py", "{doc:#}");
    assert_eq!(unlisted["symbol"], "summarize", "{doc:#}");
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
    let tmp = repo(
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
    let names = |key: &str| unlisted_names(&doc, key);
    assert_eq!(names("no_caller"), ["app/rates.py::quote"], "{doc:#}");
    assert_eq!(names("called_in_diff"), ["app/rates.py::_scale"], "{doc:#}");
    assert_eq!(doc["tests"]["definitions_without_link"]["n"], 2, "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    let (before, inside) = md
        .split_once("Changed, called only from inside the diff, no static test link:")
        .unwrap_or_else(|| panic!("{md}"));
    let (_, nothing) = before
        .split_once("Changed, nothing calls them, no static test link:")
        .unwrap_or_else(|| panic!("{md}"));
    assert!(nothing.contains("  - app/rates.py::quote ("), "{md}");
    assert!(!nothing.contains("_scale"), "{md}");
    assert!(inside.contains("  - app/rates.py::_scale ("), "{md}");
}

/// A Python caller the same range changes is the diff's own line: it is named
/// under the definition it calls as changed with it, never as a caller outside
/// the diff, and the callers outside it are counted without it.
#[test]
fn a_python_caller_changed_in_the_range_is_named_apart_from_the_callers_outside_it() {
    let rates = |k: u32| {
        format!(
            "def _scale(x):\n    return x * {k}\n\n\ndef quote(x):\n    return _scale(x) + {k}\n"
        )
    };
    let tmp = repo(
        &[
            ("app/__init__.py", ""),
            ("app/rates.py", &rates(2)),
            (
                "app/api.py",
                "from app.rates import quote\n\n\ndef handler(x):\n    return quote(x)\n",
            ),
        ],
        &[("app/rates.py", &rates(3))],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let changed = doc["definitions"].as_array().expect("changed");
    let find = |name: &str| {
        changed
            .iter()
            .find(|c| c["symbol"] == name)
            .unwrap_or_else(|| panic!("{name} missing from {doc:#}"))
    };
    let quote = find("quote");
    assert_eq!(quote["rows"][0]["path"], "app/api.py", "{doc:#}");
    assert_eq!(quote["rows"][0]["standing"], "resolved", "{doc:#}");
    let scale = find("_scale");
    assert_eq!(scale["rows"], serde_json::json!([]), "{doc:#}");
    assert!(
        scale["also_changed"][0]["path"]
            .as_str()
            .zip(scale["also_changed"][0]["symbol"].as_str())
            .is_some_and(|(p, s)| format!("{p}::{s}") == "app/rates.py::quote"),
        "{doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.lines()
            .next()
            .is_some_and(|h| h.contains("1 caller(s) outside the diff")),
        "{md}"
    );
    assert!(
        md.contains(
            "- changed, used only by definitions changed in this range: app/rates.py::_scale ("
        ) && md.contains("(used by app/rates.py::quote ("),
        "{md}"
    );
    assert!(!md.contains("called from app/rates.py"), "{md}");
}

/// A Python definition nothing calls is "nothing calls them" only while no use
/// of its name was left unplaced: a read through a receiver nothing types may
/// be one, and the answer says so instead of claiming an absence.
#[test]
fn a_python_absence_is_claimed_only_where_no_use_of_the_name_is_unplaced() {
    let tmp = repo(
        &[
            (
                "lib.py",
                "def helper():\n    return 1\n\n\ndef lone():\n    return 1\n",
            ),
            ("app.py", "def run(box):\n    return box.helper\n"),
            (
                "tests/test_other.py",
                "def test_nothing():\n    assert True\n",
            ),
        ],
        &[(
            "lib.py",
            "def helper():\n    return 2\n\n\ndef lone():\n    return 2\n",
        )],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("callers not resolved: 1 read(s) of `.helper`"),
        "{md}"
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(
        unlisted_names(&doc, "no_caller"),
        ["lib.py::lone"],
        "{doc:#}"
    );
    let helper = doc["definitions"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["symbol"] == "helper"))
        .unwrap_or_else(|| panic!("{doc:#}"));
    assert!(helper.get("guard").is_none(), "{doc:#}");
}

/// The withhold policy holds at the read, so no output surface can name a
/// withheld file: a canary under `.diffctx/ignore` and a secret-named one
/// call every changed and removed name and name the deleted path. Before
/// the policy moved into the `git grep` readers, impact printed both canaries
/// as callers and stale-reference holders while pack and tree mode hid them.
#[test]
fn no_mode_names_a_withheld_file_that_references_the_change() {
    let canary = "from lib import kept, gone\n# see scripts/rotate.sh\nkept(); gone()\n";
    let tmp = repo(
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
        let out = diffctx_unclocked()
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
        doc["inventory"]["withheld"].as_u64().unwrap_or(0) >= 1,
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
    let tmp = repo(&initial, &[("app/target.py", &changed)]);
    let callers = |doc: &serde_json::Value| -> Vec<String> {
        doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["symbol"] == "target")
            .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
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
    let tmp = repo(
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
    assert_eq!(
        doc["coverage"]["unanalysed"]["items"][0]["path"], "big.py",
        "{doc:#}"
    );
    assert_eq!(
        doc["coverage"]["unanalysed"]["items"][0]["reason"], "too_large",
        "{doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("No resolved static callers"), "{md}");
    assert!(md.contains("over the size cap"), "{md}");
}

fn over_the_size_cap(code: &str) -> String {
    let mut text = code.to_string();
    let filler = "# padding line that keeps the file over the changed-file size cap\n";
    while text.len() < 5_100_000 {
        text.push_str(filler);
    }
    text
}

/// A side over the size cap is never parsed, and the answer names the file
/// not analysed rather than reading as "nothing removed, no callers".
fn names_not_analysed(repo: &Path, path: &str) {
    let doc = impact(repo, "HEAD~1..HEAD");
    let rows = doc["coverage"]["unanalysed"]["items"]
        .as_array()
        .expect("unanalysed_files");
    assert!(
        rows.iter()
            .any(|r| r["path"] == path && r["reason"] == "too_large"),
        "{doc:#}"
    );
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(md.contains("over the size cap"), "{md}");
    assert!(!md.contains("No resolved static callers"), "{md}");
}

const BIG_DEFS: &str = "def target(x):\n    return x\n\n\ndef gone():\n    return 1\n\n\n";
const CALLER: &str = "from big import gone\n\n\ndef run():\n    return gone()\n";

#[test]
fn a_base_over_the_size_cap_is_named_not_analysed() {
    let big = over_the_size_cap(BIG_DEFS);
    let tmp = repo(
        &[("big.py", big.as_str()), ("main.py", CALLER)],
        &[("big.py", "def target(x):\n    return x\n")],
    );
    names_not_analysed(tmp.path(), "big.py");
}

#[test]
fn a_deleted_file_over_the_size_cap_is_named_not_analysed() {
    let big = over_the_size_cap(BIG_DEFS);
    let tmp = repo(&[("big.py", big.as_str()), ("main.py", CALLER)], &[]);
    std::fs::remove_file(tmp.path().join("big.py")).expect("rm");
    commit_all(tmp.path(), "drop big");
    names_not_analysed(tmp.path(), "big.py");
}

#[test]
fn a_rename_over_the_size_cap_is_named_not_analysed() {
    let big = over_the_size_cap(BIG_DEFS);
    let tmp = repo(&[("big.py", big.as_str()), ("main.py", CALLER)], &[]);
    git(tmp.path(), &["mv", "big.py", "huge.py"]);
    commit_all(tmp.path(), "rename big");
    names_not_analysed(tmp.path(), "huge.py");
}

/// #498: a comment-only edit is skipped on purpose (a comment changes
/// nothing a caller sees, #370), and the answer says that, instead of
/// "No resolved static callers" about a function with a caller.
#[test]
fn a_comment_only_edit_says_its_callers_were_not_traced() {
    let tmp = repo(
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
    assert_eq!(
        unlisted_names(&doc, "layout_only"),
        ["pay.py::load_x"],
        "{doc:#}"
    );
    assert_eq!(
        doc["unlisted"]["layout_only"]["items"][0]["lines"], "1-4",
        "{doc:#}"
    );
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
    repo(
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
    let json_shed = doc["summary"]["shed"].as_u64().unwrap_or(0);
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
    let small = diffctx_unclocked()
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
    let out = diffctx_unclocked()
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
    assert_eq!(doc["summary"]["shed"], 0, "{doc:#}");
    assert!(doc.get("omitted").is_none(), "{doc:#}");
    assert_eq!(
        doc["definitions"][0]["rows"].as_array().map(Vec::len),
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
    let tmp = repo(&initial, &[("app/defs.py", "def keep():\n    return 1\n")]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let removed: Vec<&serde_json::Value> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "call")
        .collect();
    assert_eq!(removed.len(), 1, "{doc:#}");
    // 40 files of 20 lines each spell the name: more than can be told apart.
    assert_eq!(
        removed[0]["gaps"],
        serde_json::json!([{"kind": "unlanded", "name": "call", "uses": 800}]),
        "{doc:#}"
    );
    assert_eq!(
        removed[0]["coverage"]["calls"]["complete"], false,
        "{doc:#}"
    );
    assert!(
        removed[0]["rows"].as_array().is_none_or(Vec::is_empty),
        "{doc:#}"
    );
}

/// #495: a dependency bump is a change: the lock files are named and the
/// answer is never empty, where it read "No changes to analyse".
#[test]
fn a_lock_file_change_is_named_never_empty() {
    let tmp = repo(
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
    let locks: Vec<&str> = doc["inventory"]["lockfiles"]["items"]
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

/// #641: a lock file is a changed file, in the text header as in the JSON, and
/// `mise.lock` is one like the rest.
#[test]
fn lock_files_are_counted_as_changed_files_and_mise_lock_is_one() {
    let tmp = repo(
        &[("Dockerfile", "FROM alpine\n"), ("uv.lock", "x==1\n")],
        &[
            ("Dockerfile", "FROM alpine:3\n"),
            ("uv.lock", "x==2\n"),
            ("mise.lock", "node = \"20\"\n"),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(doc["summary"]["files"], 3, "{doc:#}");
    let locks: Vec<&str> = doc["inventory"]["lockfiles"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(locks, ["mise.lock", "uv.lock"], "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains(": 3 changed file(s)"), "{md}");
}

/// #387: callers that could not be resolved are not "0 callers" (Scala,
/// which has no binding model to resolve them). The header names them, or
/// one such row reads like the rest among many (#506).
#[test]
fn unresolved_callers_are_not_reported_as_none() {
    let def = |class: &str, n: u32| {
        format!("package app\n\nobject {class} {{\n  def build(): Int = {{\n    {n}\n  }}\n}}\n")
    };
    let tmp = repo(
        &[
            ("src/app/A.scala", &def("A", 1)),
            ("src/app/B.scala", &def("B", 2)),
            ("src/app/C.scala", &def("C", 3)),
        ],
        &[("src/app/A.scala", &def("A", 10))],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        header(&md).contains("callers unresolved for `build`"),
        "{md}"
    );
    assert!(!header(&md).contains(" 0 caller(s)"), "{md}");
}

/// A file `.diffctx/ignore` withholds is never named by impact: not by a
/// definition its hunk removed, not as a deleted file.
#[test]
fn a_withheld_file_is_never_named_by_its_removed_definitions_or_deletion() {
    let tmp = repo(
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
    let tmp = repo(
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

/// A removed definition's range is on the base side; printed bare, a reader
/// opened the new file there and found the next definition, and its caller
/// read as calling something still there (#598). The control, `alpha`,
/// changed in place and keeps its plain range.
#[test]
fn a_removed_definition_says_so_in_the_markdown() {
    let tmp = repo(
        &[
            (
                "config.py",
                "def alpha():\n    return 1\n\n\ndef bravo():\n    return 2\n\n\ndef charlie():\n    return 3\n",
            ),
            (
                "job.py",
                "from config import alpha, bravo\n\n\ndef run():\n    return alpha() + bravo()\n",
            ),
        ],
        &[(
            "config.py",
            "def alpha():\n    return 10\n\n\ndef charlie():\n    return 3\n",
        )],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("- config.py::bravo (removed; was 5-6)"), "{md}");
    assert!(md.contains("- config.py::alpha (1-2)"), "{md}");
    assert!(md.contains("called from job.py::run"), "{md}");
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let removed = doc["definitions"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["path"] == "src/old.rs"))
        .unwrap_or_else(|| panic!("{doc:#}"));
    let callers: Vec<&str> = removed["rows"]
        .as_array()
        .map(|c| c.iter().filter_map(|c| c["path"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(callers, vec!["src/main_use.rs"], "{doc:#}");
}

/// `Owner.name` answers only for that owner's definition, and a path may
/// prefix `Owner::name`.
#[test]
fn a_qualified_symbol_names_its_owner_and_takes_a_path() {
    let tmp = repo(
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
        diffctx_unclocked()
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
    let tmp = repo(
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
    let out = diffctx_unclocked()
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

/// #656: once a symbol has a resolved caller, the files the repository
/// chose to withhold cannot turn the answer into a zero; counting them on
/// every lookup only taught the reader to skip the header.
#[test]
fn a_symbol_with_callers_does_not_count_withheld_files() {
    let tmp = repo(
        &[
            (".diffctx/ignore", "app/tests/\n"),
            ("app/__init__.py", ""),
            ("app/conf.py", "def workers(cpu):\n    return cpu * 2\n"),
            (
                "app/run.py",
                "from app.conf import workers\n\n\ndef start():\n    return workers(4)\n",
            ),
        ],
        &[(
            "app/tests/test_conf.py",
            "from app.conf import workers\n\n\ndef test_workers():\n    assert workers(1) == 2\n",
        )],
    );
    let out = diffctx_unclocked()
        .current_dir(tmp.path())
        .args(["--symbol", "workers", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("app/run.py::start"), "{text}");
    assert!(!text.contains("withheld"), "{text}");
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
        let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
        doc["definitions"]
            .as_array()
            .and_then(|c| c.iter().find(|c| c["symbol"] == name))
            .and_then(|c| c["rows"].as_array())
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
    let tmp = repo(
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
    let changed = doc["definitions"].as_array().expect("changed");
    let page_size = changed
        .iter()
        .find(|c| c["symbol"] == "page_size")
        .unwrap_or_else(|| panic!("page_size missing: {doc:#}"));
    assert!(
        page_size["rows"]
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
    let tmp = repo(
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
    let changed = doc["definitions"].as_array().expect("changed");
    let entry = |name: &str| changed.iter().find(|c| c["symbol"] == name);
    let ty = entry("QueryError").expect("QueryError listed");
    assert!(ty.get("gaps").is_none(), "{doc:#}");
    assert!(
        ty["rows"]
            .as_array()
            .is_some_and(|c| c.iter().any(|c| c["path"] == "src/run.rs")),
        "{doc:#}"
    );
    for (method, tr) in [("fmt", "Display"), ("from", "From")] {
        let m = entry(method).expect("trait method listed");
        assert_eq!(
            m["gaps"],
            serde_json::json!([{"kind": "dispatched", "via": tr}]),
            "{doc:#}"
        );
    }
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("rust files"), "{md}");
}

fn registry_repo(port_after: &str) -> TempDir {
    repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let tmp = repo(
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
    let mut stale: Vec<String> = doc["stale"]["items"]
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
    let tmp = repo(
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
    let out = diffctx_unclocked()
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
    let floor = repo(
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
    let swapped = repo(
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
    let template = repo(
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
    let sql = repo(
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
    let reordered = repo(
        &[("r.py", "A = 1\nB = 2\nC = 3\nD = 4\nE = 5\nF = 6\nG = 7\n")],
        &[("r.py", "B = 2\nC = 3\nD = 4\nE = 5\nF = 6\nG = 7\nA = 1\n")],
    );
    assert_ne!(change_class_of(reordered.path(), "r.py"), "layout");
}

/// Only an appended parameter or field that brings its own default leaves
/// call sites working (#388); anything else lists them.
#[test]
fn breaking_insertions_are_not_signature_compatible() {
    let mid = repo(
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
    let callback = repo(
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
    let required = repo(
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
    let body = repo(
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
    let slots = repo(
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
    let tmp = repo(
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
    let mut stale: Vec<String> = doc["stale"]["items"]
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
    let tmp = repo(
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
    let ts = repo(
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
    // The receiver is typed `Store`, so at the base the call resolved to the
    // method: a use the head still spells is a caller that broke, not a
    // possible one, and never a word match.
    assert!(
        md.contains("called from use.ts::persist")
            && !md.contains("possibly called from use.ts::persist")
            && !md.contains("use.ts:4 names save"),
        "{md}"
    );
    let rb = repo(
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
    let tmp = repo(
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
    let tail = repo(
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
    // One runtime with no binding model (Scala; Java's resolves `Config` in
    // its own package), so the link is the lexical one the manifest stop
    // governs (a test in another runtime links nothing, #487). `quiz` names
    // its own `Config`, never importing `web.Config`.
    let files = |root_manifest: bool| {
        let mut f = vec![
            (
                "web/pom.xml",
                "<project><artifactId>web</artifactId></project>\n",
            ),
            (
                "web/src/main/scala/web/Contracts.scala",
                "package web\n\nobject Contracts {\n  def theme(): String = {\n    \"dark\"\n  }\n}\n",
            ),
            (
                "web/src/main/scala/web/Config.scala",
                "package web\n\nobject Config {\n  def generated(): String = {\n    Contracts.theme()\n  }\n}\n",
            ),
            (
                "quiz/src/test/scala/quiz/ConfigTest.scala",
                "package quiz\n\nclass ConfigTest {\n  def testGenerated(): Unit = {\n    assert(Config.generated() == \"dark\")\n  }\n}\n",
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
        "web/src/main/scala/web/Contracts.scala",
        "package web\n\nobject Contracts {\n  def theme(): String = {\n    \"light\"\n  }\n}\n",
    )];
    // One project: the lexical link stands, so the probe below is not vacuous.
    // The word match is listed as text; its link stays in the JSON row.
    let tests_of_the_namer = |doc: &serde_json::Value| -> Vec<String> {
        doc["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["rows"].as_array().unwrap())
            .filter(|c| c["standing"] == "name")
            .filter_map(|c| c["guard"]["test"].as_str().map(str::to_string))
            .collect()
    };
    let together = repo(&files(false), &change);
    let doc = impact(together.path(), "HEAD~1..HEAD");
    assert_eq!(
        tests_of_the_namer(&doc),
        ["quiz/src/test/scala/quiz/ConfigTest.scala"],
        "{doc}"
    );
    let apart = repo(&files(true), &change);
    let doc = impact(apart.path(), "HEAD~1..HEAD");
    assert!(tests_of_the_namer(&doc).is_empty(), "{doc}");
    let md = impact_markdown(apart.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("web/src/main/scala/web/Config.scala:5 names theme"),
        "{md}"
    );
    assert!(!md.contains("ConfigTest.scala"), "{md}");
}

/// A sibling module of one language that imports the definition by its
/// qualified name and declares the dependency is a real one: a Maven `it/`
/// module tests `core/`. Without the declaration it could not compile
/// against it, and an undeclared import is no caller (the Go and Rust
/// `ws_*` cases).
#[test]
fn a_sibling_module_importing_the_qualified_name_still_guards_it() {
    let pricing = |rate: &str| {
        format!(
            "package com.acme.core;\n\npublic class Pricing {{\n    public static int gross(int a) {{\n        return a * {rate};\n    }}\n}}\n"
        )
    };
    let tmp = repo(
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
                "<project><artifactId>it</artifactId><dependencies><dependency><artifactId>core</artifactId></dependency></dependencies></project>\n",
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
        let repo = repo(&[(path, old)], &[(path, new)]);
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
    let tmp = repo(
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
                "import log\n\n\ndef fetch(session):\n    return session.headers()\n",
            ),
            (
                "stray.py",
                "def poll(session):\n    return session.headers()\n",
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
        "an untyped call in a module that imports `log` stays possible: {md}"
    );
    assert!(
        !md.contains("stray.py::"),
        "a script that never imports `log` is no possible caller (gh#666): {md}"
    );
    assert!(
        md.contains("possibly called from client.py:5, stray.py:2"),
        "the gap names the two reads its count holds: {md}"
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
    let tmp = repo(
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
    let unanalysed: Vec<(String, String)> = doc["coverage"]["unanalysed"]["items"]
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
    // `tools/run.py` is parsed whole, and its changed top-level statement holds no
    // definition: what it changes is not traced, which the answer says. A file the
    // run did not analyse whole has its own reason and is never listed twice.
    assert_eq!(
        unanalysed,
        vec![
            (
                "tools/run.py".to_string(),
                "outside_definitions".to_string()
            ),
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
    let tmp = repo(
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
        diffctx_unclocked()
            .current_dir(tmp.path())
            .args(["--symbol", name, "-q", "-f", "md"])
            .output()
            .expect("run diffctx")
    };
    // The files are listed as text, and the answer still says a definition
    // may sit in them: they were read as text, not analysed.
    let unparsed = symbol("relax_step");
    assert!(unparsed.status.success());
    let out = String::from_utf8_lossy(&unparsed.stdout);
    assert!(
        out.contains("2 file(s) naming it were read as text"),
        "{out}"
    );
    assert!(
        out.contains("2 fortran (no grammar in this build)"),
        "{out}"
    );
    assert!(
        out.contains("num/solver.f90:") && out.contains("num/use.f90:"),
        "{out}"
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
    let tmp = repo(
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
    let out = diffctx_unclocked()
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
    let tmp = repo(
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
    let out = diffctx_unclocked()
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
            out["inventory"]["changed"]["items"],
            serde_json::json!(["shop/pricing.py"]),
            "{range}: {out}"
        );
        for mode in [&["--mode", "pack"][..], &["--mode", "locate"], &["--full"]] {
            let out = diffctx_unclocked()
                .current_dir(repo)
                .args(["--diff", range, "-q", "-f", "json"])
                .args(mode)
                .output()
                .expect("run diffctx");
            assert!(out.status.success(), "{range} {mode:?}");
            let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
            assert_eq!(
                doc["inventory"]["changed"]["items"],
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
        let out = diffctx_unclocked()
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
        out["inventory"]["changed"]["items"],
        serde_json::json!(["README.md", "shop/pricing.py"]),
        "{out}"
    );
    assert_eq!(
        out["inventory"]["untracked"]["items"],
        serde_json::json!(["# Codex conversation"]),
        "{out}"
    );
    let text = impact_markdown(repo, "HEAD");
    assert!(
        text.contains("2 changed file(s), 1 untracked (analysed)"),
        "{text}"
    );
    // Untracked work alone is still work, never "the working tree matches HEAD".
    git(repo, &["stash", "-q"]);
    let alone = impact_markdown(repo, "HEAD");
    assert!(!alone.contains("No changes to analyse"), "{alone}");
    assert!(
        alone.contains("0 changed file(s), 1 untracked (analysed)"),
        "{alone}"
    );
    git(repo, &["stash", "pop", "-q"]);
    write(
        repo,
        "shop/coupon.py",
        "def discount(cart):\n    return 0.9\n",
    );
    git(repo, &["add", "shop/coupon.py"]);
    let out = impact(repo, "HEAD");
    assert!(
        out["inventory"]["changed"]["items"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("shop/coupon.py")),
        "a staged new file is a change: {out}"
    );
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
    let stopped =
        |doc: &serde_json::Value| doc["coverage"]["limits"].to_string().contains("deadline");
    let sweep = support::sweep_polls(
        |n| {
            impact_env(
                repo,
                "HEAD~1..HEAD",
                &[("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", &n.to_string())],
            )
        },
        |doc| !stopped(doc),
    );
    for (n, doc) in sweep {
        let got = claims(&doc);
        assert!(
            got.iter().all(|c| truth.contains(c)),
            "poll {n}: claims beyond the whole run: {got:?} vs {truth:?}"
        );
        if !stopped(&doc) {
            assert_eq!(got, truth, "poll {n}: a smaller answer that looks complete");
        }
    }
}

/// fj#157: the needle searches of an impact run poll the run's limits, so one
/// more poll lets one more search start. They ran outside the run's scope: a
/// stopped run still started every chunk. Counted in git calls, not timed.
#[cfg(unix)]
#[test]
fn a_stopped_impact_run_starts_one_needle_search_per_poll() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for factor in [1, 2] {
        let rates: String = (0..120)
            .map(|i| format!("def rate_{i:03}(x):\n    return x * {factor}\n\n\n"))
            .collect();
        write(repo, "shop/rates.py", &rates);
        commit_all(repo, "rates");
    }
    let dir = TempDir::new().expect("shim dir");
    let shim = dir.path().join("git");
    std::fs::write(&shim, support::fake_git::FAKE_GIT).expect("write shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let real = support::scrubbed_sh(repo, "command -v git")
        .output()
        .expect("git");
    let real = String::from_utf8(real.stdout).expect("utf8");
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let sweep = support::sweep_polls(
        |n| {
            let log = dir.path().join(format!("calls-{n}"));
            let polls = n.to_string();
            let env = [
                ("PATH", path.as_str()),
                ("REAL_GIT", real.trim()),
                ("FAKE_GIT_CALLS", log.to_str().expect("utf8")),
                ("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", polls.as_str()),
            ];
            let doc = impact_env(repo, "HEAD~1..HEAD", &env);
            let calls = std::fs::read_to_string(&log).unwrap_or_default();
            let searches = calls.lines().filter(|l| l.contains("-I -F -f")).count();
            (
                doc["coverage"]["limits"].to_string().contains("deadline"),
                searches,
            )
        },
        |(stopped, _)| !stopped,
    );
    let searches: Vec<usize> = sweep.iter().map(|(_, (_, count))| *count).collect();
    assert!(searches.last().is_some_and(|&c| c >= 3), "{searches:?}");
    assert!(
        searches.windows(2).all(|w| w[1] <= w[0] + 1),
        "{searches:?}"
    );
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
    let read = |doc: &serde_json::Value| doc["stale"]["total"].as_u64().unwrap_or(0);
    assert_eq!(read(&whole), 200, "{whole}");
    let stopped = impact_env(
        repo,
        "HEAD~1..HEAD",
        &[("DIFFCTX_TEST_DEADLINE_EXPIRED", "1")],
    );
    assert!(
        stopped["coverage"]["limits"]
            .to_string()
            .contains("deadline"),
        "{stopped}"
    );
    assert_eq!(read(&stopped), 0, "a stopped run read on: {stopped}");
}

/// What a run's profile line counted under `name`, as the profile telemetry
/// records it: the run's own work, which no clock enters.
fn profiled(repo: &Path, range: &str, polls: u64, name: &str) -> (serde_json::Value, u64) {
    let dir = TempDir::new().expect("profile dir");
    let profile = dir.path().join("run.jsonl");
    let polls = polls.to_string();
    let doc = impact_env(
        repo,
        range,
        &[
            ("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", polls.as_str()),
            ("DIFFCTX_PROFILE", profile.to_str().expect("utf8")),
        ],
    );
    let line = std::fs::read_to_string(&profile).expect("a profile line");
    let counted = serde_json::from_str::<serde_json::Value>(line.lines().next().expect("a line"))
        .expect("profile JSON")["counts"][name]
        .as_u64()
        .unwrap_or(0);
    (doc, counted)
}

fn stopped_by_the_deadline(doc: &serde_json::Value) -> bool {
    doc["coverage"]["limits"].to_string().contains("deadline")
}

/// A run stopped before it searched most of its definitions names them as one
/// count, in one step of the fit: a step for each rescanned the whole answer,
/// and 7 500 definitions ran a deadline of 60 s to 70 s. Counted in steps,
/// whatever the stop poll.
#[test]
fn a_stopped_run_sheds_the_definitions_it_never_searched_in_one_step() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for factor in [1, 2] {
        let rates: String = (0..80)
            .map(|i| format!("def rate_{i:03}(x):\n    return x * {factor}\n\n\n"))
            .collect();
        write(repo, "shop/rates.py", &rates);
        commit_all(repo, "rates");
    }
    let sweep = support::sweep_polls(
        |n| profiled(repo, "HEAD~1..HEAD", n, "fit.steps"),
        |(doc, _)| !stopped_by_the_deadline(doc),
    );
    let stopped: Vec<(u64, u64)> = sweep
        .iter()
        .filter(|(_, (doc, _))| stopped_by_the_deadline(doc))
        .map(|(polls, (_, steps))| (*polls, *steps))
        .collect();
    assert!(
        stopped.iter().any(|&(_, steps)| steps > 0),
        "no stopped run was fitted: {stopped:?}"
    );
    assert!(
        stopped.iter().all(|&(polls, steps)| steps <= polls + 30),
        "a step per definition it never searched: {stopped:?}"
    );
}

/// The definitions the facts layer reports that no fragment stands for (the
/// fields of one struct) are traced one at a time, and one more poll lets one
/// more of them be traced: the loop had none, so a run stopped before it traced
/// them all. Counted in definitions traced, not timed.
#[test]
fn a_stopped_run_traces_one_more_definition_per_poll() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for wide in ["i32", "i64"] {
        let fields: String = (0..60)
            .map(|i| format!("    pub f{i:02}: {wide},\n"))
            .collect();
        write(
            repo,
            "src/config.rs",
            &format!("pub struct Config {{\n{fields}}}\n"),
        );
        commit_all(repo, "config");
    }
    let sweep = support::sweep_polls(
        |n| profiled(repo, "HEAD~1..HEAD", n, "impact.traced_apart"),
        |(doc, _)| !stopped_by_the_deadline(doc),
    );
    let traced: Vec<u64> = sweep.iter().map(|(_, (_, traced))| *traced).collect();
    assert!(traced.last().is_some_and(|&t| t >= 30), "{traced:?}");
    assert!(
        traced.first().is_some_and(|&t| t < 30),
        "a run stopped at once still traced them: {traced:?}"
    );
    assert!(
        traced.windows(2).all(|w| w[1] <= w[0] + 1),
        "a poll let more than one definition be traced: {traced:?}"
    );
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
        out["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["symbol"] == symbol)
            .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
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
    let tmp = repo(
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
    let mut removed: Vec<String> = doc["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["change"]["kind"] == "removed")
        .map(|c| c["symbol"].as_str().unwrap().to_string())
        .collect();
    removed.sort();
    // Every removed definition of a cell language is listed, the private one too;
    // only the public one is a contract.
    assert_eq!(removed, ["record", "shown"], "{doc}");
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
    let tmp = repo(
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
    let removed: Vec<(String, Vec<String>)> = doc["definitions"]
        .as_array()
        .unwrap()
        .iter()
        // Every removed definition is listed; the readers are the claim: one that
        // nothing reads has none.
        .filter(|c| c["change"]["kind"] == "removed" && !c["rows"].as_array().unwrap().is_empty())
        .map(|c| {
            (
                c["symbol"].as_str().unwrap().to_string(),
                c["rows"]
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
    let tmp = repo(
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
    let callers: Vec<String> = doc["definitions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["change"]["kind"] == "removed" && c["symbol"] == "helper")
        .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
        .map(|k| k["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(callers, ["src/c.rs"], "{doc}");
}

fn impact_budget_md(repo: &Path, budget: &str) -> String {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "--unfold",
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
    let callers: Vec<&serde_json::Value> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "gone")
        .flat_map(|c| c["rows"].as_array().into_iter().flatten())
        .collect();
    let tested = |path: &str| {
        callers
            .iter()
            .find(|c| c["path"] == path)
            .map(|c| c["guard"]["test"].clone())
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
    let contracts = impact(repo, "HEAD~1..HEAD")["contracts"]["items"].to_string();
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
            &format!("import ledger\n\n\ndef nightly_{i}(w):\n    return w.reconcile_all()\n"),
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
    // At every budget the header counts all twelve guesses, no budget
    // changes it to "unresolved", and while a guess is still listed the
    // contract is too: the guesses are shed first. Tighter, the symbol is
    // named in the one omitted entry.
    let (mut with_guess, mut without) = (0, 0);
    for budget in (100..=200).step_by(10) {
        let md = impact_budget_md(repo, &budget.to_string());
        let header = md.lines().next().unwrap_or("");
        // A guess keeps the callers of `reconcile_all` unresolved: the header says
        // so and counts the twelve guesses either way.
        assert!(
            header.contains("callers unresolved for") && header.ends_with(", 12 possible"),
            "{budget}: {md}"
        );
        if md.contains("::nightly_") {
            with_guess += 1;
            assert!(md.contains("migrations/0002_add.sql"), "{budget}: {md}");
        } else {
            without += 1;
            assert!(md.contains("ledger.py::reconcile_all"), "{budget}: {md}");
        }
        assert!(!md.contains("there are none, and"), "{budget}: {md}");
    }
    assert!(with_guess > 0 && without > 0, "{with_guess} {without}");
}

/// The cap sheds callers, then definitions; a definition it sheds stays as a stub
/// that says so, in the JSON and in the text, because an answer that leaves a
/// changed class out without a word reads as "nothing calls it". Forty changed
/// classes with three callers each do not fit 2000 tokens.
#[test]
fn every_changed_class_appears_with_callers_or_as_an_omitted_stub() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for i in 0..40 {
        write(
            repo,
            &format!("pkg/m{i:02}.py"),
            &format!("class C{i:02}:\n    pass\n"),
        );
        for k in 0..3 {
            write(
                repo,
                &format!("use/u{i:02}_{k}.py"),
                &format!(
                    "from pkg.m{i:02} import C{i:02}\n\n\ndef make_{i:02}_{k}():\n    return C{i:02}()\n"
                ),
            );
        }
    }
    commit_all(repo, "base");
    for i in 0..40 {
        write(
            repo,
            &format!("pkg/m{i:02}.py"),
            &format!("class C{i:02}(object):\n    pass\n"),
        );
    }
    commit_all(repo, "change");
    let doc = impact(repo, "HEAD~1..HEAD");
    let changed = doc["definitions"].as_array().expect("changed");
    for entry in changed {
        assert!(
            entry["rows"].as_array().is_some_and(|c| !c.is_empty()),
            "a listed definition keeps its strongest caller: {entry}"
        );
    }
    let listed = changed.len();
    // The one `omitted` section stands for every definition the cap shed whole.
    let omitted = &doc["omitted"];
    let total = omitted["definitions"]["total"].as_u64().expect("the count") as usize;
    assert_eq!(listed + total, 40, "the count is whole: {doc}");
    assert!(
        total > 0,
        "the shape must overflow the cap to mean anything: {doc}"
    );
    assert!(
        listed >= 1,
        "individual stubs must not eat the budget: {doc}"
    );
    assert!(omitted["needs"].as_u64().is_some_and(|n| n > 0), "{doc}");
    let mut named = Vec::new();
    for definition in omitted["definitions"]["items"].as_array().expect("names") {
        assert_eq!(shed_total(&definition["shed"]), 3, "{definition}");
        let def = &definition["def"];
        named.push(format!("{}::{}", def["path"], def["symbol"]));
    }
    assert!(named.len() <= total, "{doc}");
    assert_eq!(shed_total(&omitted["shed"]), 3 * total, "{omitted}");
    named.sort();
    named.dedup();
    assert_eq!(
        named.len(),
        omitted["definitions"]["items"].as_array().unwrap().len(),
        "a definition is named twice: {doc}"
    );
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(
        md.contains("changed definition(s) omitted: the answer cap"),
        "{md}"
    );
    assert!(md.contains("--budget"), "{md}");
    assert!(md.contains("  - called from "), "{md}");
    assert!(md.contains(".py::C") && md.contains(" (3)"), "{md}");
}

/// A removed definition whose pairing with the head is ambiguous (two new classes
/// each gained a body equal to the one that left) is reported, as not
/// determined, and not dropped: its absence would read as "nothing was removed".
#[test]
fn a_removed_field_with_an_ambiguous_counterpart_is_reported_as_not_determined() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "dec.py",
        "class Decoder:\n    def __init__(self, request):\n        self.request = request\n\n    def decode(self, data):\n        return data\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "dec.py",
        "class Decoder:\n    def decode(self, data):\n        return data\n\n\nclass One:\n    def __init__(self, request):\n        self.request = request\n\n\nclass Two:\n    def __init__(self, request):\n        self.request = request\n",
    );
    commit_all(repo, "change");
    let doc = impact(repo, "HEAD~1..HEAD");
    let gone: Vec<&serde_json::Value> = doc["definitions"]
        .as_array()
        .expect("changed")
        .iter()
        .filter(|c| {
            c["change"]["kind"] == "removed"
                && c["symbol"]
                    .as_str()
                    .is_some_and(|s| s.rsplit('.').next() == Some("request"))
        })
        .collect();
    assert_eq!(gone.len(), 1, "{doc}");
    assert_eq!(gone[0]["lines"], "3-3", "{doc}");
    assert!(
        gone[0]["gaps"].as_array().is_some_and(
            |gaps| gaps.contains(&serde_json::json!({"kind": "unpaired", "name": "request"}))
        ),
        "{doc}"
    );
}

/// R11: a class that lost its constructor is reached through what a constructor
/// reaches: its instantiations and the `super()` call of the constructor a
/// subclass overrides. A signature that names the class and a subclass that
/// only extends it are uses of the class the removal leaves alone.
#[test]
fn a_class_that_lost_its_constructor_lists_what_builds_and_overrides_it() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "dec.py",
        "class Decoder:\n    def __init__(self, request):\n        self.request = request\n\n    def decode(self, data):\n        return data\n",
    );
    write(
        repo,
        "gzip_dec.py",
        "from dec import Decoder\n\n\nclass Gzip(Decoder):\n    def __init__(self, request):\n        super().__init__(request)\n\n    def flush(self):\n        return b\"\"\n",
    );
    write(
        repo,
        "make.py",
        "from dec import Decoder\n\n\ndef build(request):\n    return Decoder(request)\n",
    );
    write(
        repo,
        "plain_dec.py",
        "from dec import Decoder\n\n\nclass Plain(Decoder):\n    def flush(self):\n        return b\"\"\n",
    );
    write(
        repo,
        "pick.py",
        "from dec import Decoder\n\n\ndef pick(items: list[Decoder]):\n    return items[0]\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "dec.py",
        "class Decoder:\n    def decode(self, data):\n        return data\n",
    );
    commit_all(repo, "drop the constructor");
    let doc = impact(repo, "HEAD~1..HEAD");
    let mut paths: Vec<&str> = doc["definitions"]
        .as_array()
        .expect("changed")
        .iter()
        .filter(|c| {
            (c["symbol"] == "Decoder" && c["kind"] == "class")
                || (c["symbol"] == "__init__" && c["change"]["kind"] == "removed")
        })
        .flat_map(|c| c["rows"].as_array().into_iter().flatten())
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    paths.sort_unstable();
    paths.dedup();
    assert_eq!(paths, ["gzip_dec.py", "make.py"], "{doc}");
}

/// R11: an edited constructor reaches the class's instantiations, not the other
/// uses of the class: a signature that names it, an `isinstance` check and the
/// class handed on as a value stay with the class, which the edit leaves alone.
#[test]
fn an_edited_constructor_reaches_instantiations_not_other_uses_of_its_class() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "box.py",
        "class Box:\n    def __init__(self, size):\n        self.size = size\n",
    );
    write(
        repo,
        "make.py",
        "from box import Box\n\n\ndef make(size):\n    return Box(size)\n",
    );
    write(
        repo,
        "describe.py",
        "from box import Box\n\n\ndef describe(box: Box):\n    return box\n",
    );
    write(
        repo,
        "check.py",
        "from box import Box\n\n\ndef is_box(obj):\n    return isinstance(obj, Box)\n",
    );
    write(
        repo,
        "pass_on.py",
        "from box import Box\n\n\ndef factory():\n    return map(Box, [1, 2])\n",
    );
    commit_all(repo, "base");
    write(
        repo,
        "box.py",
        "class Box:\n    def __init__(self, size):\n        self.size = int(size)\n",
    );
    commit_all(repo, "size is an int");
    let doc = impact(repo, "HEAD~1..HEAD");
    let mut paths: Vec<&str> = doc["definitions"]
        .as_array()
        .expect("changed")
        .iter()
        .filter(|c| c["symbol"] == "Box" && c["kind"] == "class")
        .flat_map(|c| c["rows"].as_array().into_iter().flatten())
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    paths.sort_unstable();
    assert_eq!(paths, ["make.py"], "{doc}");
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
    let stale: Vec<String> = impact(repo, "HEAD~1..HEAD")["stale"]["items"]
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

fn engine_version(version: usize) -> String {
    let helpers: String = (0..5)
        .map(|i| format!("def helper_{i}(x):\n    return x + {}\n\n\n", i * version))
        .collect();
    let steps: String = (0..60)
        .map(|i| format!("    step_{i} = helper_{}(rows[{i}]) * {version}\n", i % 5))
        .collect();
    format!("{helpers}def plan(rows):\n{steps}    return step_0\n")
}

/// The answer is fitted to the budget asked: when nothing else can go, the
/// lists of untested definitions and the names of the omitted ones give way to
/// the counts the header and the omitted entry already hold. Helpers called
/// only from `plan`, a rewritten `plan` and a budget near the least an answer
/// can say left them whole at 218 tokens against 150 (the Python door-contract
/// suite runs this with the native binary on one CI job only).
#[test]
fn an_answer_at_a_tight_budget_sheds_its_names_to_fit() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "zsrc/x.py",
        "def compute(a, b):\n    return a + b\n\n\ndef helper(x):\n    return x * 2\n",
    );
    write(
        repo,
        "tests/test_x.py",
        "from zsrc.x import compute\n\n\ndef test_compute():\n    assert compute(1, 2) == 3\n",
    );
    write(repo, "README.md", "# Project\n\nCompute adds numbers.\n");
    write(repo, "app/engine.py", &engine_version(1));
    commit_all(repo, "initial");
    let body: String = (0..40).map(|i| format!("    total += {i} * 0\n")).collect();
    write(
        repo,
        "zsrc/x.py",
        &format!(
            "def compute(a, b):\n    total = a + b\n{body}    return total\n\n\ndef helper(x):\n    return x * 2\n"
        ),
    );
    let asserts: String = (0..25)
        .map(|i| format!("    assert compute({i}, 2) == {i}+2\n"))
        .collect();
    write(
        repo,
        "tests/test_x.py",
        &format!("from zsrc.x import compute\n\n\ndef test_compute():\n{asserts}"),
    );
    let docs: String = (0..20)
        .map(|i| format!("Line {i} of docs about compute.\n"))
        .collect();
    write(
        repo,
        "README.md",
        &format!("# Project\n\nCompute adds numbers.\n\n{docs}"),
    );
    commit_all(repo, "rewrite compute, its tests and docs");
    write(repo, "app/engine.py", &engine_version(2));
    commit_all(repo, "bump the helpers, rewrite plan");
    for budget in ["150", "200", "300"] {
        let out = diffctx_unclocked()
            .current_dir(repo)
            .args([
                "--diff",
                "HEAD~2..HEAD",
                "--mode",
                "impact",
                "-q",
                "--budget",
                budget,
            ])
            .output()
            .expect("run diffctx");
        assert!(out.status.success());
        let text = String::from_utf8(out.stdout).expect("utf8");
        let tokens = _diffctx::tokenizer::count_tokens(&text);
        let budget_tokens = budget.parse::<u32>().unwrap();
        let doc: serde_json::Value = serde_json::from_str(&text).expect("impact JSON");
        // The least a document can say may outgrow a tight budget: it then states
        // what it takes and cuts nothing more.
        assert!(
            tokens <= budget_tokens
                || doc["fit"]["least"]
                    .as_u64()
                    .is_some_and(|least| least > u64::from(budget_tokens)),
            "{budget}: {tokens} tokens: {text}"
        );
        assert_eq!(
            doc["tests"]["definitions_without_link"]["n"], 5,
            "{budget}: {doc:#}"
        );
    }
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
        !doc["coverage"]["limits"]
            .to_string()
            .contains("read_failed"),
        "{}",
        doc["coverage"]["limits"]
    );
    let callers: Vec<String> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        callers,
        vec!["pkg/consumer.py::use"],
        "{}",
        doc["coverage"]["limits"]
    );
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
        doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["symbol"] == symbol)
            .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
            .map(|c| {
                // The row of a removed definition names the line that still
                // spells it; the caller's own span is a fragment's, and a removed
                // definition's rows are the resolver's.
                format!("{}:{}", c["path"].as_str().unwrap(), c["sites"])
            })
            .collect()
    };
    assert_eq!(callers_of("absorb"), Vec::<String>::new(), "{doc}");
    assert_eq!(callers_of("gone"), vec!["src/user.rs:[4]"], "{doc}");
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

/// The cap sheds the weakest row first and a reference weighs less than a
/// call, so a symbol read by many and called by few lost every read before
/// one call. Each relation kind a symbol has keeps its strongest row (weight,
/// then path) until nothing else in the answer can go, and the counts stay
/// whole.
#[test]
fn a_cap_keeps_the_strongest_row_of_each_relation_a_symbol_has() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "core.py", "def rate(x):\n    return x * 2\n");
    for i in 1..=10 {
        write(
            repo,
            &format!("ref_{i}.py"),
            &format!(
                "from core import rate\n\n\ndef order_{i}(xs):\n    return sorted(xs, key=rate)\n"
            ),
        );
    }
    for i in 1..=3 {
        write(
            repo,
            &format!("call_{i}.py"),
            &format!("from core import rate\n\n\ndef price_{i}(x):\n    return rate(x) + {i}\n"),
        );
    }
    commit_all(repo, "base");
    write(repo, "core.py", "def rate(x):\n    return x * 3\n");
    commit_all(repo, "change");
    // Below the least the document can say the definition is shed whole and only
    // its ledger stays; from there up it is listed, and the cap bites until the
    // whole of it fits.
    // The document is charged for its own framing: below 400 tokens here it holds
    // one row, the strongest, and not one of each relation.
    let mut bitten = 0;
    for budget in ["400", "500", "600", "800", "1000"] {
        let out = diffctx_unclocked()
            .current_dir(repo)
            .args([
                "--diff",
                "HEAD~1..HEAD",
                "--mode",
                "impact",
                "-q",
                "--budget",
                budget,
            ])
            .output()
            .expect("run diffctx");
        assert!(out.status.success());
        let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("impact JSON");
        let Some(rate) = doc["definitions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["symbol"] == "rate")
        else {
            assert_eq!(
                shed_total(&doc["omitted"]["shed"]),
                13,
                "{budget}: a definition shed whole keeps its ledger: {doc:#}"
            );
            continue;
        };
        let rows = rate["rows"].as_array().unwrap();
        let paths: Vec<&str> = rows.iter().map(|c| c["path"].as_str().unwrap()).collect();
        let shed = shed_total(&rate["shed"]);
        assert_eq!(rows.len() + shed, 13, "{budget}: {doc:#}");
        assert_eq!(
            doc["summary"]["shed"].as_u64(),
            Some(shed as u64),
            "{budget}"
        );
        if shed > 0 {
            bitten += 1;
            assert!(paths.contains(&"call_1.py"), "{budget}: {paths:?}");
            assert!(paths.contains(&"ref_1.py"), "{budget}: {paths:?}");
        }
    }
    assert!(
        bitten > 0,
        "no budget listed the definition with the cap biting"
    );
}

/// #496: a Java method calling a sibling of its own class with no
/// qualifier is a caller; a comment naming it and a call through another
/// receiver are not.
#[test]
fn an_unqualified_same_class_java_call_is_a_caller() {
    let mapper = "package app;\n\npublic class Mapper {\n    static int map(int x) {\n        return x * 2;\n    }\n\n    public int toDto(int x) {\n        return map(x) + 1;\n    }\n\n    public int noted(int x) {\n        // map(x) is not called here\n        return other.map(x);\n    }\n}\n";
    let tmp = repo(
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
    let callers: Vec<String> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "map")
        .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
        .map(|c| c["symbol"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(callers, vec!["toDto"], "{doc}");
}

/// #484: a file over the fragment cap is not cut into fragments, so its calls
/// were not seen, and the answer said it names the changed definition instead of
/// reading as complete. A language whose facts read it whole (Python, TS/JS,
/// Rust, Kotlin) has nothing unread: the file's call is a caller like any other.
#[test]
fn a_namer_over_the_size_cap_is_a_caller_where_the_facts_read_it_whole() {
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
    assert!(md.contains("called from src/big.rs::uses_it"), "{md}");
    assert!(!md.contains("over the size cap"), "{md}");
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
    let tmp = repo(
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
    let callers: Vec<String> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == "read")
        .flat_map(|c| c["rows"].as_array().cloned().unwrap_or_default())
        .map(|c| c["path"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(callers, vec!["src/c.rs"], "{doc}");
}

/// #536: a deleted function whose name survives only as a parameter, a local
/// variable, or a word inside a multiline docstring has no remaining callers.
#[test]
fn a_removed_function_is_not_linked_to_a_parameter_or_docstring_word() {
    let head_doc =
        "\"\"\"Evaluation helpers.\n\nSplits audio into regions and scores alignment.\n\"\"\"\n";
    let base_eval = format!(
        "{head_doc}\ndef utterances(run_dir):\n    return [run_dir]\n\n\ndef regions(session):\n    return [session]\n\n\ndef score_wer(hyp, ref):\n    return len(hyp) - len(ref)\n\n\ndef align(reference, utterances):\n    return (reference, utterances)\n\n\ndef summarize(session):\n    regions = []\n    return regions\n\n\ndef evaluate(hyp, ref):\n    return score_wer(hyp, ref)\n"
    );
    let head_eval = format!(
        "{head_doc}\ndef align(reference, utterances):\n    return (reference, utterances)\n\n\ndef summarize(session):\n    regions = []\n    return regions\n\n\ndef evaluate(hyp, ref):\n    return score_wer(hyp, ref)\n"
    );
    let tmp = repo(
        &[("pkg/__init__.py", ""), ("pkg/eval.py", &base_eval)],
        &[("pkg/eval.py", &head_eval)],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let removed: Vec<(String, Vec<String>)> = doc["definitions"]
        .as_array()
        .unwrap()
        .iter()
        // Every removed definition is listed; the readers are the claim: one that
        // nothing reads has none.
        .filter(|c| c["change"]["kind"] == "removed" && !c["rows"].as_array().unwrap().is_empty())
        .map(|c| {
            (
                c["symbol"].as_str().unwrap().to_string(),
                c["rows"]
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
            "score_wer".to_string(),
            vec!["pkg/eval.py::evaluate".to_string()]
        )],
        "{doc:#}"
    );
}

/// The verdicts of an impact answer, leaving out which changed files were
/// untracked.
fn verdicts(doc: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "changed": doc["definitions"],
        "no_caller": doc["unlisted"]["no_caller"],
        "contracts": doc["contracts"],
        "stale_references": doc["stale"]["items"],
    })
}

/// #625: uncommitted work is one tree. A new untracked class and the
/// untracked test calling it answered "nothing calls them", and an
/// untracked script naming a deleted path was no stale reference, while the
/// same files staged were read right: git grep searched only tracked files.
#[test]
fn untracked_work_reads_like_staged_work() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(
        repo,
        "src/main/java/app/Repo.java",
        "package app;\npublic interface Repo {\n    String find(int id);\n}\n",
    );
    write(repo, "src/old_helper.py", "def helper():\n    return 1\n");
    commit_all(repo, "base");
    write(
        repo,
        "src/main/java/app/Service.java",
        "package app;\npublic class Service {\n    private final Repo repo;\n    public Service(Repo repo) { this.repo = repo; }\n    public String get(int id) { return repo.find(id); }\n}\n",
    );
    write(
        repo,
        "src/test/java/app/ServiceTest.java",
        "package app;\npublic class ServiceTest {\n    public void testGet() { new Service(id -> \"x\").get(1); }\n}\n",
    );
    git(repo, &["rm", "-q", "src/old_helper.py"]);
    write(
        repo,
        "run_untracked.sh",
        "#!/bin/sh\npython src/old_helper.py\n",
    );
    let untracked = impact(repo, "HEAD");
    git(repo, &["add", "-A"]);
    let staged = impact(repo, "HEAD");
    assert_eq!(verdicts(&untracked), verdicts(&staged), "{untracked:#}");
    let unlisted = unlisted_names(&untracked, "no_caller");
    assert!(
        !unlisted.iter().any(|d| d.ends_with("::get")),
        "{untracked:#}"
    );
    assert!(
        untracked["stale"]["items"]
            .to_string()
            .contains("run_untracked.sh"),
        "{untracked:#}"
    );
}

/// #626: a capture or a document left in the working tree beside a code
/// change is named as not followed; over the size cap it holds no caller,
/// so the answer for the code is not partial. A code file over the cap
/// still is.
#[test]
fn an_over_cap_file_outside_code_leaves_the_answer_whole() {
    let tmp = repo_with_callers();
    let repo = tmp.path();
    write(
        repo,
        "shop/pricing.py",
        "def total(items):\n    return round(sum(i.price for i in items), 2)\n",
    );
    let capture = format!("{{\"log\": \"{}\"}}\n", "x".repeat(6_000_000));
    write(repo, "capture.har", &capture);
    let doc = impact(repo, "HEAD");
    assert!(
        doc["coverage"]["limits"]
            .as_array()
            .is_none_or(Vec::is_empty),
        "{:#}",
        doc["coverage"]["limits"]
    );
    assert!(
        doc["coverage"]["unanalysed"]["items"]
            .to_string()
            .contains("capture.har")
    );
    let text = impact_markdown(repo, "HEAD");
    assert!(!text.contains("partial"), "{text}");
    assert!(!text.contains("code: capture.har"), "{text}");
    std::fs::remove_file(repo.join("capture.har")).unwrap();
    let body = "x = 1\n".repeat(1_000_000);
    write(repo, "shop/generated.py", &body);
    let doc = impact(repo, "HEAD");
    assert!(
        doc["coverage"]["limits"]
            .to_string()
            .contains("file_too_large"),
        "{doc:#}"
    );
}

/// #600: a file still naming a deleted or renamed path is a defect, the
/// one a reader must act on. Under a tight budget it went to a bare count
/// while routine callers printed in full; it now outlives every caller row.
#[test]
fn a_stale_reference_outlives_routine_callers_under_the_budget() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    for n in 0..40 {
        write(
            repo,
            &format!("src/m{n}.ts"),
            &format!("export function draw{n}() {{\n  return {n};\n}}\n"),
        );
        write(
            repo,
            &format!("src/u{n}.ts"),
            &format!(
                "import {{ draw{n} }} from \"./m{n}\";\n\nexport function use{n}() {{\n  return draw{n}();\n}}\n"
            ),
        );
    }
    write(repo, "src/old_helper.ts", "export const helper = 1;\n");
    for n in 0..6 {
        write(repo, &format!("docs/note{n}.md"), "See src/old_helper.ts\n");
    }
    commit_all(repo, "base");
    for n in 0..40 {
        write(
            repo,
            &format!("src/m{n}.ts"),
            &format!("export function draw{n}() {{\n  return {n} + 1;\n}}\n"),
        );
    }
    git(repo, &["mv", "src/old_helper.ts", "src/helper.ts"]);
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args([
            "--diff", "--mode", "impact", "-q", "-f", "md", "--budget", "600",
        ])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    for n in 0..6 {
        assert!(text.contains(&format!("docs/note{n}.md")), "{text}");
    }
    // The shed callers are disclosed: in the trailer, or in the omitted entry
    // that carries them, which states how many.
    assert!(
        text.contains("weaker caller(s) omitted") || text.contains("caller(s) are not listed)"),
        "{text}"
    );
}

/// The cap deals rows out round-robin over (definition, relation kind): a
/// constant read in many files and a function called from as many keep rows in
/// proportion, neither kind is shed before the other, and a larger budget
/// lists a superset of a smaller one's rows.
#[test]
fn a_cap_deals_rows_round_robin_and_a_larger_budget_lists_a_superset() {
    const FILES: usize = 24;
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "pkg/__init__.py", "");
    write(repo, "pkg/settings.py", "MAX_RETRIES = 3\n");
    write(repo, "pkg/calc.py", "def compute(x):\n    return x\n");
    for i in 0..FILES {
        write(
            repo,
            &format!("pkg/readers/r{i:02}.py"),
            &format!(
                "from pkg.settings import MAX_RETRIES\n\n\ndef read_{i:02}():\n    return MAX_RETRIES + {i}\n"
            ),
        );
        write(
            repo,
            &format!("pkg/callers/c{i:02}.py"),
            &format!(
                "from pkg.calc import compute\n\n\ndef call_{i:02}():\n    return compute({i})\n"
            ),
        );
    }
    commit_all(repo, "base");
    write(repo, "pkg/settings.py", "MAX_RETRIES = 5\n");
    write(repo, "pkg/calc.py", "def compute(x):\n    return x + 1\n");
    commit_all(repo, "change");
    let answer = |budget: u32| {
        let out = diffctx_unclocked()
            .current_dir(repo)
            .args([
                "--diff",
                "HEAD~1..HEAD",
                "--mode",
                "impact",
                "-q",
                "--budget",
                &budget.to_string(),
            ])
            .output()
            .expect("run diffctx");
        assert!(out.status.success());
        String::from_utf8(out.stdout).expect("utf8")
    };
    let rows = |text: &str, symbol: &str| -> Vec<String> {
        let doc: serde_json::Value = serde_json::from_str(text).expect("impact JSON");
        doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["symbol"] == symbol)
            .flat_map(|c| c["rows"].as_array().into_iter().flatten())
            .map(|k| k["path"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let whole = answer(1_000_000);
    for symbol in ["MAX_RETRIES", "compute"] {
        assert_eq!(rows(&whole, symbol).len(), FILES, "{symbol}: {whole}");
    }
    let half = _diffctx::tokenizer::count_tokens(&whole) / 2;
    let mut shown: Vec<(Vec<String>, Vec<String>)> = Vec::new();
    for budget in [half / 2, half, half + half / 2, 2 * half] {
        let text = answer(budget);
        let (reads, calls) = (rows(&text, "MAX_RETRIES"), rows(&text, "compute"));
        if let Some((earlier_reads, earlier_calls)) = shown.last() {
            assert!(
                earlier_reads.iter().all(|r| reads.contains(r))
                    && earlier_calls.iter().all(|c| calls.contains(c)),
                "{budget}: a larger budget dropped a row: {text}"
            );
        }
        if budget == half {
            assert!(
                reads.len() >= FILES / 4 && calls.len() >= FILES / 4,
                "{budget}: both kinds keep rows: {text}"
            );
            assert!(
                reads.len() < FILES && calls.len() < FILES,
                "{budget}: half the rows must overflow the cap: {text}"
            );
            assert!(
                reads.len().abs_diff(calls.len()) <= 2,
                "{budget}: {} reads against {} calls: {text}",
                reads.len(),
                calls.len()
            );
        }
        shown.push((reads, calls));
    }
}

/// One config per host, `lines` addresses long, all spelled with `noun`.
fn host_config(host: usize, noun: &str, lines: usize) -> String {
    (0..lines)
        .map(|k| format!("url_{k}: http://svc-{host:02}.example.org:8080/v1/{noun}/{k}\n"))
        .collect()
}

/// Forgejo #109: the values of every changed config are search terms, and
/// `git grep` matches each against every line, so an epoch of hundreds of
/// configs ran a 10 s answer past 300 s. The search takes the smallest
/// changes first, a bounded number of terms from each, and counts the
/// configs it left out, which stay "not followed".
#[test]
fn a_key_search_over_many_changed_configs_is_capped_and_counts_the_rest() {
    let sizes = |noun: &str| -> Vec<(String, String)> {
        (0..20)
            .map(|host| {
                let lines = if host == 0 { 1 } else { 20 };
                (
                    format!("cfg/svc_{host:02}.yaml"),
                    host_config(host, noun, lines),
                )
            })
            .collect()
    };
    let readers = [
        (
            "app/dashboard.yaml".to_string(),
            "target: http://svc-00.example.org:8080/v1/items/0\n".to_string(),
        ),
        (
            "app/other.yaml".to_string(),
            "target: http://svc-19.example.org:8080/v1/items/0\n".to_string(),
        ),
    ];
    let before: Vec<(String, String)> = sizes("items").into_iter().chain(readers).collect();
    let after = sizes("things");
    let before: Vec<(&str, &str)> = before
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let after: Vec<(&str, &str)> = after
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let tmp = repo(&before, &after);

    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains(
            "app/dashboard.yaml:1 names http://svc-00.example.org:8080/v1/items/0 (cfg/svc_00.yaml)"
        ),
        "{md}"
    );
    assert!(
        md.contains(
            "(key search capped: the keys and values of 14 changed config(s) were not searched)"
        ),
        "{md}"
    );
    assert!(!md.contains("app/other.yaml"), "{md}");
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(
        doc["named"]["key_searches"].as_array().map(Vec::len),
        Some(6),
        "{doc}"
    );
    assert!(
        !doc["named"]["key_searches"].to_string().contains("svc_19"),
        "the largest change was searched: {doc}"
    );
}

/// Forgejo #109: a run stopped at any poll inside the key search returns,
/// names the deadline, and lists no line the whole run does not.
#[test]
fn a_key_search_stopped_at_any_poll_answers_a_subset_or_the_whole() {
    let configs = |noun: &str| -> Vec<(String, String)> {
        (0..4)
            .map(|host| {
                (
                    format!("cfg/svc_{host:02}.yaml"),
                    host_config(host, noun, 2),
                )
            })
            .collect()
    };
    let readers = [(
        "app/dashboard.yaml".to_string(),
        "a: http://svc-00.example.org:8080/v1/items/0\nb: http://svc-02.example.org:8080/v1/items/1\n"
            .to_string(),
    )];
    let before: Vec<(String, String)> = configs("items").into_iter().chain(readers).collect();
    let after = configs("things");
    let before: Vec<(&str, &str)> = before
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let after: Vec<(&str, &str)> = after
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let tmp = repo(&before, &after);
    let repo = tmp.path();

    let named = |doc: &serde_json::Value| -> Vec<String> {
        doc["named"]["by_key"]["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|r| format!("{}:{} {}", r["path"], r["line"], r["key"]))
            .collect()
    };
    let truth = named(&impact(repo, "HEAD~1..HEAD"));
    assert_eq!(truth.len(), 2, "{truth:?}");
    let stopped =
        |doc: &serde_json::Value| doc["coverage"]["limits"].to_string().contains("deadline");
    let sweep = support::sweep_polls(
        |n| {
            impact_env(
                repo,
                "HEAD~1..HEAD",
                &[("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", &n.to_string())],
            )
        },
        |doc| !stopped(doc),
    );
    assert!(sweep.len() > 1, "no poll stopped the run");
    for (n, doc) in sweep {
        let got = named(&doc);
        assert!(
            got.iter().all(|c| truth.contains(c)),
            "poll {n}: lines beyond the whole run: {got:?} vs {truth:?}"
        );
        if !stopped(&doc) {
            assert_eq!(got, truth, "poll {n}: a smaller answer that looks complete");
        }
    }
}

/// `path::symbol (lines)` of every caller listed under the definition of
/// `name` in `path`, with its evidence.
fn listed_callers(
    doc: &serde_json::Value,
    path: &str,
    name: &str,
) -> std::collections::BTreeSet<String> {
    let def = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["path"] == path && c["symbol"] == name)
        .unwrap_or_else(|| panic!("{name} missing from {doc:#}"));
    def["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{} {} {}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or(""),
                c["lines"].as_str().unwrap_or(""),
                c["standing"].as_str().unwrap_or("")
            )
        })
        .collect()
}

/// Impact and `--symbol` read one resolver: the callers of a definition are
/// the same set in both, here a method called through `this` from another
/// method of its class and from a derived class's.
#[test]
fn impact_and_symbol_name_the_same_callers_of_a_definition() {
    let base = |body: &str| {
        format!(
            "export class Base {{\n  protected normalize(x: number): number {{\n    return {body};\n  }}\n\n  run(x: number): number {{\n    return this.normalize(x);\n  }}\n}}\n"
        )
    };
    let tmp = repo(
        &[
            ("src/base.ts", &base("x")),
            (
                "src/derived.ts",
                "import { Base } from './base';\n\nexport class Derived extends Base {\n  go(x: number): number {\n    return this.normalize(x) + 1;\n  }\n}\n",
            ),
        ],
        &[("src/base.ts", &base("x + 0"))],
    );
    let from_impact = impact(tmp.path(), "HEAD~1..HEAD");
    let out = diffctx_unclocked()
        .current_dir(tmp.path())
        .args(["--symbol", "normalize", "-q"])
        .output()
        .expect("run diffctx");
    assert!(out.status.success());
    let from_symbol: serde_json::Value = serde_json::from_slice(&out.stdout).expect("symbol JSON");
    let named = listed_callers(&from_impact, "src/base.ts", "normalize");
    assert_eq!(named.len(), 2, "{from_impact:#}");
    assert_eq!(
        named,
        listed_callers(&from_symbol, "src/base.ts", "normalize"),
        "{from_impact:#}\n{from_symbol:#}"
    );
}

/// A cap that drops callers says so on the definition it dropped them from,
/// so a shorter list is never read as the whole one.
#[test]
fn a_definition_whose_callers_the_cap_dropped_counts_them() {
    let shared = |body: &str| {
        format!("export function shared(x: number): number {{\n  return {body};\n}}\n")
    };
    let callers: Vec<(String, String)> = (0..150)
        .map(|i| {
            (
                format!("src/callers/caller_{i}.ts"),
                format!(
                    "import {{ shared }} from '../shared';\n\nexport function caller_{i}(): number {{\n  return shared({i});\n}}\n"
                ),
            )
        })
        .collect();
    let mut base: Vec<(&str, &str)> = vec![];
    let first = shared("x");
    base.push(("src/shared.ts", first.as_str()));
    for (path, text) in &callers {
        base.push((path.as_str(), text.as_str()));
    }
    let changed = shared("x + 0");
    let tmp = repo(&base, &[("src/shared.ts", changed.as_str())]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let def = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["symbol"] == "shared")
        .unwrap_or_else(|| panic!("shared missing from {doc:#}"));
    let listed = def["rows"].as_array().map_or(0, Vec::len);
    let omitted = shed_total(&def["shed"]);
    assert!(omitted > 0, "the cap kept all {listed} callers: {doc:#}");
    assert_eq!(listed + omitted, 150, "{doc:#}");
}

fn symbol_doc(repo: &Path, name: &str) -> serde_json::Value {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args(["--symbol", name, "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("symbol JSON")
}

/// A queried name has no diff: `exports.f = f` is another definition named `f`
/// whose lines the run anchors on, and the use of `f` there is a caller of the
/// function, never "changed in this diff".
#[test]
fn a_use_inside_a_second_definition_of_the_queried_name_is_a_caller() {
    for (assigned, expected) in [("f", 1), ("g", 1)] {
        let tmp = repo(
            &[(
                "a.js",
                &format!("function f(x) {{\n  return 1;\n}}\nexports.{assigned} = f;\n"),
            )],
            &[],
        );
        let doc = symbol_doc(tmp.path(), "f");
        let function = doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["lines"] == "1-3")
            .unwrap_or_else(|| panic!("the function is missing from {doc:#}"));
        assert_eq!(
            function["rows"].as_array().map_or(0, Vec::len),
            expected,
            "exports.{assigned}: {doc:#}"
        );
        assert!(function.get("also_changed").is_none(), "{doc:#}");
    }
}

/// A name no tracked definition stands for is said, in the JSON as in the
/// Markdown, never answered with an empty list that reads as "no callers": a
/// `const` in a module-level `for` body is a block binding the facts hold no
/// definition for.
#[test]
fn a_block_scoped_binding_is_named_untracked_not_called_by_nothing() {
    let tmp = repo(
        &[
            (
                "src/settings.ts",
                "export const options = { retries: 1 };\n",
            ),
            (
                "test/each.ts",
                "const modes = ['a', 'b'];\n\nfor (const mode of modes) {\n  const options = { mode };\n  run(() => use({ ...options }));\n}\n\nfunction run(f: () => void) {\n  f();\n}\n\nfunction use(x: object) {\n  return x;\n}\n",
            ),
        ],
        &[],
    );
    let sentence = "`options` is not a tracked definition here (a block-scoped binding); its uses are not resolved";
    let doc = symbol_doc(tmp.path(), "options");
    let untracked = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["path"] == "test/each.ts")
        .unwrap_or_else(|| panic!("the block binding is missing from {doc:#}"));
    assert!(
        untracked["gaps"].as_array().is_some_and(|gaps| gaps
            .iter()
            .any(|g| g["kind"] == "untracked" && g["name"] == "options")),
        "{doc:#}"
    );
    assert_eq!(untracked["rows"], serde_json::json!([]), "{doc:#}");
    let out = diffctx_unclocked()
        .current_dir(tmp.path())
        .args(["--symbol", "options", "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    let md = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        md.contains(&format!("callers not resolved: {sentence}")),
        "{md}"
    );
}

// ---- the folding table (Revision 12.1): what a change cannot affect is counted

fn impact_unfolded(repo: &Path) -> serde_json::Value {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "--unfold",
        ])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "impact --unfold failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn row<'a>(doc: &'a serde_json::Value, symbol: &str) -> &'a serde_json::Value {
    doc["definitions"]
        .as_array()
        .and_then(|rows| rows.iter().find(|c| c["symbol"] == symbol))
        .unwrap_or_else(|| panic!("{symbol} is not listed: {doc:#}"))
}

fn nothing_folds(doc: &serde_json::Value) {
    assert!(
        doc["definitions"]
            .as_array()
            .is_some_and(|rows| rows.iter().all(|c| c.get("folded").is_none())),
        "nothing folds here: {doc:#}"
    );
}

fn callers_of<'a>(doc: &'a serde_json::Value, symbol: &str) -> Vec<&'a str> {
    row(doc, symbol)["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["path"].as_str())
        .collect()
}

/// Four files call `price`; the change adds `tax=0`: every call still works, so
/// they are one line, counted, with three examples by path, and the line says to
/// check the default. `--unfold` lists the four, each marked with the rule.
#[test]
fn a_new_parameter_with_a_default_folds_the_calls_that_still_work() {
    let user = |name: &str, f: &str| {
        (
            format!("{name}.py"),
            format!("from shop import price\n\n\ndef {f}(i):\n    return price(i)\n"),
        )
    };
    let files: Vec<(String, String)> = [("a", "one"), ("b", "two"), ("c", "three"), ("d", "four")]
        .iter()
        .map(|(n, f)| user(n, f))
        .collect();
    let mut base: Vec<(&str, &str)> = vec![("shop.py", "def price(item):\n    return item.cost\n")];
    base.extend(files.iter().map(|(p, c)| (p.as_str(), c.as_str())));
    let tmp = repo(
        &base,
        &[(
            "shop.py",
            "def price(item, tax=0):\n    return item.cost + tax\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let price = row(&doc, "price");
    assert!(callers_of(&doc, "price").is_empty(), "{doc:#}");
    let folded = &price["folded"][0];
    assert_eq!(folded["rule"], "check-the-default", "{doc:#}");
    assert_eq!(folded["kind"], "calls", "{doc:#}");
    assert_eq!(folded["count"]["n"], 4, "{doc:#}");
    assert_eq!(folded["count"]["exact"], true, "{doc:#}");
    assert_eq!(folded["parameters"], serde_json::json!(["tax"]), "{doc:#}");
    let examples: Vec<&str> = folded["examples"]
        .as_array()
        .expect("examples")
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert_eq!(examples.len(), 3, "{doc:#}");
    for (example, file) in examples.iter().zip(["a.py", "b.py", "c.py"]) {
        assert!(example.starts_with(file), "{examples:?}");
    }
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains(
            "4 call(s) compatible with the new optional parameter(s) tax: check that the default keeps the old behaviour"
        ),
        "{md}"
    );
    assert!(md.contains("--unfold"), "{md}");
    // A folded call is still a caller: the header counts it and names the share.
    assert!(
        header(&md).contains("4 caller(s) outside the diff (4 folded as not affected)"),
        "{md}"
    );

    let unfolded = impact_unfolded(tmp.path());
    assert_eq!(callers_of(&unfolded, "price").len(), 4, "{unfolded:#}");
    assert!(
        row(&unfolded, "price")["rows"]
            .as_array()
            .expect("callers")
            .iter()
            .all(|c| c["folded_by"] == "check-the-default"),
        "{unfolded:#}"
    );
    nothing_folds(&unfolded);
}

/// A test is what the reviewer runs: among folded calls it is named first, whatever
/// its path sorts to, and the others follow by path and line.
#[test]
fn a_test_is_the_first_example_of_a_folded_line() {
    let tmp = repo(
        &[
            ("shop.py", "def price(item):\n    return item.cost\n"),
            (
                "a.py",
                "from shop import price\n\n\ndef one(i):\n    return price(i)\n",
            ),
            (
                "b.py",
                "from shop import price\n\n\ndef two(i):\n    return price(i)\n",
            ),
            (
                "tests/test_price.py",
                "from shop import price\n\n\ndef test_price():\n    assert price(1)\n",
            ),
        ],
        &[(
            "shop.py",
            "def price(item, tax=0):\n    return item.cost + tax\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let examples: Vec<&str> = row(&doc, "price")["folded"][0]["examples"]
        .as_array()
        .expect("examples")
        .iter()
        .filter_map(|e| e["path"].as_str())
        .collect();
    assert_eq!(examples.len(), 3, "{doc:#}");
    assert!(
        examples[0].starts_with("tests/test_price.py"),
        "{examples:?}"
    );
    assert!(examples[1].starts_with("a.py"), "{examples:?}");
    assert!(examples[2].starts_with("b.py"), "{examples:?}");
}

/// A parameter without a default, or one inserted before the others, breaks the
/// calls: they stay listed.
#[test]
fn a_new_required_parameter_folds_nothing() {
    for after in [
        "def price(item, tax):\n    return item.cost + tax\n",
        "def price(tax, item=0):\n    return item.cost + tax\n",
    ] {
        let tmp = repo(
            &[
                ("shop.py", "def price(item):\n    return item.cost\n"),
                (
                    "use.py",
                    "from shop import price\n\n\ndef go(i):\n    return price(i)\n",
                ),
            ],
            &[("shop.py", after)],
        );
        let doc = impact(tmp.path(), "HEAD~1..HEAD");
        nothing_folds(&doc);
        assert_eq!(callers_of(&doc, "price"), ["use.py"], "{after}: {doc:#}");
    }
}

/// An override replaces a body, it does not run it: a body-only change folds the
/// override into one line.
#[test]
fn a_body_only_change_folds_the_overrides_of_the_method() {
    let tmp = repo(
        &[
            (
                "base.py",
                "class Base:\n    def run(self):\n        return 1\n",
            ),
            (
                "child.py",
                "from base import Base\n\n\nclass Child(Base):\n    def run(self):\n        return 2\n",
            ),
        ],
        &[(
            "base.py",
            "class Base:\n    def run(self):\n        return 3\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let folded = &row(&doc, "run")["folded"][0];
    assert_eq!(folded["rule"], "override-not-run", "{doc:#}");
    assert_eq!(folded["kind"], "overrides", "{doc:#}");
    assert_eq!(folded["count"]["n"], 1, "{doc:#}");
    assert!(callers_of(&doc, "run").is_empty(), "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("overrides not affected (override-not-run): 1"),
        "{md}"
    );
}

/// A body change leaves an import or a re-export of the definition as it was.
#[test]
fn a_body_only_change_folds_the_reexports_of_the_function() {
    let tmp = repo(
        &[
            (
                "src/f.ts",
                "export function f(a: number): number {\n  return a;\n}\n",
            ),
            ("src/index.ts", "export { f } from \"./f\";\n"),
        ],
        &[(
            "src/f.ts",
            "export function f(a: number): number {\n  return a + 1;\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let folded = &row(&doc, "f")["folded"][0];
    assert_eq!(folded["rule"], "imports-a-body", "{doc:#}");
    assert_eq!(folded["kind"], "reexports", "{doc:#}");
    assert_eq!(folded["count"]["n"], 1, "{doc:#}");
}

/// R19: an export statement that names a definition of its own module is the
/// module's export of it, in ESM and in CommonJS: a body-only change leaves it
/// as it was, `--unfold` lists it as a `reexports` row.
#[test]
fn a_body_only_change_folds_the_module_own_export_of_the_function() {
    let cases: [(&str, &str, &str); 4] = [
        (
            "src/f.ts",
            "function f(a: number): number {\n  return a{n};\n}\nexport { f };\n",
            "ESM list",
        ),
        (
            "src/f.ts",
            "function f(a: number): number {\n  return a{n};\n}\nexport default f;\n",
            "ESM default",
        ),
        (
            "src/f.js",
            "function f(a) {\n  return a{n};\n}\nexports.f = f;\n",
            "exports.f",
        ),
        (
            "src/f.js",
            "function f(a) {\n  return a{n};\n}\nmodule.exports = { f };\n",
            "module.exports object",
        ),
    ];
    for (path, template, form) in cases {
        let (before, after) = (
            template.replace("{n}", " + 1"),
            template.replace("{n}", " + 2"),
        );
        let tmp = repo(&[(path, before.as_str())], &[(path, after.as_str())]);
        let doc = impact(tmp.path(), "HEAD~1..HEAD");
        let folded = &row(&doc, "f")["folded"][0];
        assert_eq!(folded["rule"], "imports-a-body", "{form}: {doc:#}");
        assert_eq!(folded["kind"], "reexports", "{form}: {doc:#}");
        assert_eq!(folded["count"]["n"], 1, "{form}: {doc:#}");
        assert!(callers_of(&doc, "f").is_empty(), "{form}: {doc:#}");
        let unfolded = impact_unfolded(tmp.path());
        let listed = row(&unfolded, "f")["rows"].as_array().expect("callers");
        assert_eq!(listed.len(), 1, "{form}: {unfolded:#}");
        assert_eq!(
            listed[0]["folded_by"], "imports-a-body",
            "{form}: {unfolded:#}"
        );
    }
}

/// A constructor function the module exports with `module.exports = Layer`, whose
/// prototype the module fills, is a definition like any: a change of its body
/// leaves the export as it was, and the writes of the prototype stay the readers
/// they are.
#[test]
fn a_body_only_change_of_an_exported_constructor_function_folds_its_export() {
    let before = "var parse = require('path-to-regexp')\n\nmodule.exports = Layer\n\nfunction Layer (path, fn) {\n  if (!(this instanceof Layer)) {\n    return new Layer(path, fn)\n  }\n  this.handle = fn\n  this.name = fn.name || '<anonymous>'\n}\n\nLayer.prototype.handle_error = function handle_error (error, req) {\n  return this.handle(error, req)\n}\n";
    let after = before.replace("'<anonymous>'", "'<anon>'");
    let tmp = repo(
        &[
            ("layer.js", before),
            (
                "router.js",
                "var Layer = require('./layer')\nexports.make = function () { return new Layer('/', function () {}) }\n",
            ),
        ],
        &[("layer.js", after.as_str())],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let folded = &row(&doc, "Layer")["folded"][0];
    assert_eq!(folded["rule"], "imports-a-body", "{doc:#}");
    assert_eq!(folded["kind"], "reexports", "{doc:#}");
    assert_eq!(folded["count"]["n"], 1, "{doc:#}");
}

/// A module that exports `Route` and also reads it: a body-only change folds the
/// export row and lists the read. Rows of different relation kinds from one
/// caller (`<module>`) are never one row, so the fold of the one is never the
/// fold of the other.
#[test]
fn a_body_only_change_folds_the_export_and_lists_the_modules_own_read_of_it() {
    let before = "module.exports = Route\n\nfunction Route (path) {\n  this.path = path\n  this.stack = []\n}\n\nvar ctor = Route\n\nRoute.prototype.dispatch = function dispatch () {\n  return new ctor(this.path)\n}\n";
    let after = before.replace("this.stack = []", "this.stack = [path]");
    let tmp = repo(&[("route.js", before)], &[("route.js", after.as_str())]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let folded = &row(&doc, "Route")["folded"];
    assert_eq!(folded.as_array().map(Vec::len), Some(1), "{doc:#}");
    assert_eq!(folded[0]["rule"], "imports-a-body", "{doc:#}");
    assert_eq!(folded[0]["kind"], "reexports", "{doc:#}");
    assert_eq!(folded[0]["count"]["n"], 1, "{doc:#}");
    assert!(
        callers_of(&doc, "Route").contains(&"route.js"),
        "the read stays listed: {doc:#}"
    );
    let unfolded = impact_unfolded(tmp.path());
    let listed = row(&unfolded, "Route")["rows"].as_array().expect("rows");
    let marked: Vec<&serde_json::Value> = listed
        .iter()
        .filter(|c| c.get("folded_by").is_some_and(|f| !f.is_null()))
        .collect();
    assert_eq!(marked.len(), 1, "{unfolded:#}");
    assert_eq!(marked[0]["class"], "reexports", "{unfolded:#}");
    assert!(
        listed.iter().any(|c| {
            c["class"] != "reexports" && c.get("folded_by").is_none_or(|f| f.is_null())
        }),
        "a row that is not the export is listed unfolded: {unfolded:#}"
    );
}

/// An added helper that a changed, tested function calls from a local variable's
/// initialiser (`const ok = a && this.passes()`) is reached by the test through
/// that function: the use belongs to the function, not to the variable, so the
/// helper is named like any changed definition but never as an untested one.
#[test]
fn an_added_helper_used_in_a_local_of_a_tested_changed_function_is_tested_through_it() {
    let tmp = repo(
        &[
            (
                "src/manager.ts",
                "export class Manager {\n  private level = 0;\n  submit(correct: boolean, streak: number): number {\n    const promote = correct && streak >= 3;\n    if (promote) {\n      this.level = this.level + 1;\n    }\n    return this.level;\n  }\n}\n",
            ),
            (
                "tests/manager.test.ts",
                "import { Manager } from '../src/manager';\n\nit('promotes after three', () => {\n  const m = new Manager();\n  expect(m.submit(true, 3)).toBe(1);\n});\n",
            ),
        ],
        &[(
            "src/manager.ts",
            "export class Manager {\n  private level = 0;\n  private passes(): boolean { return this.level < 4; }\n  submit(correct: boolean, streak: number): number {\n    const promote = correct && streak >= 3 && this.passes();\n    if (promote) {\n      this.level = this.level + 1;\n    }\n    return this.level;\n  }\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(
        doc["untested_definitions"].as_u64().unwrap_or(0),
        0,
        "{doc:#}"
    );
    let unlisted = doc["unlisted"]["no_caller"].to_string();
    assert!(
        !unlisted.contains("manager.ts::passes"),
        "the helper is not claimed untested: {doc:#}"
    );
}

/// A member an interface inherits, written through a type assertion on the
/// receiver (`(options as Opts).pagination = …`, `Opts extends Base`): the
/// write is a use of `Base.pagination` by the function, which a test calls. The
/// receiver is `Opts`, not what `options` was declared as, and the member is
/// found on the `extends` chain.
#[test]
fn a_member_written_through_an_assertion_on_an_inherited_interface_has_its_user() {
    let tmp = repo(
        &[
            (
                "src/types.ts",
                "export interface Base {\n  pagination?: { limit: number };\n}\nexport interface Opts extends Base {\n  other?: string;\n}\nexport interface Norm {\n  pagination?: { limit: number };\n  other?: string;\n}\n",
            ),
            (
                "src/use.ts",
                "import {Opts, Norm} from './types';\nexport function normalize(options: Norm, defaults?: Norm) {\n  if (defaults) {\n    (options as Opts).pagination = {...defaults.pagination};\n  }\n  return options;\n}\n",
            ),
            (
                "test/use.test.ts",
                "import {normalize} from '../src/use';\nexport function t() {\n  return normalize({});\n}\n",
            ),
        ],
        &[(
            "src/types.ts",
            "export interface Base {\n  pagination?: { limit: number; stack?: boolean };\n}\nexport interface Opts extends Base {\n  other?: string;\n}\nexport interface Norm {\n  pagination?: { limit: number };\n  other?: string;\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let users: Vec<&str> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| {
            c["symbol"]
                .as_str()
                .is_some_and(|s| s == "pagination" || s.ends_with(".pagination"))
                && c["path"] == "src/types.ts"
        })
        .flat_map(|c| c["rows"].as_array().into_iter().flatten())
        .filter_map(|c| c["path"].as_str())
        .collect();
    assert!(users.contains(&"src/use.ts"), "{users:?}: {doc:#}");
}

/// R19: a removal leaves the export dangling, which breaks the module at load,
/// so the export is listed, never folded.
#[test]
fn a_removed_definition_lists_the_export_that_names_it() {
    for (path, base, head, form) in [
        (
            "src/f.ts",
            "function f(a: number): number {\n  return a;\n}\nexport { f };\n",
            "export { f };\n",
            "ESM list",
        ),
        (
            "src/f.js",
            "function f(a) {\n  return a;\n}\nexports.f = f;\n",
            "exports.f = f;\n",
            "exports.f",
        ),
    ] {
        let tmp = repo(&[(path, base)], &[(path, head)]);
        let doc = impact(tmp.path(), "HEAD~1..HEAD");
        nothing_folds(&doc);
        assert!(callers_of(&doc, "f").contains(&path), "{form}: {doc:#}");
    }
}

/// A candidate row — a call through a receiver nothing types — folds under the
/// receiver, and the header still counts it as possible.
#[test]
fn a_candidate_folds_under_the_untyped_receiver_and_stays_counted() {
    let tmp = repo(
        &[
            (
                "engine.py",
                "class Engine:\n    def start(self):\n        return 1\n",
            ),
            (
                "use.py",
                "import engine\n\n\ndef go(thing):\n    return thing.start()\n",
            ),
        ],
        &[(
            "engine.py",
            "class Engine:\n    def start(self):\n        return 2\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let folded = &row(&doc, "start")["folded"][0];
    assert_eq!(folded["rule"], "untyped-receiver", "{doc:#}");
    assert_eq!(folded["count"]["n"], 1, "{doc:#}");
    assert!(callers_of(&doc, "start").is_empty(), "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    // A candidate may be affected: the line says what it is, never "not affected".
    assert!(
        md.contains("possible calls through an untyped receiver (not resolved): at least 1"),
        "{md}"
    );
    assert!(!md.contains("not affected (untyped-receiver)"), "{md}");
    assert!(header(&md).contains("1 possible"), "{md}");
}

/// A type use of a function folds only where its types are written down.
#[test]
fn a_type_use_folds_only_for_a_declared_type() {
    let typed = repo(
        &[
            (
                "src/f.ts",
                "export function f(a: number): number {\n  return a;\n}\n",
            ),
            (
                "src/r.ts",
                "import { f } from \"./f\";\n\nexport type R = ReturnType<typeof f>;\n",
            ),
        ],
        &[(
            "src/f.ts",
            "export function f(a: number): number {\n  return a + 1;\n}\n",
        )],
    );
    let doc = impact(typed.path(), "HEAD~1..HEAD");
    let kinds: Vec<&str> = row(&doc, "f")["folded"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["rule"].as_str())
        .collect();
    assert!(
        !kinds.contains(&"declared-type") || callers_of(&doc, "f").is_empty(),
        "{doc:#}"
    );
    // An inferred return type: the body decides it, so the use stays listed.
    let inferred = repo(
        &[
            (
                "src/f.ts",
                "export function f(a: number) {\n  return a;\n}\n",
            ),
            (
                "src/r.ts",
                "import { f } from \"./f\";\n\nexport type R = ReturnType<typeof f>;\n",
            ),
        ],
        &[(
            "src/f.ts",
            "export function f(a: number) {\n  return String(a);\n}\n",
        )],
    );
    let doc = impact(inferred.path(), "HEAD~1..HEAD");
    nothing_folds(&doc);
}

/// The cases the table never folds: a removal, a rename, a narrowed visibility, a
/// type, a field annotation a framework executes, a decorated docstring.
#[test]
fn a_removed_definition_folds_nothing() {
    let tmp = repo(
        &[
            ("shop.py", "def legacy(i):\n    return i\n"),
            (
                "use.py",
                "from shop import legacy\n\n\ndef go(i):\n    return legacy(i)\n",
            ),
        ],
        // Another body: an identical one would pair as a rename.
        &[("shop.py", "def other(i):\n    return i + 1\n")],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    nothing_folds(&doc);
    assert!(callers_of(&doc, "legacy").contains(&"use.py"), "{doc:#}");
}

#[test]
fn a_renamed_definition_folds_nothing() {
    let tmp = repo(
        &[
            ("shop.py", "def total(items):\n    return sum(items)\n"),
            (
                "use.py",
                "from shop import total\n\n\ndef go(i):\n    return total(i)\n",
            ),
        ],
        &[(
            "shop.py",
            "def grand_total(items):\n    return sum(items)\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    nothing_folds(&doc);
}

#[test]
fn a_narrowed_visibility_folds_nothing() {
    let tmp = repo(
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            ),
            ("src/lib.rs", "pub mod pricing;\npub mod user;\n"),
            (
                "src/pricing.rs",
                "pub fn total(x: u32) -> u32 {\n    x\n}\n",
            ),
            (
                "src/user.rs",
                "pub fn go() -> u32 {\n    crate::pricing::total(1)\n}\n",
            ),
        ],
        &[(
            "src/pricing.rs",
            "pub(crate) fn total(x: u32) -> u32 {\n    x\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    nothing_folds(&doc);
}

#[test]
fn an_interface_change_folds_nothing() {
    let tmp = repo(
        &[
            ("src/opts.ts", "export interface Opts {\n  a: number;\n}\n"),
            (
                "src/use.ts",
                "import { Opts } from \"./opts\";\n\nexport const o: Opts = { a: 1 };\n",
            ),
        ],
        &[(
            "src/opts.ts",
            "export interface Opts {\n  a: number;\n  b?: string;\n}\n",
        )],
    );
    // The one count an interface gaining an optional member earns is the
    // signature-compatible one (#388): it names no caller, so it folds none.
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    for fold in row(&doc, "Opts")["folded"].as_array().into_iter().flatten() {
        assert_eq!(fold["rule"], "check-the-default", "{doc:#}");
        assert_eq!(fold["examples"], serde_json::json!([]), "{doc:#}");
    }
    assert!(callers_of(&doc, "Opts").is_empty(), "{doc:#}");
}

/// A removed interface that an unchanged file still imports and annotates with is
/// a break: the file is listed with it, and "nothing references it" never stands
/// over a search that saw no type use (a removed type has only those).
#[test]
fn a_removed_interface_an_unchanged_file_still_imports_is_not_unreferenced() {
    let tmp = repo(
        &[
            (
                "src/types.ts",
                "export interface Response {\n  body: string;\n}\nexport type Kind = 'a' | 'b';\n",
            ),
            (
                "src/use.ts",
                "import {Response} from './types';\nexport function parse(r: Response): string {\n  return r.body;\n}\n",
            ),
        ],
        &[("src/types.ts", "export type Kind = 'a' | 'b';\n")],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("nothing references it"), "{md}");
    assert!(md.contains("referenced from src/use.ts"), "{md}");
}

/// The same break in every family with type uses or imports: a type removed from
/// the file that defined it, still named by a file nobody changed, is never
/// "nothing references it". One repository per family, one run, every family that
/// slipped named in the failure.
#[test]
fn a_removed_type_still_named_at_the_head_is_not_unreferenced_in_any_family() {
    struct Case {
        family: &'static str,
        base: &'static [(&'static str, &'static str)],
        changed: &'static [(&'static str, &'static str)],
        user: &'static str,
    }
    let cases = [
        Case {
            family: "python",
            base: &[
                (
                    "models.py",
                    "class Response:\n    body = \"\"\n\n\nKind = \"a\"\n",
                ),
                (
                    "use.py",
                    "from models import Response\n\n\ndef parse(r: Response) -> str:\n    return r.body\n",
                ),
            ],
            changed: &[("models.py", "Kind = \"a\"\n")],
            user: "use.py",
        },
        Case {
            family: "rust",
            base: &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("src/lib.rs", "pub mod types;\npub mod user;\n"),
                (
                    "src/types.rs",
                    "pub struct Response {\n    pub body: String,\n}\n\npub struct Kind;\n",
                ),
                (
                    "src/user.rs",
                    "use crate::types::Response;\n\npub fn parse(r: &Response) -> usize {\n    r.body.len()\n}\n",
                ),
            ],
            changed: &[("src/types.rs", "pub struct Kind;\n")],
            user: "src/user.rs",
        },
        Case {
            family: "java",
            base: &[
                (
                    "src/main/java/app/Response.java",
                    "package app;\n\nclass Response {\n    String body;\n}\n\nclass Kind {\n}\n",
                ),
                (
                    "src/main/java/app/User.java",
                    "package app;\n\npublic class User {\n    String parse(Response r) {\n        return r.body;\n    }\n}\n",
                ),
            ],
            changed: &[(
                "src/main/java/app/Response.java",
                "package app;\n\nclass Kind {\n}\n",
            )],
            user: "src/main/java/app/User.java",
        },
        Case {
            family: "kotlin",
            base: &[
                (
                    "src/Models.kt",
                    "package app\n\nclass Response(val body: String)\n\nclass Kind\n",
                ),
                (
                    "src/User.kt",
                    "package app\n\nfun parse(r: Response): String = r.body\n",
                ),
            ],
            changed: &[("src/Models.kt", "package app\n\nclass Kind\n")],
            user: "src/User.kt",
        },
        Case {
            family: "go",
            base: &[
                ("go.mod", "module example.com/app\n\ngo 1.21\n"),
                (
                    "models.go",
                    "package app\n\ntype Response struct {\n\tBody string\n}\n\ntype Kind int\n",
                ),
                (
                    "user.go",
                    "package app\n\nfunc Parse(r Response) string {\n\treturn r.Body\n}\n",
                ),
            ],
            changed: &[("models.go", "package app\n\ntype Kind int\n")],
            user: "user.go",
        },
        Case {
            family: "php",
            base: &[
                (
                    "src/Models.php",
                    "<?php\nnamespace App;\n\nclass Response\n{\n    public string $body = '';\n}\n\nclass Kind\n{\n}\n",
                ),
                (
                    "src/User.php",
                    "<?php\nnamespace App;\n\nfunction parse(Response $r): string\n{\n    return $r->body;\n}\n",
                ),
            ],
            changed: &[(
                "src/Models.php",
                "<?php\nnamespace App;\n\nclass Kind\n{\n}\n",
            )],
            user: "src/User.php",
        },
        Case {
            family: "c",
            base: &[
                (
                    "models.h",
                    "struct Response {\n  const char *body;\n};\n\nstruct Kind {\n  int a;\n};\n",
                ),
                (
                    "user.c",
                    "#include \"models.h\"\n\nconst char *parse(struct Response *r) {\n  return r->body;\n}\n",
                ),
            ],
            changed: &[("models.h", "struct Kind {\n  int a;\n};\n")],
            user: "user.c",
        },
    ];
    let mut slipped = Vec::new();
    for case in cases {
        let tmp = repo(case.base, case.changed);
        let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
        if md.contains("nothing references it")
            || !md.contains(&format!("referenced from {}", case.user))
        {
            slipped.push(format!("{}:\n{md}", case.family));
        }
    }
    assert!(slipped.is_empty(), "{}", slipped.join("\n"));
}

/// A use inside a nested function that leaves its factory (`got.extend = …` in a
/// factory returning `got`) belongs to that definition, not to the factory around
/// it: the factory is changed here, and a changed definition is no caller outside
/// the diff.
#[test]
fn a_use_in_a_nested_definition_is_not_named_by_the_changed_factory_around_it() {
    let tmp = repo(
        &[
            (
                "src/create.ts",
                "export const isOk = (v: number): boolean => v > 0;\n\nexport const create = (defaults: {a: number}) => {\n  const got = (url: string) => url + defaults.a;\n  got.extend = (n: number) => {\n    if (isOk(n)) {\n      return create({a: defaults.a + n});\n    }\n    return got;\n  };\n  return got;\n};\n",
            ),
            (
                "test/c.test.ts",
                "import {create} from '../src/create';\nexport function t() {\n  return create({a: 1});\n}\n",
            ),
        ],
        &[(
            "src/create.ts",
            "export const isOk = (v: number): boolean => v >= 0;\n\nexport const create = (defaults: {a: number}) => {\n  const got = (url: string) => url + defaults.a;\n  got.extend = (n: number) => {\n    if (isOk(n)) {\n      return create({a: defaults.a + n});\n    }\n    return got;\n  };\n  Object.freeze(defaults);\n  return got;\n};\n",
        )],
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(!md.contains("called from src/create.ts::create"), "{md}");
    assert!(md.contains("src/create.ts::extend"), "{md}");
}

/// A pydantic model's field annotation is run by the framework: a type use there
/// is never folded, though a plain function's annotation of the same name is.
#[test]
fn an_annotation_a_framework_executes_is_never_folded() {
    let tmp = repo(
        &[
            ("money.py", "Price: type = float\n"),
            (
                "model.py",
                "from pydantic import BaseModel\n\nfrom money import Price\n\n\nclass Item(BaseModel):\n    price: Price\n",
            ),
        ],
        &[("money.py", "Price: type = int\n")],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    nothing_folds(&doc);
}

/// A decorator may read the docstring (CLI help, routes): a decorated function's
/// doc edit is no layout edit, and its callers are listed.
#[test]
fn a_decorated_docstring_change_is_not_set_aside() {
    let tmp = repo(
        &[
            (
                "app.py",
                "import functools\n\n\n@functools.cache\ndef index():\n    \"\"\"Home page.\"\"\"\n    return 1\n",
            ),
            (
                "use.py",
                "from app import index\n\n\ndef go():\n    return index()\n",
            ),
        ],
        &[(
            "app.py",
            "import functools\n\n\n@functools.cache\ndef index():\n    \"\"\"The home page.\"\"\"\n    return 1\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    nothing_folds(&doc);
    assert_eq!(callers_of(&doc, "index"), ["use.py"], "{doc:#}");
}

fn impact_budget(repo: &Path, budget: u32) -> serde_json::Value {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "--budget",
            &budget.to_string(),
        ])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "impact --budget {budget} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

/// A changed caller whose body the fragmenter cut into chunks is named as the function
/// the line belongs to, never as `run[N]`, the chunk that holds the call: the chunk is
/// no definition, and the name misses the caller the answer means.
#[test]
fn a_changed_rust_caller_with_a_chunked_body_is_named_as_its_function() {
    let run = |tag: u32| {
        let statements: Vec<String> = (0..60)
            .map(|i| match i {
                40 => "    total += helper(total);".to_string(),
                _ => format!("    total += {i};"),
            })
            .collect();
        format!(
            "pub fn run() -> u32 {{\n    let mut total = {tag};\n{}\n    total\n}}\n",
            statements.join("\n")
        )
    };
    let lib = |k: u32, tag: u32| {
        format!(
            "pub fn helper(x: u32) -> u32 {{\n    x + {k}\n}}\n\n{}",
            run(tag)
        )
    };
    let manifest = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    let tmp = repo(
        &[("Cargo.toml", manifest), ("src/lib.rs", &lib(1, 0))],
        &[("src/lib.rs", &lib(2, 1))],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let helper = row(&doc, "helper");
    let named: Vec<String> = helper["also_changed"]
        .as_array()
        .expect("also_changed")
        .iter()
        .filter_map(|r| r["symbol"].as_str().map(str::to_string))
        .collect();
    assert_eq!(named, ["run"], "{doc:#}");
}

/// A helper in a test file is a definition like any other: the tests the range
/// changes use it, and the answer names it with them on the one line for what
/// only the range's own changes use. The test function nothing calls stays silent.
#[test]
fn a_test_helper_used_only_by_changed_tests_is_named_like_any_definition() {
    let tmp = repo(
        &[
            ("tests/__init__.py", ""),
            ("tests/helpers.py", "def make(n):\n    return n\n"),
            (
                "tests/test_make.py",
                "from tests.helpers import make\n\n\ndef test_make():\n    assert make(1) == 1\n",
            ),
        ],
        &[
            ("tests/helpers.py", "def make(n):\n    return n + 0\n"),
            (
                "tests/test_make.py",
                "from tests.helpers import make\n\n\ndef test_make():\n    assert make(2) == 2\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let make = row(&doc, "make");
    assert_eq!(make["rows"], serde_json::json!([]), "{doc:#}");
    assert!(
        make["also_changed"][0]["path"]
            .as_str()
            .zip(make["also_changed"][0]["symbol"].as_str())
            .is_some_and(|(p, s)| format!("{p}::{s}") == "tests/test_make.py::test_make"),
        "{doc:#}"
    );
    assert!(
        !changed_symbols(&doc).contains(&"test_make".to_string()),
        "a test nothing calls says nothing: {doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains(
            "- changed, used only by definitions changed in this range: tests/helpers.py::make ("
        ) && md.contains("(used by tests/test_make.py::test_make ("),
        "{md}"
    );
}

/// Definitions of test files rank after every other target: when the cap sheds
/// whole definitions, the test helper goes first, though it sorts before the
/// definition outside the tests.
#[test]
fn the_cap_sheds_the_definitions_of_test_files_before_any_other() {
    let rates = |k: u32| -> String {
        (0..4)
            .map(|i| format!("def quote{i}(x):\n    return x + {k}\n"))
            .collect::<Vec<_>>()
            .join("\n\n")
            + "\n"
    };
    let api: String = "from zrates.rates import quote0, quote1, quote2, quote3\n\n\n".to_string()
        + &(0..4)
            .map(|i| format!("def handler{i}(x):\n    return quote{i}(x)\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
    let tmp = repo(
        &[
            ("aaa/__init__.py", ""),
            ("aaa/test_helpers.py", "def make(n):\n    return n\n"),
            (
                "aaa/test_use.py",
                "from aaa.test_helpers import make\n\n\ndef test_make():\n    assert make(1) == 1\n",
            ),
            ("zrates/__init__.py", ""),
            ("zrates/rates.py", &rates(0)),
            ("zrates/api.py", &api),
        ],
        &[
            ("aaa/test_helpers.py", "def make(n):\n    return n + 0\n"),
            ("zrates/rates.py", &rates(1)),
        ],
    );
    let whole = impact_budget(tmp.path(), 2_000);
    assert!(
        callers_of(&whole, "make") == ["aaa/test_use.py"]
            && callers_of(&whole, "quote3") == ["zrates/api.py"],
        "every definition is listed with its caller: {whole:#}"
    );
    // Across the sweep: no definition outside the tests is shed while the test
    // helper, which sorts first, is still listed. (Where the first shed takes two,
    // the omitted entry's own text is why one does not fit.)
    let helper = "aaa/test_helpers.py::make";
    let top = _diffctx::tokenizer::count_tokens(&whole.to_string()) + 20;
    let mut helper_shed = false;
    for budget in (60..=top).rev().step_by(20) {
        let doc = impact_budget(tmp.path(), budget);
        let omitted: Vec<String> = doc["omitted"]["definitions"]["items"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| {
                format!(
                    "{}::{}",
                    d["def"]["path"].as_str().unwrap_or(""),
                    d["def"]["symbol"].as_str().unwrap_or("")
                )
            })
            .collect();
        helper_shed |= omitted.iter().any(|d| d == helper);
        assert!(
            omitted.iter().all(|d| d == helper) || omitted.iter().any(|d| d == helper),
            "budget {budget}: a definition outside the tests is shed while the helper is listed: {doc:#}"
        );
    }
    assert!(helper_shed, "no budget shed the test helper");
}

/// A field the range edits, read only by a function the range changes, is named
/// with its user: "nothing outside the diff" is shown, not implied.
#[test]
fn a_field_used_only_by_changed_definitions_is_named_with_its_users() {
    let tmp = repo(
        &[
            ("pkg/__init__.py", ""),
            (
                "pkg/opts.py",
                "class Options:\n    retries = 1\n    limit = 3\n",
            ),
            (
                "pkg/use.py",
                "from pkg.opts import Options\n\n\ndef attempts(options: Options):\n    return 1\n",
            ),
        ],
        &[
            (
                "pkg/opts.py",
                "class Options:\n    retries = 1\n    limit = 4\n",
            ),
            (
                "pkg/use.py",
                "from pkg.opts import Options\n\n\ndef attempts(options: Options):\n    return options.limit\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let limit = doc["definitions"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|c| c["symbol"].as_str().is_some_and(|s| s.ends_with("limit")))
        })
        .unwrap_or_else(|| panic!("the field is not listed: {doc:#}"));
    assert_eq!(limit["rows"], serde_json::json!([]), "{doc:#}");
    assert!(
        limit["also_changed"][0]["path"]
            .as_str()
            .zip(limit["also_changed"][0]["symbol"].as_str())
            .is_some_and(|(p, s)| format!("{p}::{s}") == "pkg/use.py::attempts"),
        "{doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("- changed, used only by definitions changed in this range: ")
            && md.contains("pkg/opts.py::limit (")
            && md.contains("(used by pkg/use.py::attempts ("),
        "{md}"
    );
}

/// A definition the range adds is named like an edited one: `helper` is read only
/// by `run`, which the range changes too, so it is a row and the answer says it is
/// used only by a definition changed in this range.
#[test]
fn a_definition_the_range_adds_is_named_with_the_changed_definitions_that_use_it() {
    let tmp = repo(
        &[
            ("app.py", "def run(x):\n    return x\n"),
            (
                "tests/test_app.py",
                "from app import run\n\n\ndef test_run():\n    assert run(1) == 1\n",
            ),
        ],
        &[(
            "app.py",
            "def helper(x):\n    return x + 1\n\n\ndef run(x):\n    return helper(x)\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        changed_symbols(&doc).contains(&"helper".to_string()),
        "{doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("helper"), "{md}");
}

/// A removed definition a line still names at the head is a broken reference
/// whatever else the range changed in that line's function: its user is a caller
/// of the removed name, never one of the diff's own lines.
#[test]
fn a_removed_definition_still_named_by_a_changed_function_keeps_that_user() {
    let tmp = repo(
        &[
            (
                "lib.py",
                "def old(x):\n    return x\n\n\ndef keep(x):\n    return x\n",
            ),
            (
                "app.py",
                "from lib import old\n\n\ndef run(x):\n    return old(x)\n",
            ),
        ],
        &[
            ("lib.py", "def keep(x):\n    return x\n"),
            (
                "app.py",
                "from lib import old\n\n\ndef run(x):\n    return old(x) + 1\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let old = row(&doc, "old");
    assert_eq!(old["change"]["kind"], "removed", "{doc:#}");
    assert_eq!(callers_of(&doc, "old"), ["app.py"], "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("called from app.py::run"), "{md}");
    assert!(!md.contains("nothing references it"), "{md}");
}

/// Every removed definition of a cell language is listed, a private constant of a
/// TypeScript module too: here as one nothing references.
#[test]
fn a_removed_private_typescript_constant_is_listed_alone() {
    let tmp = repo(
        &[(
            "src/util.ts",
            "const factor = 7;\n\nexport function price(a: number): number {\n  return a * factor;\n}\n",
        )],
        &[(
            "src/util.ts",
            "export function price(a: number): number {\n  return a * 7;\n}\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let factor = row(&doc, "factor");
    assert_eq!(factor["change"]["kind"], "removed", "{doc:#}");
    assert_eq!(factor["rows"], serde_json::json!([]), "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(md.contains("src/util.ts::factor (removed; was "), "{md}");
    assert!(md.contains("nothing references it"), "{md}");
}

/// A contract row is about the export's place in the surface: a declaration
/// reflowed over lines, with a new body, keeps its name, kind and signature and
/// is none; a parameter added is one.
#[test]
fn an_export_that_keeps_its_name_kind_and_signature_is_no_contract() {
    let tmp = repo_steps_for_exports();
    let reflowed = impact(tmp.path(), "HEAD~2..HEAD~1");
    assert!(
        !contract_symbols(&reflowed).contains(&"public_api mono".to_string()),
        "{reflowed:#}"
    );
    let widened = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        contract_symbols(&widened).contains(&"public_api mono".to_string()),
        "{widened:#}"
    );
}

/// A method whose only caller is a function the range changes, through a
/// receiver nothing types, has no resolved use and so no caller: silence there
/// would read as "nothing uses it". It is named with that function as a possible
/// user, never as a caller or as a resolved user.
#[test]
fn a_method_called_only_through_untyped_receivers_in_changed_functions_is_possibly_used() {
    let tmp = repo(
        &[
            (
                "src/retry.ts",
                "export class RetryObject {\n  computeDelay(): number {\n    return 1;\n  }\n}\n",
            ),
            (
                "src/delay.ts",
                "export function calculateRetryDelay(retry: any): number {\n  return 0;\n}\n",
            ),
        ],
        &[
            (
                "src/retry.ts",
                "export class RetryObject {\n  computeDelay(): number {\n    return 2;\n  }\n}\n",
            ),
            (
                "src/delay.ts",
                "export function calculateRetryDelay(retry: any): number {\n  return retry.computeDelay();\n}\n",
            ),
        ],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let method = doc["definitions"]
        .as_array()
        .and_then(|rows| {
            rows.iter().find(|c| {
                c["symbol"]
                    .as_str()
                    .is_some_and(|s| s.ends_with("computeDelay"))
            })
        })
        .unwrap_or_else(|| panic!("the method is not listed: {doc:#}"));
    assert_eq!(method["rows"], serde_json::json!([]), "{doc:#}");
    assert!(method.get("also_changed").is_none(), "{doc:#}");
    assert!(
        method["possibly_also_changed"][0]["path"]
            .as_str()
            .zip(method["possibly_also_changed"][0]["symbol"].as_str())
            .is_some_and(|(p, s)| format!("{p}::{s}") == "src/delay.ts::calculateRetryDelay"),
        "{doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("computeDelay (")
            && md.contains("(possibly used by src/delay.ts::calculateRetryDelay ("),
        "{md}"
    );
    assert!(!md.contains("(used by src/delay.ts"), "{md}");
}

fn repo_steps_for_exports() -> TempDir {
    support::repo_steps(&[
        &[(
            "src/panel.ts",
            "export function mono(x: number, y: number): number {\n  return x * y;\n}\n",
        )],
        &[(
            "src/panel.ts",
            "export function mono(\n  x: number,\n  y: number\n): number {\n  return x * y + 1;\n}\n",
        )],
        &[(
            "src/panel.ts",
            "export function mono(\n  x: number,\n  y: number,\n  z: number\n): number {\n  return x * y + z;\n}\n",
        )],
    ])
}

/// One escape rule for every function value: a constant holding an arrow that
/// the function returns through an object is a definition like a declaration is,
/// so a change of its body is its own entry and never the function's.
#[test]
fn a_changed_function_value_that_leaves_its_function_has_its_own_entry() {
    let source = |each: &str| {
        format!(
            "export function create() {{\n  const got: any = () => 1;\n  function eachDecl(n: number) {{ return n + {each}; }}\n  const eachConst = (n: number) => n + {each};\n  got.eachDecl = eachDecl;\n  got.eachConst = eachConst;\n  return got;\n}}\n"
        )
    };
    let (before, after) = (source("1"), source("2"));
    let tmp = repo(
        &[
            ("src/c.ts", before.as_str()),
            (
                "src/u.ts",
                "import { create } from \"./c\";\n\nexport function g() {\n  return create();\n}\n",
            ),
        ],
        &[("src/c.ts", after.as_str())],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let symbols: Vec<&str> = doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["symbol"].as_str())
        .collect();
    for wanted in ["eachDecl", "eachConst"] {
        assert!(
            symbols
                .iter()
                .any(|s| *s == wanted || s.ends_with(&format!(".{wanted}"))),
            "{wanted} is not an entry of {symbols:?}"
        );
    }
    assert!(
        !symbols.contains(&"create"),
        "the edit is attributed to create: {symbols:?}"
    );
}

/// A comment is stored nowhere a decorator can read: a comment-only edit of a
/// decorated function leaves every relation as it was, counted in one line,
/// where a docstring edit (above) keeps its callers listed.
#[test]
fn a_comment_only_change_of_a_decorated_function_folds_every_relation() {
    let tmp = repo(
        &[
            (
                "app.py",
                "import functools\n\n\n@functools.cache\ndef index():\n    return 1\n",
            ),
            (
                "use.py",
                "from app import index\n\n\ndef go():\n    return index()\n",
            ),
        ],
        &[(
            "app.py",
            "import functools\n\n\n@functools.cache\ndef index():\n    # one is enough\n    return 1\n",
        )],
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let folded = &row(&doc, "index")["folded"][0];
    assert_eq!(folded["rule"], "no-code-change", "{doc:#}");
    assert_eq!(folded["kind"], "calls", "{doc:#}");
    assert_eq!(folded["count"]["n"], 1, "{doc:#}");
    assert!(callers_of(&doc, "index").is_empty(), "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("calls not affected (no-code-change): 1"),
        "{md}"
    );
}

fn repo_deleting_a_schema_module(others: &[(&str, &str)]) -> TempDir {
    let models = (0..10)
        .map(|i| format!("class Model{i}:\n    name = \"\"\n    id = 0\n"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "pkg/__init__.py", "");
    write(repo, "pkg/schema.py", &models);
    write(repo, "app/other.py", "def go():\n    return 1\n");
    for (path, text) in others {
        write(repo, path, text);
    }
    commit_all(repo, "initial");
    std::fs::remove_file(repo.join("pkg/schema.py")).expect("delete the module");
    commit_all(repo, "drop the schema module");
    tmp
}

fn removed_symbols(doc: &serde_json::Value) -> Vec<&str> {
    doc["definitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["change"]["kind"] == "removed")
        .filter_map(|c| c["symbol"].as_str())
        .collect()
}

/// A module the range deletes that no file imports is one line, not one per
/// definition it held: no import reaches any of them, so no name of a removed
/// class's field is a candidate for anything. No file computes an import target
/// here, so the claim is "no importers", not "none found".
#[test]
fn a_deleted_module_nothing_imports_is_named_once() {
    let tmp = repo_deleting_a_schema_module(&[]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(
        doc["deleted_modules"],
        serde_json::json!({"total": 1, "items": [{"path": "pkg/schema.py", "definitions": 10}]}),
        "{doc:#}"
    );
    assert!(removed_symbols(&doc).is_empty(), "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains("- deleted module pkg/schema.py: 10 definition(s), no importers"),
        "{md}"
    );
    assert!(!md.contains("too common"), "{md}");
    assert_eq!(md.matches("deleted module").count(), 1, "{md}");
}

/// The same module with an importer: the module names it, the one class the
/// importer uses keeps its row and its caller, and the nine nothing uses are
/// folded into the module's count.
#[test]
fn a_deleted_module_with_an_importer_keeps_the_definitions_it_uses_and_names_it() {
    let tmp = repo_deleting_a_schema_module(&[(
        "app/use.py",
        "from pkg.schema import Model0\n\n\ndef build():\n    return Model0()\n",
    )]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(
        doc["deleted_modules"]["items"],
        serde_json::json!([{
            "path": "pkg/schema.py",
            "definitions": 10,
            "listed": 1,
            "importers": ["app/use.py"]
        }]),
        "{doc:#}"
    );
    assert_eq!(removed_symbols(&doc), ["Model0"], "{doc:#}");
    assert_eq!(callers_of(&doc, "Model0"), ["app/use.py"], "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains(
            "- deleted module pkg/schema.py: 10 definition(s), 1 listed on its own, imported by app/use.py"
        ),
        "{md}"
    );
}

/// An import whose name is computed anywhere may reach the module: "no importers"
/// becomes "none found", and the answer is no empty one.
#[test]
fn a_computed_import_leaves_a_deleted_module_unproven() {
    let tmp = repo_deleting_a_schema_module(&[(
        "app/loader.py",
        "import importlib\n\n\ndef load(name):\n    return importlib.import_module(name)\n",
    )]);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(
        doc["deleted_modules"]["items"][0]["dynamic_import"], true,
        "{doc:#}"
    );
    assert_ne!(
        doc["empty"], true,
        "a claim with a gap is no empty answer: {doc:#}"
    );
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains(
            "- deleted module pkg/schema.py: 10 definition(s), no importer found; imports computed at run time are not read"
        ),
        "{md}"
    );
}

/// A package of two modules, each a class with the members `run`, `start` and
/// `title`, deleted whole. `other/` spells those words on `lines` lines and never
/// imports the package; `importer` is a file that does.
fn repo_deleting_a_package(lines: usize, importer: Option<(&str, &str)>) -> TempDir {
    let class = |name: &str| {
        format!(
            "class {name}:\n    title = \"\"\n\n    def run(self):\n        return 1\n\n    def start(self):\n        return 2\n"
        )
    };
    let noise = (0..lines)
        .map(|_| "    obj.run()\n    obj.start()\n    obj.title\n")
        .collect::<String>();
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "pkg/__init__.py", "");
    write(repo, "pkg/first.py", &class("First"));
    write(repo, "pkg/second.py", &class("Second"));
    write(repo, "other/loop.py", &format!("def spin(obj):\n{noise}"));
    if let Some((path, text)) = importer {
        write(repo, path, text);
    }
    commit_all(repo, "initial");
    std::fs::remove_dir_all(repo.join("pkg")).expect("delete the package");
    commit_all(repo, "drop the package");
    tmp
}

/// A package nothing imports: `other/` spells the members' names, and that is no
/// way to reach a deleted class. Each module is one line, the members are no
/// candidates, and nothing says "possibly called".
#[test]
fn a_deleted_package_nothing_imports_folds_the_members_other_files_spell() {
    let tmp = repo_deleting_a_package(4, None);
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(doc["deleted_modules"]["total"], 2, "{doc:#}");
    assert!(removed_symbols(&doc).is_empty(), "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert_eq!(md.matches("no importers").count(), 2, "{md}");
    assert!(!md.contains("possibly called"), "{md}");
    assert!(!md.contains("too common"), "{md}");
}

/// One module is imported and uses only its class: the module says who imports
/// it, the class that is used stays a row with its caller, and the members a name
/// on hundreds of lines elsewhere cannot place are folded into the count instead
/// of "too common to trace". The module nobody imports is still one line.
#[test]
fn a_deleted_module_names_its_importers_and_folds_the_members_nothing_uses() {
    let tmp = repo_deleting_a_package(
        300,
        Some((
            "app/use.py",
            "from pkg.first import First\n\n\ndef build():\n    return First()\n",
        )),
    );
    let doc = impact(tmp.path(), "HEAD~1..HEAD");
    let modules = &doc["deleted_modules"];
    assert_eq!(modules["total"], 2, "{doc:#}");
    let first = modules["items"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["path"] == "pkg/first.py"))
        .expect("pkg/first.py is a deleted module");
    assert_eq!(
        first["importers"],
        serde_json::json!(["app/use.py"]),
        "{doc:#}"
    );
    assert_eq!(first["definitions"], 3, "{doc:#}");
    assert_eq!(first["listed"], 1, "{doc:#}");
    assert_eq!(removed_symbols(&doc), ["First"], "{doc:#}");
    assert_eq!(callers_of(&doc, "First"), ["app/use.py"], "{doc:#}");
    let md = impact_markdown(tmp.path(), "HEAD~1..HEAD");
    assert!(
        md.contains(
            "- deleted module pkg/first.py: 3 definition(s), 1 listed on its own, imported by app/use.py"
        ),
        "{md}"
    );
    assert!(
        md.contains("- deleted module pkg/second.py: 3 definition(s), no importers"),
        "{md}"
    );
    assert!(!md.contains("too common"), "{md}");
}

/// A package's importer reaches every file of it: a Go file that imports the
/// package and uses one name is an importer of the package's other files, whose
/// members are folded.
#[test]
fn a_deleted_go_package_is_imported_as_one_module() {
    let tmp = TempDir::new().expect("tempdir");
    let repo = tmp.path();
    init_repo(repo);
    write(repo, "go.mod", "module example.com/app\n\ngo 1.22\n");
    write(
        repo,
        "pkg/first.go",
        "package pkg\n\ntype First struct{}\n\nfunc (f *First) Run() int { return 1 }\n",
    );
    write(
        repo,
        "pkg/second.go",
        "package pkg\n\ntype Second struct{}\n\nfunc (s *Second) Run() int { return 2 }\n",
    );
    write(
        repo,
        "app/use.go",
        "package app\n\nimport \"example.com/app/pkg\"\n\nfunc Build() *pkg.First { return &pkg.First{} }\n",
    );
    commit_all(repo, "initial");
    std::fs::remove_dir_all(repo.join("pkg")).expect("delete the package");
    commit_all(repo, "drop the package");
    let md = impact_markdown(repo, "HEAD~1..HEAD");
    assert!(
        md.contains("- deleted module pkg/second.go: 2 definition(s), imported by app/use.go"),
        "{md}"
    );
    assert!(!md.contains("no importers"), "{md}");
}

/// The module's default export is one export across a change of form: `export
/// default (…) => …` made a named `export default function`, with the same
/// parameters and return type, is no export removed and no export added. A
/// parameter added to it is a contract.
#[test]
fn a_default_export_that_changes_only_its_form_is_one_export() {
    let tmp = support::repo_steps(&[
        &[(
            "src/rate.ts",
            "export default (x: number, y: number): number => x * y;\n",
        )],
        &[(
            "src/rate.ts",
            "export default function mono(x: number, y: number): number {\n  return x * y;\n}\n",
        )],
        &[(
            "src/rate.ts",
            "export default function mono(x: number, y: number, z: number): number {\n  return x * y * z;\n}\n",
        )],
    ]);
    let reshaped = impact(tmp.path(), "HEAD~2..HEAD~1");
    assert_eq!(
        contract_symbols(&reshaped),
        Vec::<String>::new(),
        "{reshaped:#}"
    );
    assert!(
        !changed_symbols(&reshaped).contains(&"default".to_string()),
        "the default export was not removed: {reshaped:#}"
    );
    let widened = impact(tmp.path(), "HEAD~1..HEAD");
    assert!(
        contract_symbols(&widened).contains(&"public_api mono".to_string()),
        "{widened:#}"
    );
}
