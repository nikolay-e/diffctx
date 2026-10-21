// Rust callers, definitions and test links (#506 B, Rust lane): each gate
// pairs the true answer with look-alikes that must not join it — a
// same-named function of a module nobody imports, a method of the same
// name on another type, the name in a string or a comment, a macro of
// that name — and a control that has to come back exactly.

use std::path::Path;

use tempfile::TempDir;

mod support;
use support::{diffctx, git};

const MANIFEST: (&str, &str) = (
    "Cargo.toml",
    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
);

/// A repository with one commit per step; a step writes its files and
/// deletes the paths it lists with no content.
fn repo(steps: &[&[(&str, Option<&str>)]]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    for (step, files) in steps.iter().enumerate() {
        for (rel, content) in *files {
            let path = root.join(rel);
            match content {
                Some(text) => {
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(path, text).unwrap();
                }
                None => std::fs::remove_file(path).unwrap(),
            }
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", &format!("step {step}")]);
    }
    tmp
}

fn run(repo: &Path, args: &[&str]) -> String {
    let out = diffctx()
        .current_dir(repo)
        .args(args)
        .args(["-q", "--timeout", "600"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

fn symbol_markdown(repo: &Path, name: &str) -> String {
    run(repo, &[".", "--symbol", name, "-f", "md"])
}

fn impact_markdown(repo: &Path) -> String {
    run(
        repo,
        &["--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", "md"],
    )
}

/// `path::symbol` of every listed caller.
fn callers(md: &str) -> Vec<String> {
    let mut found: Vec<String> = md
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("- called from "))
        .map(|l| l.split(" (").next().unwrap_or(l).to_string())
        .collect();
    found.sort();
    found
}

/// The `path::symbol` entries listed under a heading of the answer.
fn listed_under(md: &str, heading: &str) -> Vec<String> {
    let mut found: Vec<String> = md
        .lines()
        .skip_while(|l| !l.starts_with(heading))
        .skip(1)
        .take_while(|l| l.starts_with("  - "))
        .map(|l| {
            let entry = l.trim_start_matches("  - ");
            entry.split(" (").next().unwrap_or(entry).to_string()
        })
        .collect();
    found.sort();
    found
}

/// Files that each spell a name the way a call would, without calling it.
fn decoys(name: &str) -> Vec<(String, String)> {
    vec![
        (
            "src/other.rs".to_string(),
            format!(
                "pub fn {name}(x: u32) -> u32 {{\n    x\n}}\n\npub fn own() -> u32 {{\n    {name}(2)\n}}\n"
            ),
        ),
        (
            "src/meter.rs".to_string(),
            format!(
                "use crate::engine;\n\npub struct Meter;\n\nimpl Meter {{\n    pub fn {name}(&self, x: u32) -> u32 {{\n        x\n    }}\n}}\n\npub fn measure(m: &Meter) -> u32 {{\n    m.{name}(4) + engine::helper_1(0)\n}}\n"
            ),
        ),
        (
            "src/notes.rs".to_string(),
            format!(
                "use crate::engine;\n\npub fn notes() -> &'static str {{\n    // engine::{name}(5) was the old way\n    let _ = engine::helper_2(0);\n    \"engine::{name}(6)\"\n}}\n"
            ),
        ),
        (
            "src/macros.rs".to_string(),
            format!(
                "use crate::engine;\n\nmacro_rules! {name} {{\n    ($x:expr) => {{\n        $x\n    }};\n}}\n\npub fn via_macro() -> u32 {{\n    {name}!(7) + engine::helper_3(0)\n}}\n"
            ),
        ),
    ]
}

const DECOY_MODULES: &str = "pub mod macros;\npub mod meter;\npub mod notes;\npub mod other;\n";

/// #547: a long function whose fragment references more names than the
/// ranking graph keeps edges for calls the definition from inside a closure,
/// in the definition's own file; it read "0 callers".
#[test]
fn a_capped_caller_in_the_definitions_own_file_is_a_caller() {
    let helpers = (0..80)
        .map(|i| format!("pub fn helper_{i}(x: u32) -> u32 {{\n    x + {i}\n}}\n\n"))
        .collect::<Vec<_>>()
        .concat();
    let calls = (0..80)
        .map(|i| format!("        t += helper_{i}(t);\n"))
        .collect::<Vec<_>>()
        .concat();
    let engine = format!(
        "{helpers}pub fn target(x: u32) -> u32 {{\n    x * 3\n}}\n\npub fn driver(xs: Vec<u32>) -> u32 {{\n    let mut t = 0;\n    for s in &xs {{\n{calls}        let m: u32 = xs.iter().map(|v| target(*v + *s)).sum();\n        t += m;\n    }}\n    t\n}}\n"
    );
    let decoys = decoys("target");
    let mut files: Vec<(&str, Option<&str>)> = decoys
        .iter()
        .map(|(p, c)| (p.as_str(), Some(c.as_str())))
        .collect();
    let lib = format!("pub mod engine;\npub mod user;\n{DECOY_MODULES}");
    files.extend([
        (MANIFEST.0, Some(MANIFEST.1)),
        ("src/lib.rs", Some(lib.as_str())),
        ("src/engine.rs", Some(engine.as_str())),
        (
            "src/user.rs",
            Some("use crate::engine;\n\npub fn user() -> u32 {\n    engine::target(9)\n}\n"),
        ),
    ]);
    let tmp = repo(&[&files]);
    let md = symbol_markdown(tmp.path(), "src/engine.rs:target");
    let found: Vec<String> = callers(&md)
        .into_iter()
        .map(|c| c.split('[').next().unwrap_or(&c).to_string())
        .collect();
    assert_eq!(
        found,
        ["src/engine.rs::driver", "src/user.rs::user"],
        "{md}"
    );
}

/// #552: callers that sit in the diff make a definition called, not one
/// nothing calls; a name only spelled alike there leaves it uncalled.
#[test]
fn a_definition_called_only_inside_the_diff_reads_so() {
    let resource = |extra: &str| {
        format!(
            "pub struct RunContext {{\n    pub limit: usize,\n}}\n\nimpl RunContext {{\n    pub fn check(&self) -> bool {{\n        self.limit > 0\n    }}\n{extra}}}\n"
        )
    };
    let added = resource(
        "\n    pub fn bounded_par<I: IntoIterator>(&self, items: I) -> impl Iterator<Item = I::Item> + '_\n    where\n        I::IntoIter: 'static,\n    {\n        items.into_iter().take_while(move |_| self.check())\n    }\n}\n\npub fn lonely(x: u32) -> u32 {\n    x + 1\n",
    );
    let base_resource = resource("");
    let scan = |body: &str| {
        format!(
            "use crate::resource::RunContext;\n\npub fn scan(ctx: &RunContext, paths: Vec<String>) -> Vec<String> {{\n    {body}.filter(|p| p.ends_with(\".rs\")).collect()\n}}\n"
        )
    };
    let walk = |head: &str, extra: &str| {
        format!(
            "use crate::other;\nuse crate::resource::RunContext;\nuse tally::lonely;\n\npub fn walk(ctx: &RunContext, files: Vec<String>) -> usize {{\n    let mut n = 0;\n    for f in {head} {{\n        if f.len() > 2 {{\n            n += 1;\n        }}\n    }}\n{extra}    n\n}}\n"
        )
    };
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some("pub mod other;\npub mod resource;\npub mod scan;\npub mod walk;\n"),
            ),
            ("src/resource.rs", Some(&base_resource)),
            (
                "src/other.rs",
                Some("pub fn lonely(x: u32) -> u32 {\n    x\n}\n"),
            ),
            ("src/scan.rs", Some(&scan("paths.into_iter()"))),
            ("src/walk.rs", Some(&walk("files", ""))),
            (
                "tests/smoke.rs",
                Some(
                    "use fixture::resource::RunContext;\n\n#[test]\nfn checks_its_limit() {\n    assert!(RunContext { limit: 1 }.check());\n}\n",
                ),
            ),
        ],
        &[
            ("src/resource.rs", Some(&added)),
            ("src/scan.rs", Some(&scan("ctx.bounded_par(paths)"))),
            (
                "src/walk.rs",
                Some(&walk(
                    "ctx.bounded_par(files)",
                    "    // lonely(n) is resource's\n    let _ = \"lonely(n)\";\n    n += other::lonely(1) as usize + lonely!(2);\n",
                )),
            ),
        ],
    ]);
    let md = impact_markdown(tmp.path());
    assert_eq!(
        listed_under(&md, "Changed, called only from inside the diff"),
        ["src/resource.rs::bounded_par"],
        "{md}"
    );
    assert_eq!(
        listed_under(&md, "Changed, nothing calls them"),
        [
            "src/resource.rs::lonely",
            "src/scan.rs::scan",
            "src/walk.rs::walk"
        ],
        "{md}"
    );
}

