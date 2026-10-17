// Rust callers, definitions and test links (#506 B, Rust lane): each gate
// pairs the true answer with look-alikes that must not join it — a
// same-named function of a module nobody imports, a method of the same
// name on another type, the name in a string or a comment, a macro of
// that name — and a control that has to come back exactly.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

const MANIFEST: (&str, &str) = (
    "Cargo.toml",
    "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
);

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
    let out = Command::new(&*BIN)
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
