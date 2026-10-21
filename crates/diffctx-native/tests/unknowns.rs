// What the answer says it could not resolve, and what it can rule out
// (#505): an unknown is named, never read as nothing.

use std::path::Path;

use tempfile::TempDir;

mod support;
use support::{diffctx, fill, repo};

fn symbol_markdown(repo: &Path, name: &str) -> String {
    let out = diffctx()
        .current_dir(repo)
        .args([".", "--symbol", name, "-q", "-f", "md"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

fn impact_markdown_within(repo: &Path, budget: &str) -> String {
    let out = diffctx()
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

fn impact_markdown(repo: &Path) -> String {
    let out = diffctx()
        .current_dir(repo)
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

/// #389: `out = []; out.append(x)` appends to a list, whatever else defines
/// `append`. The same name through a `Report` is the caller, and through a
/// parameter nothing types it stays possible.
#[test]
fn a_builtin_container_receiver_is_no_caller_of_a_same_named_method() {
    let report = |body: &str| {
        format!(
            "class Report:\n    def __init__(self):\n        self.lines = []\n\n    def append(self, text):\n        {body}\n"
        )
    };
    let base = report("self.lines.append(text)");
    let tmp = repo(
        &[
            ("report.py", &base),
            (
                "lists.py",
                "def collect(xs):\n    out = []\n    for x in xs:\n        out.append(x)\n    return out\n",
            ),
            (
                "built.py",
                "def gather():\n    acc = list()\n    acc.append(1)\n    return acc\n",
            ),
            (
                "writer.py",
                "from report import Report\n\n\ndef write():\n    r = Report()\n    r.append(\"x\")\n",
            ),
            ("loose.py", "def emit(w):\n    w.append(\"y\")\n"),
        ],
        &[("report.py", &report("self.lines.append(text.strip())"))],
    );
    let md = impact_markdown(tmp.path());
    assert!(md.contains("called from writer.py::write"), "{md}");
    assert!(md.contains("possibly called from loose.py::emit"), "{md}");
    assert!(!md.contains("lists.py"), "{md}");
    assert!(!md.contains("built.py"), "{md}");
}

/// #389 on TypeScript: `new Map()` is a `Map`, and its `get` is not
/// `PreferencesService.get`.
#[test]
fn a_ts_builtin_map_receiver_is_no_caller_of_get() {
    let service = |n: u32| {
        format!(
            "export class PreferencesService {{\n  get(key: string): number {{\n    return key.length + {n};\n  }}\n}}\n"
        )
    };
    let tmp = repo(
        &[
            ("prefs.ts", &service(1)),
            (
                "cache.ts",
                "const m = new Map<string, number>();\n\nexport function look(k: string) {\n  return m.get(k);\n}\n",
            ),
            (
                "use.ts",
                "import { PreferencesService } from './prefs';\n\nexport function read() {\n  const p = new PreferencesService();\n  return p.get('a');\n}\n",
            ),
        ],
        &[("prefs.ts", &service(2))],
    );
    let md = impact_markdown(tmp.path());
    assert!(md.contains("called from use.ts::read"), "{md}");
    assert!(!md.contains("cache.ts"), "{md}");
}

/// #399: `cart.total` through a parameter nothing types may read the
/// changed property or any other object's. Listing it was noise; dropping
/// it read as "nothing calls them". It is counted as not resolved.
#[test]
fn a_read_through_an_untyped_receiver_is_counted_not_dropped() {
    let cart =
        |n: u32| format!("class Cart:\n    @property\n    def total(self):\n        return {n}\n");
    let tmp = repo(
        &[
            ("cart.py", &cart(1)),
            ("app.py", "def checkout(cart):\n    return cart.total\n"),
        ],
        &[("cart.py", &cart(2))],
    );
    let md = impact_markdown(tmp.path());
    assert!(
        md.contains("callers not resolved: 1 read(s) of `.total`"),
        "{md}"
    );
    assert!(!md.contains("nothing calls them"), "{md}");
    assert!(!md.contains("called from app.py"), "{md}");
}

/// A read and a call through the same untyped receiver (`if engine.switch:
/// engine.switch()`) is a possible caller: the call says more than the read.
#[test]
fn a_call_beside_a_read_through_one_receiver_stays_a_possible_caller() {
    let engine = |n: u32| format!("class Engine:\n    def switch(self):\n        return {n}\n");
    let tmp = repo(
        &[
            ("engine.py", &engine(1)),
            (
                "client.py",
                "def run(engine):\n    if engine.switch:\n        return engine.switch()\n",
            ),
        ],
        &[("engine.py", &engine(2))],
    );
    let md = impact_markdown(tmp.path());
    assert!(md.contains("possibly called from client.py::run"), "{md}");
    assert!(!md.contains("read(s) of `.switch`"), "{md}");
}

/// #427: a CI script feeds Python to the interpreter through a heredoc and
/// that code calls the function. Nothing reads embedded code, so the script
/// is named as not analysed; a script that only echoes the name is not.
#[test]
fn a_script_whose_embedded_code_names_the_symbol_is_named() {
    let tmp = repo(
        &[
            ("src/__init__.py", ""),
            (
                "src/seed_demo_data.py",
                "def seed_demo_data(n):\n    return list(range(n))\n",
            ),
            (
                "scripts/seed.sh",
                "#!/bin/sh\npython - <<'EOF'\nfrom src.seed_demo_data import seed_demo_data\nseed_demo_data(3)\nEOF\n",
            ),
            (
                "scripts/note.sh",
                "#!/bin/sh\necho \"run seed_demo_data after the migration\"\n",
            ),
            (
                "scripts/fmt.sh",
                "#!/bin/sh\npython3 - <<'PY'\n# seed_demo_data runs after this\nprint(\"seed_demo_data\")\nPY\n",
            ),
            (
                "tests/test_seed.py",
                "from src.seed_demo_data import seed_demo_data\n\n\ndef test_seed():\n    assert seed_demo_data(2) == [0, 1]\n",
            ),
        ],
        &[],
    );
    let md = symbol_markdown(tmp.path(), "seed_demo_data");
    assert!(md.contains("its embedded code is not analysed"), "{md}");
    assert!(md.contains("scripts/seed.sh"), "{md}");
    assert!(!md.contains("note.sh"), "{md}");
    assert!(!md.contains("fmt.sh"), "{md}");
    assert!(md.contains("called from tests/test_seed.py"), "{md}");
}

/// #393: the server config's only consumers name it by path, outside
/// Python: the entrypoint's command line and the Dockerfile that copies
/// it. A document or a comment that mentions it is no consumer.
#[test]
fn a_config_named_by_a_command_line_lists_where() {
    let conf = |n: u32| format!("workers = {n}\naccesslog = '-'\n");
    let tmp = repo(
        &[
            ("app/gunicorn_conf.py", &conf(2)),
            (
                "app/docker-entrypoint.sh",
                "#!/bin/sh\nexec gunicorn -c /app/gunicorn_conf.py \"src.app:server\"\n",
            ),
            (
                "app/Dockerfile",
                "FROM python:3.12\nCOPY gunicorn_conf.py docker-entrypoint.sh /app/\n",
            ),
            ("README.md", "Tune workers in app/gunicorn_conf.py.\n"),
            (
                "app/notes.sh",
                "#!/bin/sh\n# app/gunicorn_conf.py is read at boot\necho ok\n",
            ),
        ],
        &[("app/gunicorn_conf.py", &conf(4))],
    );
    let md = impact_markdown(tmp.path());
    assert!(
        md.contains("app/docker-entrypoint.sh:2 names app/gunicorn_conf.py"),
        "{md}"
    );
    assert!(
        md.contains("app/Dockerfile:2 names app/gunicorn_conf.py"),
        "{md}"
    );
    assert!(!md.contains("README.md"), "{md}");
    assert!(!md.contains("notes.sh"), "{md}");
    assert!(!md.contains("No resolved static callers"), "{md}");
}

/// #523: the settings and scripts that run a changed shell script by path,
/// absolute inside the repository or relative. A bare `lint.sh` names it
/// only while one file has that name.
#[test]
fn a_script_run_by_config_lists_the_entries_that_run_it() {
    let tmp = TempDir::new().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let settings = format!(
        "{{\"hooks\": {{\"PreToolUse\": [\n  {{\"command\": \"{}/bin/guard.sh\"}},\n  {{\"command\": \"bin/lint.sh\"}}\n]}}}}\n",
        root.display()
    );
    fill(
        &root,
        &[
            (
                "bin/guard.sh",
                "#!/bin/bash\ncheck() { grep -q x <<<\"$1\"; }\ncheck \"$@\"\n",
            ),
            (
                "bin/lint.sh",
                "#!/bin/bash\nlint() { echo \"$1\"; }\nlint \"$@\"\n",
            ),
            ("other/lint.sh", "#!/bin/bash\necho other\n"),
            ("settings.json", &settings),
            (
                "package.json",
                "{\"scripts\": {\"guard\": \"./bin/guard.sh\"}}\n",
            ),
            ("tools/run.sh", "#!/bin/bash\nbash lint.sh\n"),
            (
                ".github/workflows/ci.yml",
                "on: push\njobs:\n  l:\n    runs-on: ubuntu-latest\n    steps:\n      - run: bash bin/lint.sh a\n",
            ),
        ],
        &[
            (
                "bin/guard.sh",
                "#!/bin/bash\ncheck() { grep -qi x <<<\"$1\"; }\ncheck \"$@\"\n",
            ),
            (
                "bin/lint.sh",
                "#!/bin/bash\nlint() { echo \"lint: $1\"; }\nlint \"$@\"\n",
            ),
        ],
    );
    let md = impact_markdown(&root);
    assert!(md.contains("settings.json:2 names bin/guard.sh"), "{md}");
    assert!(md.contains("settings.json:3 names bin/lint.sh"), "{md}");
    assert!(md.contains("package.json:1 names bin/guard.sh"), "{md}");
    assert!(
        md.contains(".github/workflows/ci.yml:6 names bin/lint.sh"),
        "{md}"
    );
    assert!(!md.contains("tools/run.sh"), "{md}");
}

/// #430: eight changed Scala methods named `make` and `build`, each defined
/// in four objects, say one thing between them. They take one folded line,
/// and the function whose callers resolve keeps them. Scala has no binding
/// model; Rust's resolves the same shape (`tests/lane_rs.rs`).
#[test]
fn names_too_common_to_trace_fold_into_one_line() {
    let ty = |name: &str, n: u32, extra: &str| {
        format!(
            "package app\n\nobject {name} {{\n  def make(): Int = {n}\n  def build(): Int = {n} + 1\n{extra}}}\n"
        )
    };
    let a = |n: u32| ty("A", n, &format!("  def uniqueHelper(): Int = {n}\n"));
    let tmp = repo(
        &[
            (
                "src/app/Lib.scala",
                "package app\n\nobject Lib {\n  def entry(): Int = {\n    A.make() + B.make() + C.make() + D.make() + A.uniqueHelper()\n  }\n}\n",
            ),
            ("src/app/A.scala", &a(1)),
            ("src/app/B.scala", &ty("B", 2, "")),
            ("src/app/C.scala", &ty("C", 3, "")),
            ("src/app/D.scala", &ty("D", 4, "")),
            (
                "src/app/Main.scala",
                "package app\n\nobject Main {\n  def main(args: Array[String]): Unit = {\n    println(Lib.entry() + A.uniqueHelper())\n  }\n}\n",
            ),
        ],
        &[
            ("src/app/A.scala", &a(10)),
            ("src/app/B.scala", &ty("B", 20, "")),
            ("src/app/C.scala", &ty("C", 30, "")),
            ("src/app/D.scala", &ty("D", 40, "")),
        ],
    );
    let md = impact_markdown(tmp.path());
    assert!(
        md.contains("- 8 changed definition(s) with names too common to trace by name (scala has no binding model here"),
        "{md}"
    );
    assert!(!md.contains("callers not resolved: `make`"), "{md}");
    assert!(md.contains("called from src/app/Main.scala::main"), "{md}");
    assert!(md.contains("called from src/app/Lib.scala::entry"), "{md}");
}

/// What names a changed file by path is context, never at a caller's
/// expense: under a tight cap the same callers show with or without the
/// configs that name the file, its omitted-count line included.
#[test]
fn named_by_never_displaces_a_caller() {
    let lib = |n: u32| format!("def total(items):\n    return sum(items) * {n}\n");
    let callers: Vec<(String, String)> = (1..=6)
        .map(|i| {
            (
                format!("use{i}.py"),
                format!("from lib import total\n\n\ndef use{i}(x):\n    return total(x) + {i}\n"),
            )
        })
        .collect();
    let configs: Vec<(String, String)> = (1..=10)
        .map(|i| {
            (
                format!("jobs/run{i}.yml"),
                format!("steps:\n  - run: python lib.py --part {i}\n"),
            )
        })
        .collect();
    let base = |named: bool| {
        let mut files: Vec<(&str, &str)> = vec![("lib.py", "")];
        files.extend(callers.iter().map(|(p, c)| (p.as_str(), c.as_str())));
        if named {
            files.extend(configs.iter().map(|(p, c)| (p.as_str(), c.as_str())));
        }
        files
    };
    let (one, two) = (lib(1), lib(2));
    let build = |named: bool| {
        let mut files = base(named);
        files[0].1 = &one;
        repo(&files, &[("lib.py", &two)])
    };
    let (plain, named) = (build(false), build(true));
    for budget in ["120", "160", "200", "260", "400"] {
        let count = |md: &str| md.matches("called from use").count();
        let (a, b) = (
            impact_markdown_within(plain.path(), budget),
            impact_markdown_within(named.path(), budget),
        );
        assert_eq!(count(&a), count(&b), "budget {budget}:\n{a}\n---\n{b}");
    }
    let md = impact_markdown(named.path());
    assert!(md.contains("jobs/run1.yml:2 names lib.py"), "{md}");
}

fn impact_json(repo: &Path) -> serde_json::Value {
    let out = diffctx()
        .current_dir(repo)
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

/// #559: only the lines naming a file by path are searched for a changed
/// file outside code, so a config, template, query or stylesheet whose
/// content something reads is named as not followed, never a clean zero. A
/// document or a binary is found by its path alone, a comment edit reaches
/// no reader, and code keeps its callers' answer: each control stays empty.
#[test]
fn a_change_whose_readers_nothing_traces_is_named_never_a_clean_zero() {
    let untraced: &[(&str, &str, &str)] = &[
        (
            "template.yaml",
            "spec:\n  synchronization:\n    mutexes:\n      - name: build\n  entrypoint: main\n",
            "spec:\n  entrypoint: main\n",
        ),
        ("settings.json", "{\"retries\": 3}\n", "{\"retries\": 5}\n"),
        ("app.toml", "[pool]\nsize = 4\n", "[pool]\nsize = 8\n"),
        (
            "web/index.html",
            "<div id=\"cart\"></div>\n",
            "<div id=\"basket\"></div>\n",
        ),
        (
            "ui/site.css",
            ".btn { color: red; }\n",
            ".button { color: red; }\n",
        ),
        (
            "Dockerfile",
            "FROM alpine\nENV PORT=80\n",
            "FROM alpine\nENV PORT=8080\n",
        ),
        ("mail/body.j2", "Hello {{ user }}\n", "Hello {{ name }}\n"),
        ("requirements.txt", "requests==2.31\n", "requests==2.32\n"),
        (
            "CMakeLists.txt",
            "add_compile_definitions(MAX_USERS=10)\n",
            "add_compile_definitions(MAX_USERS=20)\n",
        ),
    ];
    let reader = (
        "healer.py",
        "def blocked(wf):\n    return wf.get(\"synchronization\")\n",
    );
    for (path, before, after) in untraced {
        let tmp = repo(&[(path, before), reader], &[(path, after)]);
        let json = impact_json(tmp.path());
        assert_eq!(json["empty"], false, "{path}: {json}");
        let listed = json["unanalysed_files"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            listed
                .iter()
                .any(|f| f["path"] == *path && f["reason"] == "content_untraced"),
            "{path}: {json}"
        );
        let md = impact_markdown(tmp.path());
        assert!(md.contains("not followed"), "{path}: {md}");
        assert!(!md.contains("No resolved static callers"), "{path}: {md}");
    }

    let png_before = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01".to_vec();
    let mut png_after = png_before.clone();
    png_after.push(0x02);
    let png_before = String::from_utf8_lossy(&png_before).into_owned();
    let png_after = String::from_utf8_lossy(&png_after).into_owned();
    let controls: &[(&str, &str, &str)] = &[
        ("README.md", "# shop\n", "# shop\n\nVAT included.\n"),
        ("logo.png", &png_before, &png_after),
        (
            "ci.yaml",
            "jobs:\n  test:\n    run: make\n",
            "# the only job\njobs:\n  test:\n    run: make\n",
        ),
        (
            "lib.py",
            "def f():\n    return 1\n",
            "def f():\n    return 2\n",
        ),
    ];
    for (path, before, after) in controls {
        let tmp = repo(&[(path, before)], &[(path, after)]);
        let json = impact_json(tmp.path());
        assert_eq!(json["empty"], true, "{path}: {json}");
        assert!(json.get("unanalysed_files").is_none(), "{path}: {json}");
    }
}

/// #563: a changed document is named by the config or code that reads it
/// by path. A document naming it — a prose mention, an address with an `@`,
/// even an `@AGENTS.md` import that loads it — is left out: on every edit of
/// the file the same line would repeat (#564).
#[test]
fn a_document_is_named_by_what_reads_it_not_by_other_documents() {
    let tmp = repo(
        &[
            ("AGENTS.md", "# Agents\nRule one.\n"),
            ("CLAUDE.md", "@AGENTS.md\n"),
            ("docs/guide.md", "# Guide\n"),
            ("refs.yaml", "docs: docs/guide.md\nagents: AGENTS.md\n"),
            ("NOTES.md", "Read docs/guide.md and AGENTS.md first.\n"),
            ("CONTACT.md", "Write to ops@AGENTS.md.\n"),
            ("tool.py", "def run():\n    return 1\n"),
            ("README.md", "Run tool.py to start.\n"),
        ],
        &[
            ("AGENTS.md", "# Agents\nRule two.\n"),
            ("docs/guide.md", "# Guide\nx\n"),
            ("tool.py", "def run():\n    return 2\n"),
        ],
    );
    let md = impact_markdown(tmp.path());
    for named in [
        "refs.yaml:1 names docs/guide.md",
        "refs.yaml:2 names AGENTS.md",
    ] {
        assert!(md.contains(named), "{named}: {md}");
    }
    for decoy in ["CLAUDE.md", "NOTES.md", "CONTACT.md", "README.md"] {
        assert!(!md.contains(decoy), "{decoy}: {md}");
    }
}

/// #538: a name five Java classes define is no reason to drop the call
/// inside the definition's own class: `findOne` calling `toReport` is that
/// class's call, and the test reaching `findOne` reaches it. A same-named
/// method in another class is never listed, and since Java's binding model
/// tells the five apart (#335), a private one leaves nothing unresolved.
#[test]
fn an_ambiguous_name_still_resolves_inside_its_own_file() {
    let service = |name: &str, body: &str| {
        format!(
            "package com.example;\n\npublic class {name} {{\n    public Report findOne(String id) {{ return toReport(id); }}\n    private Report toReport(String id) {{\n        return {body};\n    }}\n}}\n"
        )
    };
    let mut base: Vec<(String, String)> = vec![
        (
            "src/main/java/com/example/ReportService.java".into(),
            service("ReportService", "new Report(id)"),
        ),
        (
            "src/test/java/com/example/ReportServiceTest.java".into(),
            "package com.example;\n\nclass ReportServiceTest {\n    @Test\n    void findsOne() {\n        new ReportService().findOne(\"1\");\n    }\n}\n".into(),
        ),
    ];
    for other in ["Alpha", "Beta", "Gamma", "Delta"] {
        base.push((
            format!("src/main/java/com/example/{other}Service.java"),
            service(&format!("{other}Service"), "new Report(id)"),
        ));
    }
    let changed = service("ReportService", "new Report(id.trim())");
    let base: Vec<(&str, &str)> = base.iter().map(|(p, c)| (p.as_str(), c.as_str())).collect();
    let tmp = repo(
        &base,
        &[("src/main/java/com/example/ReportService.java", &changed)],
    );
    let md = impact_markdown(tmp.path());
    assert!(
        md.contains("called from src/main/java/com/example/ReportService.java::findOne"),
        "{md}"
    );
    assert!(md.contains("ReportServiceTest.java"), "{md}");
    assert!(!md.contains("not resolved"), "{md}");
    assert!(!md.contains("AlphaService"), "{md}");
}

/// #566: a member a supertype, an interface or the runtime can call is not
/// one nothing calls. With no direct caller, each language's route is named
/// instead of a clean zero; a private helper, a free function and a method
/// with a direct caller keep the answer they had.
#[test]
fn a_member_only_dispatch_can_reach_is_named_never_dead() {
    let dispatched: &[(&str, &str, &str, &str)] = &[
        (
            "src/Square.java",
            "public class Square implements Shape {\n    @Override\n    public double area() {\n        return 1.0;\n    }\n}\n",
            "public class Square implements Shape {\n    @Override\n    public double area() {\n        return 2.0;\n    }\n}\n",
            "`Shape`",
        ),
        (
            "money.py",
            "class Money:\n    def __eq__(self, other):\n        return True\n",
            "class Money:\n    def __eq__(self, other):\n        return False\n",
            "`__eq__`",
        ),
        (
            "writer.go",
            "package main\n\ntype Upper struct{}\n\nfunc (u Upper) Write(p []byte) (int, error) {\n\treturn 1, nil\n}\n",
            "package main\n\ntype Upper struct{}\n\nfunc (u Upper) Write(p []byte) (int, error) {\n\treturn 2, nil\n}\n",
            "interface",
        ),
        (
            "html.ts",
            "export class HtmlRenderer implements Renderer {\n  render(x: number): string {\n    return `<b>${x}</b>`;\n  }\n}\n",
            "export class HtmlRenderer implements Renderer {\n  render(x: number): string {\n    return `<i>${x}</i>`;\n  }\n}\n",
            "`Renderer`",
        ),
        (
            "Worker.kt",
            "class Worker(val n: Int) : Runnable {\n    override fun run() {\n        println(n)\n    }\n}\n",
            "class Worker(val n: Int) : Runnable {\n    override fun run() {\n        println(n + 1)\n    }\n}\n",
            "`Runnable`",
        ),
    ];
    for (path, before, after, via) in dispatched {
        let tmp = repo(&[(path, before)], &[(path, after)]);
        let json = impact_json(tmp.path());
        assert_eq!(json["empty"], false, "{path}: {json}");
        let unresolved = json["changed"][0]["unresolved"].as_str().unwrap_or("");
        assert!(
            unresolved.contains(via) && unresolved.contains("not traced"),
            "{path}: {json}"
        );
    }

    let controls: &[(&str, &str, &str)] = &[
        (
            "src/Square.java",
            "public class Square implements Shape {\n    private double helper() {\n        return 1.0;\n    }\n}\n",
            "public class Square implements Shape {\n    private double helper() {\n        return 2.0;\n    }\n}\n",
        ),
        (
            "money.py",
            "class Money(Base):\n    def _helper(self):\n        return 1\n",
            "class Money(Base):\n    def _helper(self):\n        return 2\n",
        ),
        (
            "util.go",
            "package main\n\nfunc helper() int {\n\treturn 1\n}\n",
            "package main\n\nfunc helper() int {\n\treturn 2\n}\n",
        ),
        (
            "util.ts",
            "export function helper(): number {\n  return 1;\n}\n",
            "export function helper(): number {\n  return 2;\n}\n",
        ),
    ];
    for (path, before, after) in controls {
        let tmp = repo(&[(path, before)], &[(path, after)]);
        let json = impact_json(tmp.path());
        assert_eq!(json["empty"], true, "{path}: {json}");
    }

    let tmp = repo(
        &[
            (
                "shop/money.py",
                "class Money(Base):\n    def total(self):\n        return 1\n",
            ),
            (
                "shop/cart.py",
                "from shop.money import Money\n\n\ndef pay():\n    return Money().total()\n",
            ),
        ],
        &[(
            "shop/money.py",
            "class Money(Base):\n    def total(self):\n        return 2\n",
        )],
    );
    let json = impact_json(tmp.path());
    let total = &json["changed"][0];
    assert!(
        total["callers"].as_array().is_some_and(|c| !c.is_empty()),
        "{json}"
    );
    assert!(total.get("unresolved").is_none(), "{json}");
}

/// #442: an edit that changes the code's tokens, the block a line sits in
/// or a string's content is never layout, so its callers are listed. Each
/// has a formatting look-alike that stays layout and says its callers were
/// not traced: a call's trailing comma beside a tuple's, a whole-body
/// reindent beside a dedent out of a loop, a `;` beside a line break after
/// `return`, quote style beside a Go rune, names reordered beside aliases
/// swapped, whitespace and blank lines between tokens beside whitespace in
/// a string.
#[test]
fn an_edit_a_formatter_cannot_make_is_never_layout() {
    let py = (
        "main.py",
        "from calc import tally\n\n\ndef run():\n    return tally([1])\n",
        "called from main.py::run",
    );
    let js = (
        "main.js",
        "import { tally } from './calc.js';\n\nexport function run() {\n  return tally(1);\n}\n",
        "called from main.js::run",
    );
    let loop_body =
        |inner: &str| format!("def tally(xs):\n    s = 0\n    for x in xs:\n{inner}    return s\n");
    let ret = |e: &str| format!("def tally(x):\n    return {e}\n");
    let fun = |body: &str| format!("export function tally(a, b) {{\n{body}}}\n");
    let semantic: Vec<(&str, String, String, (&str, &str, &str))> = vec![
        (
            "calc.py",
            loop_body("        if x:\n            s += x\n        s += 1\n"),
            loop_body("        if x:\n            s += x\n    s += 1\n"),
            py,
        ),
        (
            "calc.py",
            loop_body("        s += x\n\n        s += 1\n"),
            loop_body("        s += x\n\n    s += 1\n"),
            py,
        ),
        ("calc.py", ret("(x,)"), ret("(x)"), py),
        ("calc.py", ret("x in ('ab',)"), ret("x in ('ab')"), py),
        ("calc.py", ret("d[1,]"), ret("d[1]"), py),
        (
            "calc.py",
            "def tally():\n    return \"\"\"\n# Release v1\n\"\"\"\n".into(),
            "def tally():\n    return \"\"\"\n# Release v2\n\"\"\"\n".into(),
            py,
        ),
        (
            "calc.py",
            "def tally():\n    return \"\"\"\nkeep  two\n\"\"\"\n".into(),
            "def tally():\n    return \"\"\"\nkeep two\n\"\"\"\n".into(),
            py,
        ),
        (
            "calc.py",
            "def tally():\n    from m import A as B, C as D\n    return B() + D()\n".into(),
            "def tally():\n    from m import A as D, C as B\n    return B() + D()\n".into(),
            py,
        ),
        (
            "calc.js",
            fun("  return a;\n"),
            fun("  return\n    a;\n"),
            js,
        ),
        (
            "calc.js",
            fun("  return /a +/.test(a);\n"),
            fun("  return /a+/.test(a);\n"),
            js,
        ),
        (
            "calc.js",
            fun("  return a + ++b;\n"),
            fun("  return a++ + b;\n"),
            js,
        ),
        (
            "calc.js",
            fun("  return `\n    https://a.example/${a}\n  `;\n"),
            fun("  return `\n    https://b.example/${a}\n  `;\n"),
            js,
        ),
        (
            "calc.go",
            "package m\n\nfunc Tally() any {\n\treturn 'a'\n}\n".into(),
            "package m\n\nfunc Tally() any {\n\treturn \"a\"\n}\n".into(),
            (
                "main.go",
                "package m\n\nfunc Run() {\n\tTally()\n}\n",
                "called from main.go::Run",
            ),
        ),
        (
            "src/calc.rs",
            "pub fn tally() -> (i32,) {\n    (1,)\n}\n".into(),
            "pub fn tally() -> (i32,) {\n    (1)\n}\n".into(),
            (
                "src/main.rs",
                "mod calc;\n\nfn main() {\n    println!(\"{:?}\", calc::tally());\n}\n",
                "called from src/main.rs::main",
            ),
        ),
    ];
    for (path, before, after, (caller, caller_src, listed)) in &semantic {
        let tmp = repo(&[(path, before), (caller, caller_src)], &[(path, after)]);
        let md = impact_markdown(tmp.path());
        assert!(md.contains(listed), "{path}: {after:?}\n{md}");
        assert!(
            !md.contains("callers not traced"),
            "{path}: {after:?}\n{md}"
        );
    }
    for (before, after) in [
        ("a:\n  b: 1\n  d: 2\n", "a:\n  b: 1\nd: 2\n"),
        ("a: 1\nb: 2\n", "a:1\nb: 2\n"),
    ] {
        let tmp = repo(&[("schemas/s.yaml", before)], &[("schemas/s.yaml", after)]);
        let json = impact_json(tmp.path());
        let contracts = json["contracts"].as_array().cloned().unwrap_or_default();
        assert!(
            contracts.iter().any(|c| c["kind"] == "schema"),
            "{after:?}: {json}"
        );
    }

    let layout: Vec<(&str, String, String, (&str, &str, &str))> = vec![
        (
            "calc.py",
            loop_body("        s += x\n        s += 1\n"),
            "def tally(xs):\n  s = 0\n  for x in xs:\n    s += x\n    s += 1\n  return s\n".into(),
            py,
        ),
        ("calc.py", ret("g(x,)"), ret("g(x)"), py),
        ("calc.py", ret("x + 'a'"), ret("x + \"a\""), py),
        (
            "calc.py",
            ret("x"),
            "def tally(x):\n    return x   \n".into(),
            py,
        ),
        (
            "calc.py",
            "def tally(x):\n    y = x\n    return y\n".into(),
            "def tally(x):\n    # kept\n    y = x\n\n    return y\n".into(),
            py,
        ),
        (
            "calc.py",
            "def tally():\n    from m import C, A\n    return A() + C()\n".into(),
            "def tally():\n    from m import A, C\n    return A() + C()\n".into(),
            py,
        ),
        (
            "calc.py",
            ret("\"keep  two\""),
            "def tally(x):\n    return   \"keep  two\"\n".into(),
            py,
        ),
        ("calc.js", fun("  return a\n"), fun("  return a;\n"), js),
        (
            "calc.js",
            fun("  const c = a + b;\n  return c;\n"),
            fun("      const c = a + b;\n      return c;\n"),
            js,
        ),
    ];
    for (path, before, after, (caller, caller_src, listed)) in &layout {
        let tmp = repo(&[(path, before), (caller, caller_src)], &[(path, after)]);
        let json = impact_json(tmp.path());
        let skipped = json["layout_only"].as_array().cloned().unwrap_or_default();
        assert!(
            skipped.iter().any(|s| s
                .as_str()
                .is_some_and(|s| s.starts_with(&format!("{path}::")))),
            "{path}: {after:?}\n{json}"
        );
        let md = impact_markdown(tmp.path());
        assert!(!md.contains(listed), "{path}: {after:?}\n{md}");
        assert!(md.contains("callers not traced"), "{path}: {after:?}\n{md}");
        assert!(
            !md.contains("No resolved static callers"),
            "{path}: {after:?}\n{md}"
        );
    }
}

fn pack_json(repo: &Path, extra: &[&str], polls: Option<u64>) -> serde_json::Value {
    let mut cmd = diffctx();
    cmd.current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "-q",
            "-f",
            "json",
            "--timeout",
            "600",
        ])
        .args(extra);
    if let Some(n) = polls {
        cmd.env("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", n.to_string());
    }
    let out = cmd.output().expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("pack JSON")
}

/// #550: a run nothing limited carries no coverage block, so when the fit
/// to `--budget` cuts it the block is added afterwards and nobody counted
/// what the run read. Its `resources` stay the zeros the schema requires,
/// and `unknown` names them. A block the run wrote itself was measured and
/// names nothing; a run that fits has no block at all.
#[test]
fn a_coverage_block_added_by_the_fit_names_its_counts_unknown() {
    // Eight changed functions: 900 tokens fit a few of them with room on
    // both sides, so the envelope's own size (its latencies) cannot move
    // the cut to all or nothing.
    let calc = |k: u32| -> String {
        (0..8)
            .map(|i| {
                format!(
                    "def step_{i}(values):\n    total = 0\n    for value in values:\n        if value > {i}:\n            total += value * {i}\n        else:\n            total -= value\n    return total + {k}\n\n\n"
                )
            })
            .collect()
    };
    let tmp = repo(
        &[
            ("calc.py", &calc(1)),
            (
                "main.py",
                "import calc\n\n\ndef run(xs):\n    return [getattr(calc, f'step_{i}')(xs) for i in range(8)]\n",
            ),
        ],
        &[("calc.py", &calc(2))],
    );
    let cut = pack_json(tmp.path(), &["--budget", "900"], None);
    assert!(
        cut["fragment_count"].as_u64().is_some_and(|n| n > 0),
        "{cut}"
    );
    let coverage = &cut["coverage"];
    assert!(
        coverage["limit_reasons"]
            .as_array()
            .is_some_and(|r| r.iter().any(|r| r == "selection_budget_exceeded")),
        "{cut}"
    );
    assert_eq!(
        coverage["unknown"],
        serde_json::json!(["resources"]),
        "{cut}"
    );
    assert!(coverage["resources"].is_object(), "{cut}");

    let measured = pack_json(tmp.path(), &[], Some(0));
    assert_eq!(measured["coverage"]["status"], "partial", "{measured}");
    assert!(measured["coverage"].get("unknown").is_none(), "{measured}");

    let whole = pack_json(tmp.path(), &[], None);
    assert!(whole.get("coverage").is_none(), "{whole}");
}

/// #542: `--symbol` reads the files that name the symbol, so a module over
/// the candidate size cap that subclasses it is a caller, and a test over
/// it that calls it is a test link. A test over the cap that names only a
/// caller is not read, and that caller says so instead of reading as
/// untested. A comment and a string in an over-cap file stay no caller.
#[test]
fn a_file_over_the_size_cap_that_names_the_symbol_is_read() {
    let pad = |what: &str, n: usize| -> String {
        (0..n)
            .map(|i| format!("\n\ndef {what}_{i}(x):\n    '''Padding {i} of a long module.'''\n    return x + {i}\n"))
            .collect()
    };
    let server = format!(
        "from pkg.base import StrictServer\n\n\nclass ChildServer(StrictServer):\n    def call_tool(self, name):\n        return name\n{}",
        pad("tool", 1500)
    );
    let notes = format!(
        "# StrictServer is the base every server extends.\nLABEL = \"StrictServer\"\n{}",
        pad("note", 1500)
    );
    let base_test = format!(
        "from pkg.base import StrictServer\n\n\ndef test_strict():\n    assert StrictServer().call_tool('a') == 'a'\n{}",
        pad("test_pad", 1500)
    );
    let child_test = format!(
        "from pkg.server import ChildServer\n\n\ndef test_child():\n    assert ChildServer().call_tool('a') == 'a'\n{}",
        pad("test_more", 1500)
    );
    let docs_test = format!(
        "# make is documented here, never called\nUSAGE = \"make builds one\"\n{}",
        pad("test_docs", 1500)
    );
    let tmp = repo(
        &[
            ("pkg/__init__.py", ""),
            (
                "pkg/base.py",
                "class StrictServer:\n    def call_tool(self, name):\n        return name\n",
            ),
            (
                "pkg/small.py",
                "from pkg.base import StrictServer\n\n\ndef make():\n    return StrictServer()\n",
            ),
            ("pkg/server.py", &server),
            ("pkg/notes.py", &notes),
            ("tests/test_base.py", &base_test),
            ("tests/test_child.py", &child_test),
            ("tests/test_docs.py", &docs_test),
        ],
        &[],
    );
    for big in [
        "pkg/server.py",
        "pkg/notes.py",
        "tests/test_base.py",
        "tests/test_child.py",
        "tests/test_docs.py",
    ] {
        let len = std::fs::metadata(tmp.path().join(big)).unwrap().len();
        assert!(len > 100_000, "{big} is {len} bytes, under the cap");
    }
    let md = symbol_markdown(tmp.path(), "StrictServer");
    assert!(md.contains("from pkg/server.py::ChildServer"), "{md}");
    assert!(
        md.contains(
            "called from tests/test_base.py::test_strict (4-5) — reachable from tests: tests/test_base.py"
        ),
        "{md}"
    );
    assert!(
        md.contains("tests/test_child.py names it but was not read (over the size cap)"),
        "{md}"
    );
    assert!(
        md.contains("called from pkg/small.py::make (4-5) — no static test link found\n"),
        "{md}"
    );
    assert!(!md.contains("pkg/notes.py"), "{md}");
    assert!(!md.contains("not fully analysed"), "{md}");
}

/// #432: an Astro content entry is read by its collection's name, never
/// imported or named by path, so its readers were a clean zero. Each call
/// naming the collection is listed — split over lines, through
/// `getEntry`, or with a name computed at run time; the same call for
/// another collection, in a comment, in a string or outside the Astro
/// project lists nothing. An edit outside any collection keeps its zero.
#[test]
fn a_content_entry_lists_the_calls_reading_its_collection() {
    let base = [
        (
            "site/astro.config.mjs",
            "import { defineConfig } from 'astro/config';\n\nexport default defineConfig({});\n",
        ),
        (
            "site/src/content/projects/a.md",
            "---\ntitle: A\norder: 1\n---\n\nProject A.\n",
        ),
        (
            "site/src/content/posts/hello.md",
            "---\ntitle: Hello\n---\n\nHi.\n",
        ),
        (
            "site/src/pages/projects/[slug].astro",
            "---\nimport { getCollection } from 'astro:content';\n\nexport async function getStaticPaths() {\n  const projects = (await getCollection('projects')).sort((a, b) => a.data.order - b.data.order);\n  return projects.map((p) => ({ params: { slug: p.slug } }));\n}\n---\n<h1>Projects</h1>\n",
        ),
        (
            "site/src/pages/index.astro",
            "---\nimport { getCollection } from 'astro:content';\nconst all = await getCollection(\n  'projects',\n  ({ data }) => data.order > 0,\n);\n---\n<p>{all.length}</p>\n",
        ),
        (
            "site/src/pages/blog.astro",
            "---\nimport { getCollection } from 'astro:content';\nconst posts = await getCollection('posts');\n---\n<ul>{posts.map((p) => <li>{p.data.title}</li>)}</ul>\n",
        ),
        (
            "site/src/lib/featured.ts",
            "import { getEntry } from 'astro:content';\n\nexport async function featured() {\n  // getCollection('projects') is not called here\n  return getEntry('projects', 'a');\n}\n",
        ),
        (
            "site/src/lib/any.ts",
            "import { getCollection } from 'astro:content';\n\nexport const load = (name: string) => getCollection(name);\n",
        ),
        (
            "site/src/lib/label.ts",
            "export const hint = \"call getCollection('projects') to list them\";\n",
        ),
        (
            "tools/list.js",
            "export const all = () => getCollection('projects');\n",
        ),
        ("site/README.md", "# Site\n"),
    ];
    let tmp = repo(
        &base,
        &[(
            "site/src/content/projects/a.md",
            "---\ntitle: A\norder: 3\n---\n\nProject A.\n",
        )],
    );
    let md = impact_markdown(tmp.path());
    for listed in [
        "0 caller(s) outside the diff, 4 read(s) by collection name",
        "site/src/pages/projects/[slug].astro:5 reads collection projects",
        "site/src/pages/index.astro:3 reads collection projects",
        "site/src/lib/featured.ts:5 reads collection projects",
        "site/src/lib/any.ts:3 reads a collection named at run time",
    ] {
        assert!(md.contains(listed), "{listed}\n{md}");
    }
    for decoy in ["blog.astro", "featured.ts:4", "label.ts", "tools/list.js"] {
        assert!(!md.contains(decoy), "{decoy}\n{md}");
    }
    assert!(!md.contains("No resolved static callers"), "{md}");
    let json = impact_json(tmp.path());
    assert_eq!(json["empty"], false, "{json}");
    assert_eq!(
        json["collection_reads"].as_array().map(Vec::len),
        Some(4),
        "{json}"
    );

    let control = repo(&base, &[("site/README.md", "# Site\n\nA portfolio.\n")]);
    let json = impact_json(control.path());
    assert_eq!(json["empty"], true, "{json}");
    assert!(json.get("collection_reads").is_none(), "{json}");
}

/// #579: a call inside a long function is a call from that function. Its
/// chunk (`processA[7]`) was reported as the caller, and no test edge
/// reaches a chunk, so a function the tests drive read untested. The
/// function is the caller, with every call site and the test reach of its
/// own callers; a long function no test reaches stays untested, and a
/// short one is listed as before.
#[test]
fn a_call_in_a_long_function_is_the_function_s_call() {
    let steps = |n: usize| -> String {
        (0..n)
            .map(|i| format!("        total += step{i}(pending.size)\n"))
            .collect()
    };
    let helpers: String = (0..30)
        .map(|i| format!("    private fun step{i}(n: Int): Int = n + {i}\n"))
        .collect();
    let worker = format!(
        "package demo\n\nclass Worker(private val store: Store) {{\n    fun run() {{ processA(); processB() }}\n\n    fun processA(): Int {{\n        var total = 0\n        val pending =\n            (\n                store.findPending(2) +\n                    store.findMissing(2)\n            ).distinct()\n{}        total += store.findPending(1).size\n{}        return total\n    }}\n\n    fun processB(): Int {{\n        val xs = store.findPending(3)\n        return xs.size\n    }}\n\n    fun processC(): Int {{\n        var total = 0\n        val pending = store.findPending(4)\n{}        return total\n    }}\n\n{helpers}}}\n",
        steps(15),
        steps(15),
        steps(30),
    );
    let tmp = repo(
        &[
            (
                "src/main/kotlin/demo/Store.kt",
                "package demo\n\nclass Store {\n    fun findPending(limit: Int): List<Int> = (0 until limit).toList()\n\n    fun findMissing(limit: Int): List<Int> = (limit until 2 * limit).toList()\n}\n",
            ),
            ("src/main/kotlin/demo/Worker.kt", &worker),
            (
                "src/test/kotlin/demo/WorkerTest.kt",
                "package demo\n\nimport kotlin.test.Test\n\nclass WorkerTest {\n    @Test\n    fun runs() {\n        Worker(Store()).run()\n    }\n}\n",
            ),
        ],
        &[],
    );
    let md = symbol_markdown(tmp.path(), "findPending");
    let caller = |name: &str| {
        md.lines()
            .find(|l| l.contains(&format!("Worker.kt::{name}")))
            .unwrap_or_else(|| panic!("{name}\n{md}"))
            .to_string()
    };
    let a = caller("processA");
    assert!(a.contains("processA (6-"), "{md}");
    assert!(
        a.ends_with("reachable from tests: src/test/kotlin/demo/WorkerTest.kt via run"),
        "{md}"
    );
    assert!(
        caller("processB")
            .ends_with("reachable from tests: src/test/kotlin/demo/WorkerTest.kt via run"),
        "{md}"
    );
    let c = caller("processC");
    assert!(
        !c.contains("processC[") && !c.contains("reachable from tests"),
        "{md}"
    );
    assert!(!md.contains("processA["), "{md}");
    assert!(
        md.contains("3 caller(s), 1 caller(s) without a static test link"),
        "{md}"
    );
}

/// #562: a Kotlin test reaching its subject through a helper,
/// `private fun client() = ApiClient(…)`, left `client().f()` with an
/// untyped receiver, so a function whose test came with it read "no
/// static test link". The helper's expression body types the receiver.
/// A helper returning another class with a method of the same name links
/// nothing, and a direct `ApiClient().f()` links as before.
#[test]
fn a_test_helper_s_expression_body_types_the_receiver() {
    let client = |body: &str| {
        format!("package demo\n\nclass ApiClient {{\n    fun ping(): Int = 1\n{body}}}\n")
    };
    let added = "\n    fun fetchListeningSummary(days: Int): List<Int> {\n        return (0 until days).toList()\n    }\n";
    let test = |helper: &str, call: &str| {
        format!(
            "package demo\n\nimport kotlin.test.Test\nimport kotlin.test.assertEquals\n\nclass ApiClientListeningTest {{\n{helper}\n    @Test\n    fun fetchesSummary() {{\n        assertEquals(30, {call}.fetchListeningSummary(30).size)\n    }}\n}}\n"
        )
    };
    let base = [
        ("app/src/main/java/demo/ApiClient.kt", client("")),
        (
            "app/src/main/java/demo/Other.kt",
            "package demo\n\nclass Other {\n    fun fetchListeningSummary(days: Int): List<Int> = listOf(days)\n}\n".to_string(),
        ),
        (
            "app/src/test/java/demo/PingTest.kt",
            "package demo\n\nimport kotlin.test.Test\n\nclass PingTest {\n    @Test\n    fun pings() {\n        ApiClient().ping()\n    }\n}\n".to_string(),
        ),
    ];
    let base: Vec<(&str, &str)> = base.iter().map(|(p, c)| (*p, c.as_str())).collect();
    let run = |helper: &str, call: &str| {
        let after = client(added);
        let test = test(helper, call);
        let tmp = repo(
            &base,
            &[
                ("app/src/main/java/demo/ApiClient.kt", &after),
                ("app/src/test/java/demo/ApiClientListeningTest.kt", &test),
            ],
        );
        impact_markdown(tmp.path())
    };
    for (helper, call) in [
        (
            "    private fun client(path: String = \"/\") = ApiClient()\n",
            "client()",
        ),
        ("", "ApiClient()"),
    ] {
        let md = run(helper, call);
        assert!(!md.contains("without a static test link"), "{call}\n{md}");
    }
    let md = run("    private fun other() = Other()\n", "other()");
    assert!(
        md.contains("1 changed definition(s) without a static test link"),
        "{md}"
    );
}

/// #505, #613: a zero needs a witness. A module a CronWorkflow runs by
/// name, and a function only an `.astro` page calls, read as "0 callers"
/// though impact never looked where they are named. Now the config line is
/// named, the function says its callers may be missing, and the header
/// carries no bare zero. A document mentioning the name is no reader.
#[test]
fn a_name_spelled_where_nothing_is_traced_is_no_zero() {
    let infra = repo(
        &[
            (
                "controls/foo_check.py",
                "LIMIT = 5\n\n\ndef main():\n    print(\"checked\", LIMIT)\n",
            ),
            (
                "deploy/cron-foo-check.yaml",
                "kind: CronWorkflow\nspec:\n  workflowSpec:\n    arguments:\n      parameters:\n        - {name: control, value: foo_check}\n",
            ),
        ],
        &[(
            "controls/foo_check.py",
            "LIMIT = 7\n\n\ndef main():\n    print(\"checked\", LIMIT)\n",
        )],
    );
    let doc = impact_json(infra.path());
    assert!(
        doc["named_by"]
            .to_string()
            .contains("deploy/cron-foo-check.yaml"),
        "{doc:#}"
    );
    let site = repo(
        &[
            (
                "src/lib/dates.ts",
                "export function formatDate(d: Date): string {\n  return d.toISOString();\n}\n",
            ),
            (
                "src/pages/index.astro",
                "---\nimport { formatDate } from \"../lib/dates\";\n---\n<p>{formatDate(new Date())}</p>\n",
            ),
            ("README.md", "Uses formatDate for dates.\n"),
            (
                "src/lib/total.ts",
                "export function total(xs: number[]) {\n  return xs.length;\n}\n",
            ),
            ("data/labels.json", "{\"label\": \"total\"}\n"),
        ],
        &[
            (
                "src/lib/dates.ts",
                "export function formatDate(d: Date): string {\n  return d.toISOString().slice(0, 10);\n}\n",
            ),
            (
                "src/lib/total.ts",
                "export function total(xs: number[]) {\n  return xs.length + 0;\n}\n",
            ),
        ],
    );
    let doc = impact_json(site.path());
    assert_eq!(doc["empty"], false, "{doc:#}");
    let missed = doc["changed"].to_string();
    assert!(missed.contains("src/pages/index.astro"), "{doc:#}");
    assert!(!missed.contains("README.md"), "{doc:#}");
    // A plain word in a data file is no reader of a function named `total`.
    assert!(!missed.contains("data/labels.json"), "{doc:#}");
    let text = impact_markdown(site.path());
    assert!(!text.contains(" 0 caller(s)"), "{text}");
    assert!(text.contains("callers may be missing"), "{text}");
}