/// #528: a `#[test]` in an inline `mod tests` is a test; a helper beside it
/// that no test calls, and a function only commented like a test, are not.
#[test]
fn an_inline_test_is_a_test_link_not_an_untested_caller() {
    let tmp = repo(&[&[
        (MANIFEST.0, Some(MANIFEST.1)),
        ("src/lib.rs", Some("pub mod resource;\n")),
        (
            "src/resource.rs",
            Some(
                "pub fn poll_current() -> u32 {\n    1\n}\n\npub fn poll_every() -> u32 {\n    poll_current() + 1\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    fn setup() -> u32 {\n        poll_current()\n    }\n\n    // #[test]\n    fn test_shaped() -> u32 {\n        poll_current() * 2\n    }\n\n    #[test]\n    fn polls_now() {\n        assert_eq!(poll_current(), 1);\n    }\n\n    #[tokio::test]\n    async fn polls_later() {\n        assert_eq!(poll_current() + setup(), 2);\n    }\n}\n",
            ),
        ),
        (
            "tests/cli.rs",
            Some(
                "use fixture::resource::poll_every;\n\n#[test]\nfn every_polls() {\n    assert_eq!(poll_every(), 2);\n}\n",
            ),
        ),
    ]]);
    let md = symbol_markdown(tmp.path(), "poll_current");
    assert!(
        md.lines()
            .next()
            .unwrap_or("")
            .ends_with("5 caller(s), 1 caller(s) without a static test link"),
        "{md}"
    );
    let out = run(tmp.path(), &[".", "--symbol", "poll_current", "-f", "json"]);
    let doc: serde_json::Value = serde_json::from_str(&out).expect("json");
    let mut guards: Vec<String> = doc["changed"][0]["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{} {} {}",
                c["symbol"].as_str().unwrap_or(""),
                c["tested_by"].as_str().unwrap_or("-"),
                c["file_tested"].as_bool().unwrap_or(false)
            )
        })
        .collect();
    guards.sort();
    assert_eq!(
        guards,
        [
            "poll_every tests/cli.rs false",
            "polls_later src/resource.rs false",
            "polls_now src/resource.rs false",
            "setup src/resource.rs false",
            "test_shaped - true"
        ],
        "{out}"
    );
}

/// #545: the changed methods of one trait, each reached only through `dyn`,
/// fold into one line; a lone method of another trait and a resolved caller
/// keep their rows.
#[test]
fn methods_of_one_trait_fold_into_one_line() {
    let a = |n: u32| {
        format!(
            "use crate::facts::{{Model, Other}};\n\npub struct A;\n\nimpl Model for A {{\n    fn covers(&self, p: &str) -> bool {{\n        p.len() > {n}\n    }}\n\n    fn name(&self) -> String {{\n        format!(\"a{n}\")\n    }}\n}}\n\nimpl Other for A {{\n    fn other(&self) -> u32 {{\n        {n}\n    }}\n}}\n\npub fn helper(x: u32) -> u32 {{\n    x + {n}\n}}\n"
        )
    };
    let b = |n: u32| {
        format!(
            "use crate::facts::Model;\n\npub struct B;\n\nimpl Model for B {{\n    fn covers(&self, p: &str) -> bool {{\n        p.is_empty() || p.len() == {n}\n    }}\n\n    fn name(&self) -> String {{\n        format!(\"b{n}\")\n    }}\n}}\n"
        )
    };
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some("pub mod a;\npub mod b;\npub mod facts;\npub mod user;\n"),
            ),
            (
                "src/facts.rs",
                Some(
                    "pub trait Model {\n    fn covers(&self, p: &str) -> bool;\n    fn name(&self) -> String;\n}\n\npub trait Other {\n    fn other(&self) -> u32;\n}\n",
                ),
            ),
            ("src/a.rs", Some(&a(1))),
            ("src/b.rs", Some(&b(1))),
            (
                "src/user.rs",
                Some("use crate::a;\n\npub fn run() -> u32 {\n    a::helper(3)\n}\n"),
            ),
        ],
        &[("src/a.rs", Some(&a(2))), ("src/b.rs", Some(&b(2)))],
    ]);
    let md = impact_markdown(tmp.path());
    let folded: Vec<&str> = md.lines().filter(|l| l.contains("`Model`")).collect();
    let line = "- 4 changed method(s) implementing `Model` (calls through `dyn Model` or a generic bound are not resolved): ";
    assert!(folded.len() == 1 && folded[0].starts_with(line), "{md}");
    let mut members: Vec<&str> = folded[0][line.len()..].split(", ").collect();
    members.sort_unstable();
    assert_eq!(
        members,
        [
            "src/a.rs::covers",
            "src/a.rs::name",
            "src/b.rs::covers",
            "src/b.rs::name"
        ],
        "{md}"
    );
    assert_eq!(
        md.matches("implements `Other`; calls through `dyn Other`")
            .count(),
        1,
        "{md}"
    );
    assert!(md.contains("called from src/user.rs::run"), "{md}");
}

/// #540: a module split moves definitions into new files and re-exports
/// them; they are not removed. A definition truly removed, and one whose name
/// only recurs in a new file it is not re-exported from, still are, under
/// the path they had at the base.
#[test]
fn a_definition_moved_in_a_split_is_not_removed() {
    let shared = (0..30)
        .map(|i| format!("    let v{i} = x + {i};\n"))
        .collect::<Vec<_>>()
        .concat();
    let shared = format!("pub fn shared(x: u32) -> u32 {{\n{shared}    x\n}}\n");
    let read = "pub fn read(p: &str) -> usize {\n    p.len() * 2 + 1\n}\n";
    let python = "pub fn resolve_python(m: &str) -> String {\n    format!(\"{m}.py\")\n}\n";
    let js = "pub fn resolve_js(m: &str) -> String {\n    format!(\"{m}.js\")\n}\n";
    let helper_old = "pub fn helper(x: u32) -> u32 {\n    x * 7\n}\n";
    let helper_new = "pub fn helper(x: u32) -> u32 {\n    x + 100\n}\n";
    let obsolete = "pub fn obsolete() -> u32 {\n    41\n}\n";
    let before = format!("{shared}\n{read}\n{python}\n{js}\n{helper_old}\n{obsolete}");
    let after = format!(
        "pub mod js;\npub mod python;\n\npub use js::{{read, resolve_js}};\npub use python::resolve_python;\n\n{shared}"
    );
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            ("src/lib.rs", Some("pub mod bindings;\npub mod user;\n")),
            ("src/bindings.rs", Some(&before)),
            (
                "src/user.rs",
                Some(
                    "use crate::bindings;\n\npub fn user(p: &str) -> usize {\n    bindings::read(p)\n        + bindings::resolve_js(p).len()\n        + bindings::resolve_python(p).len()\n        + bindings::helper(1) as usize\n        + bindings::obsolete() as usize\n}\n",
                ),
            ),
        ],
        &[
            ("src/bindings.rs", None),
            ("src/bindings/mod.rs", Some(&after)),
            ("src/bindings/python.rs", Some(python)),
            (
                "src/bindings/js.rs",
                Some(&format!("{read}\n{js}\n{helper_new}")),
            ),
        ],
    ]);
    let out = run(
        tmp.path(),
        &["--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", "json"],
    );
    let doc: serde_json::Value = serde_json::from_str(&out).expect("json");
    let named = |items: &serde_json::Value, keep: &dyn Fn(&serde_json::Value) -> bool| {
        let mut found: Vec<String> = items
            .as_array()
            .into_iter()
            .flatten()
            .filter(|s| keep(s))
            .map(|s| {
                format!(
                    "{}::{}",
                    s["path"].as_str().unwrap_or(""),
                    s["symbol"].as_str().unwrap_or("")
                )
            })
            .collect();
        found.sort();
        found
    };
    let expected = ["src/bindings.rs::helper", "src/bindings.rs::obsolete"];
    assert_eq!(
        named(&doc["changed"], &|s| s["kind"] == "removed"),
        expected,
        "{out}"
    );
    assert_eq!(
        named(&doc["contracts"], &|s| s["kind"] == "removed_api"),
        expected,
        "{out}"
    );
}

/// A removed `pub const` breaks the code still reading it; the removal read
/// as a clean zero because only items with bodies counted as declarations
/// (found with #491). A module with its own constant of that name and the
/// name in a string are no readers.
#[test]
fn a_removed_const_names_the_readers_it_breaks() {
    let manifest = "[package]\nname = \"x\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    let tmp = repo(&[
        &[
            ("Cargo.toml", Some(manifest)),
            (
                "src/lib.rs",
                Some(
                    "pub mod limits;\npub mod other;\n\npub fn run() -> u32 {\n    limits::MAX_RETRIES\n}\n",
                ),
            ),
            (
                "src/limits.rs",
                Some("pub const MAX_RETRIES: u32 = 3;\n\npub fn keep() -> u32 {\n    1\n}\n"),
            ),
            (
                "src/other.rs",
                Some(
                    "pub const MAX_RETRIES: u32 = 9;\n\npub fn own() -> (u32, &'static str) {\n    (MAX_RETRIES, \"limits::MAX_RETRIES\")\n}\n",
                ),
            ),
        ],
        &[("src/limits.rs", Some("pub fn keep() -> u32 {\n    1\n}\n"))],
    ]);
    let md = impact_markdown(tmp.path());
    let readers: Vec<&str> = md
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("- referenced from "))
        .map(|l| l.split(" (").next().unwrap_or(l))
        .collect();
    assert_eq!(readers, vec!["src/lib.rs::run"], "{md}");
    assert!(
        md.contains("removed_api src/limits.rs::MAX_RETRIES"),
        "{md}"
    );
}

fn impact_json(repo: &Path) -> serde_json::Value {
    let out = run(
        repo,
        &["--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", "json"],
    );
    serde_json::from_str(&out).expect("json")
}

/// `path::symbol` of each caller of the changed (or removed) `symbol`.
fn callers_of(doc: &serde_json::Value, symbol: &str, removed: bool) -> Vec<String> {
    let mut found: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["symbol"] == symbol && (c["kind"] == "removed") == removed)
        .flat_map(|c| c["callers"].as_array().cloned().unwrap_or_default())
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    found.sort();
    found
}

/// `path::symbol` of every contract of `kind`.
fn contracts(doc: &serde_json::Value, kind: &str) -> Vec<String> {
    let mut found: Vec<String> = doc["contracts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["kind"] == kind)
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    found.sort();
    found
}

/// `path::symbol` of every listed caller, called or referenced.
fn referrers(md: &str) -> Vec<String> {
    let mut found: Vec<String> = md
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            l.strip_prefix("- called from ")
                .or_else(|| l.strip_prefix("- referenced from "))
        })
        .map(|l| l.split(" (").next().unwrap_or(l).to_string())
        .collect();
    found.sort();
    found
}

/// A module over the size cap: what it says is still read by the model.
fn over_the_cap(tail: &str) -> String {
    let body: String = (0..25)
        .map(|j| format!("    let v{j} = x + {j};\n"))
        .collect();
    let filler: String = (0..300)
        .map(|i| format!("pub fn filler_{i}(x: u32) -> u32 {{\n{body}    x\n}}\n\n"))
        .collect();
    format!("{filler}{tail}")
}

/// #548: a name in scope wins over a same-named item elsewhere: the file's
/// own function (a file over the size cap included, whose fragments were
/// never read), a method of another type (`BTreeMap::entry`) and a local
/// closure (`at`). A `mod` declaration and an `impl Context` header use
/// nothing. The call that
/// does reach each definition is kept.
#[test]
fn a_name_in_scope_shadows_and_an_impl_header_is_no_caller() {
    let export = |gone: bool| {
        let removed = "pub fn to_json(x: u32) -> String {\n    format!(\"{x}\")\n}\n\npub fn entry(k: u32) -> u32 {\n    k + 1\n}\n\npub fn render(x: u32) -> String {\n    x.to_string()\n}\n\n";
        let kept = "pub fn kept() -> u32 {\n    1\n}\n";
        if gone {
            format!("{kept}\npub fn at(i: usize) -> usize {{\n    i * 3\n}}\n")
        } else {
            format!("{removed}{kept}\npub fn at(i: usize) -> usize {{\n    i * 2\n}}\n")
        }
    };
    let ctx = |field: &str, init: &str| {
        format!(
            "pub struct Context {{\n    pub depth: u32,{field}\n}}\n\nimpl Context {{\n    pub fn new() -> Self {{\n        Context {{ depth: 1{init} }}\n    }}\n}}\n\nimpl std::fmt::Display for Context {{\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {{\n        write!(f, \"{{}}\", self.depth)\n    }}\n}}\n"
        )
    };
    let big = over_the_cap(
        "fn to_json(x: u32) -> String {\n    format!(\"<{x}>\")\n}\n\npub fn big_bind() -> String {\n    to_json(9)\n}\n",
    );
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some(
                    "pub mod big;\npub mod bridge;\npub mod ctx;\npub mod export;\npub mod maps;\npub mod render;\npub mod user;\n",
                ),
            ),
            ("src/export.rs", Some(&export(false))),
            ("src/render.rs", Some("pub fn paint() -> u32 {\n    2\n}\n")),
            ("src/big.rs", Some(&big)),
            (
                "src/bridge.rs",
                Some(
                    "use crate::export::kept;\n\nmacro_rules! wrap {\n    ($f:ident, $x:expr) => {\n        $f($x)\n    };\n}\n\nfn to_json(x: u32) -> String {\n    format!(\"[{x}]\")\n}\n\npub fn bind() -> String {\n    wrap!(to_json, kept()) + &to_json(2)\n}\n",
                ),
            ),
            (
                "src/maps.rs",
                Some(
                    "use std::collections::BTreeMap;\n\npub fn count(m: &mut BTreeMap<u32, u32>) -> usize {\n    *m.entry(1).or_insert(0) += 1;\n    let at = |i: usize| i + 1;\n    at(m.len())\n}\n",
                ),
            ),
            ("src/ctx.rs", Some(&ctx("", ""))),
            (
                "src/user.rs",
                Some(
                    "use crate::ctx::Context;\nuse crate::export::to_json;\n\npub fn real() -> String {\n    to_json(3)\n}\n\npub fn drawn() -> String {\n    crate::export::render(4)\n}\n\npub fn make() -> u32 {\n    Context::new().depth\n}\n",
                ),
            ),
        ],
        &[
            ("src/export.rs", Some(&export(true))),
            (
                "src/ctx.rs",
                Some(&ctx("\n    pub wide: bool,", ", wide: false")),
            ),
        ],
    ]);
    let doc = impact_json(tmp.path());
    assert_eq!(
        callers_of(&doc, "to_json", true),
        ["src/user.rs::real"],
        "{doc:#}"
    );
    assert_eq!(
        callers_of(&doc, "render", true),
        ["src/user.rs::drawn"],
        "{doc:#}"
    );
    assert_eq!(callers_of(&doc, "entry", true), [""; 0], "{doc:#}");
    assert_eq!(callers_of(&doc, "at", false), [""; 0], "{doc:#}");
    assert_eq!(
        contracts(&doc, "removed_api"),
        [
            "src/export.rs::entry",
            "src/export.rs::render",
            "src/export.rs::to_json"
        ],
        "{doc:#}"
    );
    assert_eq!(
        callers_of(&doc, "Context", false),
        ["src/user.rs::make"],
        "{doc:#}"
    );
}

/// #484: a call reaches its definition through a `crate::` path, a module a
/// `use` binds, a `use` of the function itself or an alias of it, the
/// library's crate name from the binary, and as a value passed by path.
/// A method is reached through a field whose type the struct states. Not
/// callers: another module's function of the same name and its importer,
/// a method of another type, a macro, a comment and a string.
#[test]
fn calls_through_paths_imports_and_typed_fields_are_callers() {
    let tmp = repo(&[&[
        (
            "Cargo.toml",
            Some(
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\nname = \"_fx\"\n",
            ),
        ),
        (
            "src/lib.rs",
            Some(
                "pub mod alias;\npub mod bound;\npub mod decoy;\npub mod git;\npub mod hook;\npub mod macros;\npub mod meter;\npub mod other;\npub mod pipe;\npub mod resolver;\npub mod via_use;\n",
            ),
        ),
        (
            "src/git.rs",
            Some(
                "pub fn split_range(r: &str) -> (String, Option<String>) {\n    match r.split_once(\"..\") {\n        Some((a, b)) => (a.to_string(), Some(b.to_string())),\n        None => (r.to_string(), None),\n    }\n}\n",
            ),
        ),
        (
            "src/hook.rs",
            Some(
                "pub fn head(range: &str) -> Option<String> {\n    crate::git::split_range(range).1\n}\n",
            ),
        ),
        (
            "src/pipe.rs",
            Some(
                "use crate::git;\n\npub fn heads(ranges: &[&str]) -> Vec<Option<String>> {\n    ranges.iter().map(|r| git::split_range(r)).map(|p| p.1).collect()\n}\n\npub fn bases(ranges: Vec<&str>) -> Vec<(String, Option<String>)> {\n    ranges.into_iter().map(crate::git::split_range).collect()\n}\n",
            ),
        ),
        (
            "src/via_use.rs",
            Some(
                "use crate::git::split_range;\n\npub fn base(range: &str) -> String {\n    split_range(range).0\n}\n",
            ),
        ),
        (
            "src/alias.rs",
            Some(
                "use crate::git::split_range as split;\n\npub fn short(range: &str) -> String {\n    split(range).0\n}\n",
            ),
        ),
        (
            "src/main.rs",
            Some(
                "fn main() {\n    let r = _fx::git::split_range(\"a..b\");\n    println!(\"{:?}\", r);\n}\n",
            ),
        ),
        (
            "src/decoy.rs",
            Some(
                "pub fn split_range(r: &str) -> usize {\n    r.len()\n}\n\npub fn own() -> usize {\n    split_range(\"a..b\")\n}\n\npub fn text() -> &'static str {\n    // crate::git::split_range(x) was the other one\n    \"crate::git::split_range(y)\"\n}\n",
            ),
        ),
        (
            "src/other.rs",
            Some(
                "use crate::decoy::split_range;\n\npub fn len_of(r: &str) -> usize {\n    split_range(r)\n}\n",
            ),
        ),
        (
            "src/meter.rs",
            Some(
                "pub struct Meter;\n\nimpl Meter {\n    pub fn split_range(&self, r: &str) -> usize {\n        r.len()\n    }\n\n    pub fn imports_file(&self, a: &str) -> bool {\n        a.is_empty()\n    }\n}\n\npub fn measure(m: &Meter) -> usize {\n    m.split_range(\"x\") + usize::from(m.imports_file(\"y\"))\n}\n",
            ),
        ),
        (
            "src/macros.rs",
            Some(
                "macro_rules! split_range {\n    ($x:expr) => {\n        $x\n    };\n}\n\npub fn via_macro() -> u32 {\n    split_range!(7)\n}\n",
            ),
        ),
        (
            "src/resolver.rs",
            Some(
                "pub struct Resolver {\n    pub depth: u32,\n}\n\nimpl Resolver {\n    pub fn imports_file(&self, a: &str) -> bool {\n        a.len() as u32 > self.depth\n    }\n}\n",
            ),
        ),
        (
            "src/bound.rs",
            Some(
                "use crate::resolver::Resolver;\n\npub struct Bound {\n    resolver: Resolver,\n}\n\nimpl Bound {\n    pub fn reaches(&self, t: &str) -> bool {\n        self.resolver.imports_file(t)\n    }\n}\n",
            ),
        ),
    ]]);
    let md = symbol_markdown(tmp.path(), "src/git.rs:split_range");
    assert_eq!(
        referrers(&md),
        [
            "src/alias.rs::short",
            "src/hook.rs::head",
            "src/main.rs::main",
            "src/pipe.rs::bases",
            "src/pipe.rs::heads",
            "src/via_use.rs::base"
        ],
        "{md}"
    );
    let md = symbol_markdown(tmp.path(), "src/resolver.rs:imports_file");
    assert_eq!(referrers(&md), ["src/bound.rs::reaches"], "{md}");
}

/// #503: a constant its own file's function reads is referenced by that
/// function, and the inline test calling it reaches the constant through
/// it. Another module's constant of the same name, and the name in a
/// string or a comment, read nothing of it.
#[test]
fn a_constant_its_own_function_reads_is_referenced_and_tested() {
    let cfg = |names: &str| {
        format!(
            "pub const NON_SEMANTIC: &[&str] = &[{names}];\n\npub fn undeclared(seen: &[&str]) -> Vec<String> {{\n    seen.iter()\n        .filter(|s| !NON_SEMANTIC.contains(s))\n        .map(|s| s.to_string())\n        .collect()\n}}\n\npub fn describe() -> &'static str {{\n    // NON_SEMANTIC lists the names\n    \"NON_SEMANTIC\"\n}}\n\n#[cfg(test)]\nmod tests {{\n    use super::*;\n\n    #[test]\n    fn names_the_undeclared() {{\n        assert_eq!(undeclared(&[\"X\"]), vec![\"X\".to_string()]);\n    }}\n}}\n"
        )
    };
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            ("src/lib.rs", Some("pub mod cfg;\npub mod other;\n")),
            ("src/cfg.rs", Some(&cfg("\"A\", \"B\""))),
            (
                "src/other.rs",
                Some(
                    "const NON_SEMANTIC: &[&str] = &[\"Z\"];\n\npub fn own() -> usize {\n    NON_SEMANTIC.len()\n}\n",
                ),
            ),
        ],
        &[("src/cfg.rs", Some(&cfg("\"A\", \"B\", \"C\"")))],
    ]);
    let md = impact_markdown(tmp.path());
    assert_eq!(referrers(&md), ["src/cfg.rs::undeclared"], "{md}");
    assert!(
        md.contains(
            "referenced from src/cfg.rs::undeclared (3-8) — reachable from tests: src/cfg.rs"
        ),
        "{md}"
    );
    assert!(
        !md.lines()
            .next()
            .unwrap_or("")
            .contains("changed definition(s) without a static test link"),
        "{md}"
    );
}

/// #540: a function moved to another module, every importer following it,
/// is no removal. One importer still naming the old module keeps a move a
/// removal with that caller; a same-named function the range adds elsewhere
/// that no importer names leaves a removal too.
#[test]
fn a_move_every_importer_follows_is_no_removal() {
    let def = |name: &str, n: u32| format!("pub fn {name}() -> u32 {{\n    {n}\n}}\n");
    let stays = def("stays", 1);
    let old = format!(
        "{}\n{}\n{}\n{stays}",
        def("tax_rate", 19),
        def("levy", 3),
        def("fee", 5)
    );
    let new_base = def("other", 1);
    let new_head = format!(
        "{new_base}\n{}\n{}\n{}",
        def("tax_rate", 19),
        def("levy", 3),
        def("fee", 50)
    );
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some("pub mod a;\npub mod b;\npub mod c;\npub mod new;\npub mod old;\n"),
            ),
            ("src/old.rs", Some(&old)),
            ("src/new.rs", Some(&new_base)),
            (
                "src/a.rs",
                Some(
                    "use crate::old::tax_rate;\n\npub fn price() -> u32 {\n    tax_rate() * 2 + crate::old::levy()\n}\n",
                ),
            ),
            (
                "src/b.rs",
                Some(
                    "pub fn cost() -> u32 {\n    crate::old::tax_rate() + crate::old::stays()\n}\n",
                ),
            ),
            (
                "src/c.rs",
                Some("pub fn duty() -> u32 {\n    crate::old::levy()\n}\n"),
            ),
        ],
        &[
            ("src/old.rs", Some(&stays)),
            ("src/new.rs", Some(&new_head)),
            (
                "src/a.rs",
                Some(
                    "use crate::new::tax_rate;\n\npub fn price() -> u32 {\n    tax_rate() * 2 + crate::new::levy()\n}\n",
                ),
            ),
            (
                "src/b.rs",
                Some(
                    "pub fn cost() -> u32 {\n    crate::new::tax_rate() + crate::old::stays()\n}\n",
                ),
            ),
        ],
    ]);
    let doc = impact_json(tmp.path());
    assert_eq!(
        contracts(&doc, "removed_api"),
        ["src/old.rs::fee", "src/old.rs::levy"],
        "{doc:#}"
    );
    assert_eq!(
        callers_of(&doc, "levy", true),
        ["src/c.rs::duty"],
        "{doc:#}"
    );
    assert_eq!(callers_of(&doc, "tax_rate", true), [""; 0], "{doc:#}");
}

/// #540, Kotlin: a declaration removed from a file the range renamed is
/// reported under the base path its line numbers belong to, never under
/// the head path, and its caller still resolves. A declaration the renamed
/// file keeps is no removal.
#[test]
fn a_kotlin_removal_from_a_renamed_file_carries_the_base_path() {
    let store = |legacy: bool| {
        let kept: String = (0..12)
            .map(|i| format!("    fun keep{i}(x: Int) = x + {i}\n"))
            .collect();
        let legacy = if legacy { "    fun legacy() = 2\n" } else { "" };
        format!("package a\n\nclass Store {{\n{legacy}{kept}}}\n")
    };
    let tmp = repo(&[
        &[
            ("src/main/kotlin/a/Store.kt", Some(&store(true))),
            (
                "src/main/kotlin/a/Client.kt",
                Some(
                    "package a\n\nclass Client(private val store: Store) {\n    fun go() = store.legacy() + store.keep1(1)\n}\n",
                ),
            ),
        ],
        &[
            ("src/main/kotlin/a/Store.kt", None),
            ("src/main/kotlin/a/Shop.kt", Some(&store(false))),
        ],
    ]);
    let doc = impact_json(tmp.path());
    let removed: Vec<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["kind"] == "removed")
        .map(|c| {
            format!(
                "{}::{} {}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or(""),
                c["lines"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        removed,
        ["src/main/kotlin/a/Store.kt::legacy 4-4"],
        "{doc:#}"
    );
    assert_eq!(
        contracts(&doc, "removed_api"),
        ["src/main/kotlin/a/Store.kt::legacy"],
        "{doc:#}"
    );
    assert_eq!(
        callers_of(&doc, "legacy", true),
        ["src/main/kotlin/a/Client.kt::go"],
        "{doc:#}"
    );
}

/// #594: a function handed on by name (`.and_then(to_ms)`) is used by the
/// function that hands it, which carries its test reach; so does a field
/// of a struct long enough to be read in chunks. Not uses: a parameter
/// and a closure of that name, a field `c.to_ms`, a comment, a string, and
/// another module's own `to_ms` passed the same way.
#[test]
fn a_function_passed_by_name_is_a_use_with_its_test_reach() {
    let lib = |body: &str| {
        format!(
            "pub mod locate;\npub mod other;\n\npub fn parse_all(items: &[&str]) -> Vec<i64> {{\n    items.iter().filter_map(|s| to_ms(s)).collect()\n}}\n\npub fn parse_first(items: &[&str]) -> Option<i64> {{\n    items.first().copied().and_then(to_ms)\n}}\n\npub fn shadowed(items: &[&str], to_ms: fn(&str) -> Option<i64>) -> Vec<i64> {{\n    items.iter().copied().filter_map(to_ms).collect()\n}}\n\npub fn closure(items: &[&str]) -> Vec<i64> {{\n    let to_ms = |s: &str| s.len() as i64;\n    items.iter().copied().map(to_ms).collect()\n}}\n\npub struct Clock {{\n    pub to_ms: i64,\n}}\n\npub fn field(c: &Clock) -> i64 {{\n    c.to_ms\n}}\n\npub fn text() -> &'static str {{\n    // .and_then(to_ms) in a comment\n    \"and_then(to_ms)\"\n}}\n\nfn to_ms(stamp: &str) -> Option<i64> {{\n    {body}\n}}\n\n#[cfg(test)]\nmod tests {{\n    #[test]\n    fn first() {{\n        assert_eq!(super::parse_first(&[\"1\"]), Some(1));\n    }}\n}}\n"
        )
    };
    let fields = |prefix: &str| -> String {
        (0..40)
            .map(|i| format!("    pub {prefix}_{i}: u32,\n"))
            .collect()
    };
    let locate = |extra: &str| {
        format!(
            "pub struct Output {{\n{}    pub coverage: Coverage,\n{}}}\n\npub struct Coverage {{\n    pub clean: bool,{extra}\n}}\n\npub fn schema() -> usize {{\n    std::mem::size_of::<Output>()\n}}\n",
            fields("head"),
            fields("tail")
        )
    };
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            ("src/lib.rs", Some(&lib("stamp.parse().ok()"))),
            ("src/locate.rs", Some(&locate(""))),
            (
                "src/other.rs",
                Some(
                    "fn to_ms(stamp: &str) -> Option<i64> {\n    stamp.len().try_into().ok()\n}\n\npub fn elsewhere(items: &[&str]) -> Vec<i64> {\n    items.iter().copied().filter_map(to_ms).collect()\n}\n",
                ),
            ),
            (
                "tests/schema.rs",
                Some(
                    "#[test]\nfn schema_is_sized() {\n    assert!(fixture::locate::schema() > 0);\n}\n",
                ),
            ),
        ],
        &[
            ("src/lib.rs", Some(&lib("stamp.trim().parse().ok()"))),
            ("src/locate.rs", Some(&locate("\n    pub wide: bool,"))),
        ],
    ]);
    let md = impact_markdown(tmp.path());
    assert_eq!(
        referrers(&md),
        [
            "src/lib.rs::parse_all",
            "src/lib.rs::parse_first",
            "src/locate.rs::Output"
        ],
        "{md}"
    );
    assert!(
        md.contains(
            "referenced from src/lib.rs::parse_first (8-10) — reachable from tests: src/lib.rs"
        ),
        "{md}"
    );
    assert!(
        md.contains("referenced from src/locate.rs::Output (1-83) — reachable from tests: tests/schema.rs via schema"),
        "{md}"
    );
    assert!(
        !md.lines()
            .next()
            .unwrap_or("")
            .contains("changed definition(s) without a static test link"),
        "{md}"
    );
}

/// #484: a method is reached through a value whose type the syntax states
/// without a declaration of its own: what a function returns (`?`
/// unwrapping its `Result`), a type alias, a path through `super::super`.
/// Not callers: a unit struct and a returned value of another type with a
/// method of that name, and another module's free function of that name.
#[test]
fn a_method_is_reached_through_a_returned_or_aliased_value() {
    let graph = |n: u32| {
        format!(
            "pub struct Graph {{\n    pub n: u32,\n}}\n\nimpl Graph {{\n    pub fn new() -> Self {{\n        Graph {{ n: 0 }}\n    }}\n\n    pub fn add_edge(&mut self, w: u32) {{\n        self.n += w + {n};\n    }}\n}}\n\npub type Alias = Graph;\n"
        )
    };
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some("pub mod edges;\npub mod graph;\npub mod make;\n"),
            ),
            ("src/graph.rs", Some(&graph(0))),
            (
                "src/edges/mod.rs",
                Some("pub mod base;\npub mod semantic;\n"),
            ),
            (
                "src/edges/base.rs",
                Some("pub fn add_edge(v: &mut Vec<u32>, w: u32) {\n    v.push(w);\n}\n"),
            ),
            ("src/edges/semantic/mod.rs", Some("pub mod js;\n")),
            (
                "src/edges/semantic/js.rs",
                Some(
                    "use super::super::base::{self};\n\npub fn link(v: &mut Vec<u32>) {\n    base::add_edge(v, 1);\n}\n",
                ),
            ),
            (
                "src/make.rs",
                Some(
                    "use crate::graph::{Alias, Graph};\n\npub struct Unit;\n\nimpl Unit {\n    pub fn add_edge(&self) -> u32 {\n        3\n    }\n}\n\npub fn make() -> Graph {\n    Graph::new()\n}\n\nfn maybe() -> Result<Graph, String> {\n    Ok(make())\n}\n\nfn unit() -> Unit {\n    Unit\n}\n\npub fn by_return() -> u32 {\n    let mut g = make();\n    g.add_edge(1);\n    g.n\n}\n\npub fn by_try() -> Result<u32, String> {\n    let mut g = maybe()?;\n    g.add_edge(2);\n    Ok(g.n)\n}\n\npub fn by_alias(g: &mut Alias) {\n    g.add_edge(3);\n}\n\npub fn by_unit() -> u32 {\n    Unit.add_edge() + unit().add_edge()\n}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn returns() {\n        assert_eq!(super::by_return(), 1);\n    }\n}\n",
                ),
            ),
        ],
        &[("src/graph.rs", Some(&graph(1)))],
    ]);
    let md = symbol_markdown(tmp.path(), "src/graph.rs:add_edge");
    assert_eq!(
        referrers(&md),
        [
            "src/make.rs::by_alias",
            "src/make.rs::by_return",
            "src/make.rs::by_try"
        ],
        "{md}"
    );
    assert!(!md.contains("possibly called"), "{md}");
    assert!(
        md.contains(
            "called from src/make.rs::by_return (23-27) — reachable from tests: src/make.rs"
        ),
        "{md}"
    );
    let md = symbol_markdown(tmp.path(), "src/edges/base.rs:add_edge");
    assert_eq!(referrers(&md), ["src/edges/semantic/js.rs::link"], "{md}");
    assert!(!md.contains("possibly called"), "{md}");
}

/// #540: a method moved to an `impl` of its type in another file is no
/// removal, and its caller is no broken caller. A function removed for
/// good stays a removal, and the files the removal search withholds are
/// the code files that name it, never a withheld data file.
#[test]
fn a_method_moved_to_another_impl_of_its_type_is_no_removal() {
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some("pub mod ledger;\npub mod settle;\npub mod user;\n"),
            ),
            (
                "src/ledger.rs",
                Some(
                    "pub fn levy() -> u32 {\n    3\n}\n\npub struct Ledger;\n\nimpl Ledger {\n    pub fn settle(&self) -> u32 {\n        levy()\n    }\n}\n",
                ),
            ),
            ("src/settle.rs", Some("pub fn other() -> u32 {\n    1\n}\n")),
            (
                "src/user.rs",
                Some(
                    "use crate::ledger::Ledger;\n\npub fn pay(l: &Ledger) -> u32 {\n    l.settle()\n}\n",
                ),
            ),
            (".diffctx/ignore", Some("data/\n")),
            ("data/notes.txt", Some("levy settle levy\n")),
        ],
        &[
            ("src/ledger.rs", Some("pub struct Ledger;\n")),
            (
                "src/settle.rs",
                Some(
                    "use crate::ledger::Ledger;\n\npub fn other() -> u32 {\n    1\n}\n\nimpl Ledger {\n    pub fn settle(&self) -> u32 {\n        4\n    }\n}\n",
                ),
            ),
        ],
    ]);
    let doc = impact_json(tmp.path());
    assert_eq!(callers_of(&doc, "settle", true), [""; 0], "{doc:#}");
    assert_eq!(
        contracts(&doc, "removed_api"),
        ["src/ledger.rs::levy"],
        "{doc:#}"
    );
    assert!(doc.get("withheld_files").is_none_or(|w| w == 1), "{doc:#}");
}

/// #599: a private item is named only in its module and the ones below
/// it, so a file over the size cap elsewhere is no hedge on it. A file
/// below its module keeps the hedge, and so does any file for a
/// `pub(crate)` item.
#[test]
fn a_private_item_is_hedged_only_by_files_that_can_name_it() {
    let paths = |n: u32| {
        format!(
            "mod deep;\n\npub fn full(dir: &str, file: &str) -> String {{\n    join(dir, file) + &split(dir) + &wide(file)\n}}\n\nfn join(a: &str, b: &str) -> String {{\n    format!(\"{{a}}/{{b}}/{n}\")\n}}\n\nfn split(a: &str) -> String {{\n    a.replace('/', \"{n}\")\n}}\n\npub(crate) fn wide(a: &str) -> String {{\n    a.repeat({n})\n}}\n"
        )
    };
    let big = over_the_cap(
        "pub fn table() -> String {\n    [\"a\", \"b\"].join(\"=\") + &\"x\".split('y').count().to_string() + \"wide\"\n}\n",
    );
    let deep = over_the_cap("pub fn under() -> usize {\n    \"a/b\".split('/').count()\n}\n");
    let tmp = repo(&[
        &[
            (MANIFEST.0, Some(MANIFEST.1)),
            (
                "src/lib.rs",
                Some("mod big;\nmod paths;\n\npub use big::table;\npub use paths::full;\n"),
            ),
            ("src/paths.rs", Some(&paths(1))),
            ("src/paths/deep.rs", Some(&deep)),
            ("src/big.rs", Some(&big)),
        ],
        &[("src/paths.rs", Some(&paths(2)))],
    ]);
    let doc = impact_json(tmp.path());
    let missed = |symbol: &str| -> Vec<String> {
        let mut found: Vec<String> = doc["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["symbol"] == symbol)
            .flat_map(|c| c["missed_in"].as_array().cloned().unwrap_or_default())
            .map(|m| m["path"].as_str().unwrap_or("").to_string())
            .collect();
        found.sort();
        found
    };
    assert_eq!(missed("join"), [""; 0], "{doc:#}");
    assert_eq!(missed("split"), ["src/paths/deep.rs"], "{doc:#}");
    assert_eq!(missed("wide"), ["src/big.rs"], "{doc:#}");
    assert_eq!(
        callers_of(&doc, "join", false),
        ["src/paths.rs::full"],
        "{doc:#}"
    );
}
