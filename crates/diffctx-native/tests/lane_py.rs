// Python definitions, callers and test reach read off the syntax (#506):
// each gate pairs the true sites with look-alikes that must not become one
// (a same-named attribute of another class, the name in a string and in a
// comment, a same-named definition in another module) and a control the
// base already models, answered exactly.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

mod support;
use support::diffctx;

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

/// A repository whose last commit applies `change` on top of `base`.
fn repo(base: &[(&str, &str)], change: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    for (step, files) in [base, change]
        .into_iter()
        .filter(|f| !f.is_empty())
        .enumerate()
    {
        for (rel, content) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", &format!("step {step}")]);
    }
    tmp
}

fn run(repo: &Path, args: &[&str]) -> serde_json::Value {
    let out = diffctx()
        .current_dir(repo)
        .args(args)
        .args(["-q", "-f", "json"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

fn symbol(repo: &Path, name: &str) -> serde_json::Value {
    run(repo, &[".", "--symbol", name])
}

fn impact(repo: &Path) -> serde_json::Value {
    run(repo, &["--diff", "HEAD~1..HEAD", "--mode", "impact"])
}

fn changed<'v>(doc: &'v serde_json::Value, path: &str, name: &str) -> &'v serde_json::Value {
    doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|c| c["path"] == path && c["symbol"] == name)
        .unwrap_or_else(|| panic!("no changed {path}::{name} in {doc}"))
}

/// `path::symbol` of every caller listed for `path::name`.
fn callers(doc: &serde_json::Value, path: &str, name: &str) -> BTreeSet<String> {
    changed(doc, path, name)["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

/// #420: a module-level constant, annotated or plain, is a definition, and
/// its readers — by name, under an alias, through the module — are its
/// callers. Another module's same-named constant, a class attribute of the
/// same name, the name in a string or a comment are not.
#[test]
fn a_module_level_constant_is_defined_and_read() {
    let tmp = repo(
        &[
            ("kit/__init__.py", ""),
            (
                "kit/consts.py",
                "from typing import Any\n\nTABLE: dict[str, Any] = {\"a\": 1}\nPLAIN = {\"b\": 2}\n\n\ndef use():\n    return TABLE[\"a\"], PLAIN[\"b\"]\n\n\ndef lookup(key):\n    return key\n",
            ),
            (
                "kit/reader.py",
                "from kit.consts import TABLE, lookup\n\n\ndef read():\n    return TABLE[\"a\"]\n\n\ndef find():\n    return lookup(\"a\")\n",
            ),
            (
                "kit/alias.py",
                "from kit.consts import TABLE as T\n\n\ndef via_alias():\n    return T[\"a\"]\n",
            ),
            (
                "kit/qualified.py",
                "from kit import consts\n\n\ndef through_module():\n    return consts.PLAIN\n",
            ),
            (
                "kit/other.py",
                "TABLE = \"decoy\"\n\n\ndef own_table():\n    return TABLE\n\n\ndef lookup(key):\n    return None\n",
            ),
            (
                "kit/noise.py",
                "class Holder:\n    TABLE = 1\n\n\ndef attribute(h: Holder):\n    return h.TABLE\n\n\ndef says():\n    # TABLE in a comment, PLAIN too\n    return \"TABLE PLAIN\"\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "kit/consts.py:TABLE");
    assert_eq!(
        callers(&doc, "kit/consts.py", "TABLE"),
        set(&[
            "kit/alias.py::via_alias",
            "kit/consts.py::use",
            "kit/reader.py::read",
        ]),
        "{doc}"
    );
    let doc = symbol(tmp.path(), "kit/consts.py:PLAIN");
    assert_eq!(
        callers(&doc, "kit/consts.py", "PLAIN"),
        set(&["kit/consts.py::use", "kit/qualified.py::through_module"]),
        "{doc}"
    );
    // Control: a function, which the base already models.
    let doc = symbol(tmp.path(), "kit/consts.py:lookup");
    assert_eq!(
        callers(&doc, "kit/consts.py", "lookup"),
        set(&["kit/reader.py::find"]),
        "{doc}"
    );
}

/// #398: a changed module-level statement is a changed definition, and a
/// test importing it reaches it. A test of another module's same-named
/// variable, and one that only spells the name in a string, do not.
#[test]
fn a_module_level_change_is_reached_by_the_tests_importing_it() {
    let server = |name: &str| {
        format!(
            "from app.kit import StrictServer\n\nmcp = StrictServer(\"{name}\")\n\n\ndef tool():\n    return mcp\n"
        )
    };
    let tmp = repo(
        &[
            ("app/__init__.py", ""),
            ("app/server.py", &server("a")),
            (
                "app/kit.py",
                "class StrictServer:\n    def __init__(self, name):\n        self.name = name\n",
            ),
            ("app/other.py", "mcp = None\n"),
            (
                "tests/test_server.py",
                "from app.server import mcp\n\n\ndef test_name():\n    assert mcp.name\n",
            ),
            (
                "tests/test_other.py",
                "from app.other import mcp\n\n\ndef test_other():\n    assert mcp is None\n",
            ),
            (
                "tests/test_words.py",
                "def test_words():\n    # mcp\n    assert \"mcp\"\n",
            ),
        ],
        &[("app/server.py", &server("b"))],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "app/server.py", "mcp"),
        set(&["app/server.py::tool", "tests/test_server.py::test_name"]),
        "{doc}"
    );
    let mcp = changed(&doc, "app/server.py", "mcp");
    assert_eq!(mcp["untested"], serde_json::Value::Null, "{doc}");
    assert_eq!(
        doc["untested_definitions"],
        serde_json::Value::Null,
        "{doc}"
    );
}

/// #488 (Python): a class-level field is a definition; reads and writes
/// through a receiver typed as its class are its uses. The same-named field
/// of another class, a dict key spelling it, a comment, and a read through
/// an untyped receiver (named as unresolved) are not listed.
#[test]
fn a_class_field_is_defined_and_its_readers_and_writers_listed() {
    let tmp = repo(
        &[
            ("app/__init__.py", ""),
            (
                "app/models.py",
                "from datetime import datetime\n\n\nclass Asset:\n    \"\"\"A held asset.\"\"\"\n\n    name: str = \"\"\n    valued_at: datetime | None = None\n\n    def stamp(self, when):\n        self.valued_at = when\n\n\nclass Other:\n    valued_at = None\n",
            ),
            (
                "app/api.py",
                "from app.models import Asset, Other\n\n\ndef patch(when):\n    a = Asset()\n    a.valued_at = when\n    return a\n\n\ndef show(a: Asset):\n    return a.valued_at\n\n\ndef peek(o: Other):\n    return o.valued_at\n\n\ndef keyed(updates, when):\n    # valued_at is set by key\n    updates[\"valued_at\"] = when\n\n\ndef loose(x):\n    return x.valued_at\n\n\ndef restamp(a: Asset):\n    a.stamp(None)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Asset.valued_at");
    assert_eq!(
        callers(&doc, "app/models.py", "valued_at"),
        set(&[
            "app/api.py::patch",
            "app/api.py::show",
            "app/models.py::stamp",
        ]),
        "{doc}"
    );
    let field = changed(&doc, "app/models.py", "valued_at");
    assert!(
        field["unresolved"]
            .as_str()
            .is_some_and(|u| u.contains("1 read(s) of `.valued_at`")),
        "{doc}"
    );
    // Control: a method, which the base already models.
    let doc = symbol(tmp.path(), "Asset.stamp");
    assert_eq!(
        callers(&doc, "app/models.py", "stamp"),
        set(&["app/api.py::restamp"]),
        "{doc}"
    );
}

/// #477: a method called on what a repository function returns (a test
/// helper's `make()`, `helpers.make()`) is a possible caller, its receiver
/// unresolved: the syntax does not say what the function returns. The same
/// method name on an instance of another class, in a comment and in a
/// string stays no caller. Control: `auth_mod.check()` through an imported
/// module answers exactly.
#[test]
fn a_receiver_a_function_returns_is_a_possible_caller() {
    let tmp = repo(
        &[
            ("app/__init__.py", ""),
            (
                "app/client.py",
                "class Client:\n    def token(self):\n        return \"t\"\n",
            ),
            (
                "app/other.py",
                "class Other:\n    def token(self):\n        return \"o\"\n",
            ),
            (
                "app/auth_mod.py",
                "def check(token):\n    return token == \"ok\"\n",
            ),
            (
                "app/server.py",
                "from app import auth_mod\n\n\ndef handle(req):\n    return auth_mod.check(req)\n",
            ),
            ("tests/__init__.py", ""),
            (
                "tests/helpers.py",
                "from app.client import Client\n\n\ndef make():\n    return Client()\n",
            ),
            (
                "tests/test_api.py",
                "from tests import helpers\nfrom tests.helpers import make\nfrom app.other import Other\n\n\ndef test_token():\n    client = make()\n    assert client.token() == \"t\"\n\n\ndef test_qualified():\n    c = helpers.make()\n    assert c.token()\n\n\ndef test_other():\n    o = Other()\n    # client.token() in a comment\n    assert o.token() == \"o\" and \"client.token()\"\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Client.token");
    let token = changed(&doc, "app/client.py", "token");
    let listed: BTreeSet<String> = token["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{} {} {}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or(""),
                c["evidence"].as_str().unwrap_or(""),
                c["reason"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        listed,
        set(&[
            "tests/test_api.py::test_qualified candidate receiver_unresolved",
            "tests/test_api.py::test_token candidate receiver_unresolved",
        ]),
        "{doc}"
    );
    // Control: a function reached through its module.
    let doc = symbol(tmp.path(), "app/auth_mod.py:check");
    assert_eq!(
        callers(&doc, "app/auth_mod.py", "check"),
        set(&["app/server.py::handle"]),
        "{doc}"
    );
}

/// #525: a test that puts a directory on `sys.path` — itself, or through a
/// rig it imports first — imports that directory's module. Another
/// directory's module of the same name, and a test spelling the call in a
/// comment and a string, stay unlinked. Control: a package import of the
/// other module answers exactly.
#[test]
fn a_test_importing_through_sys_path_reaches_the_module() {
    let jobs = |inc: u32| {
        format!("def step(x):\n    return x + {inc}\n\n\ndef run(x):\n    return step(x)\n")
    };
    let tmp = repo(
        &[
            ("transcribe/jobs.py", &jobs(1)),
            ("other/__init__.py", ""),
            ("other/jobs.py", &jobs(1)),
            (
                "tests/rig.py",
                "import sys\nfrom pathlib import Path\n\nREPO = Path(__file__).resolve().parent.parent\nsys.path.insert(0, str(REPO / \"transcribe\"))\n",
            ),
            (
                "tests/test_crash.py",
                "import rig\nimport jobs\n\n\ndef test_run():\n    assert jobs.run(1) == 2\n",
            ),
            (
                "tests/test_direct.py",
                "import os\nimport sys\n\nsys.path.insert(0, os.path.join(os.path.dirname(__file__), \"..\", \"transcribe\"))\n\nfrom jobs import step\n\n\ndef test_step():\n    assert step(1) == 2\n",
            ),
            (
                "tests/test_words.py",
                "def test_words():\n    # jobs.run(1), step(1)\n    assert \"jobs.run step\"\n",
            ),
            (
                "tests/test_other.py",
                "from other import jobs as other_jobs\n\n\ndef test_other():\n    assert other_jobs.step(1) == 2\n",
            ),
        ],
        &[("transcribe/jobs.py", &jobs(2))],
    );
    let doc = impact(tmp.path());
    let step = changed(&doc, "transcribe/jobs.py", "step");
    let listed: BTreeSet<String> = step["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{} {} {}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or(""),
                c["evidence"].as_str().unwrap_or(""),
                c["tested_by"].as_str().unwrap_or("-")
            )
        })
        .collect();
    assert_eq!(
        listed,
        set(&[
            "tests/test_direct.py::test_step resolved tests/test_direct.py",
            "transcribe/jobs.py::run resolved tests/test_crash.py",
        ]),
        "{doc}"
    );
    assert_eq!(step["untested"], serde_json::Value::Null, "{doc}");
    // Control: the other module, imported as a package.
    let doc = symbol(tmp.path(), "other/jobs.py:step");
    assert_eq!(
        callers(&doc, "other/jobs.py", "step"),
        set(&["other/jobs.py::run", "tests/test_other.py::test_other"]),
        "{doc}"
    );
}

/// #426 (Python): `self.x = …` in a method declares the attribute `x` of
/// its class; reads through `self` and writes through a constructed
/// instance are its uses, and changing the line keeps the constructor's
/// callers. The same-named attribute of another class, the name in a
/// comment and a string are not uses; a read through an untyped receiver
/// is named as unresolved. Control: the method `enter` answers exactly.
#[test]
fn an_attribute_a_method_sets_is_defined_and_used() {
    let engine = |t: u32| {
        format!(
            "class Engine:\n    def __init__(self, opts):\n        self.load_timeout = opts.get(\"t\", {t})\n\n    def enter(self, url):\n        return (url, self.load_timeout)\n"
        )
    };
    let tmp = repo(
        &[
            ("app/__init__.py", ""),
            ("app/engine.py", &engine(10)),
            (
                "app/store.py",
                "from app.engine import Engine\n\n\ndef start(url):\n    e = Engine({})\n    e.load_timeout = 5\n    return e.enter(url)\n\n\ndef peek(o):\n    return o.load_timeout\n",
            ),
            (
                "app/other.py",
                "class Other:\n    def __init__(self):\n        self.load_timeout = 3\n\n\ndef other_peek(o: Other):\n    return o.load_timeout\n\n\ndef words():\n    # load_timeout in a comment\n    return \"load_timeout\"\n",
            ),
        ],
        &[("app/engine.py", &engine(20))],
    );
    let doc = symbol(tmp.path(), "Engine.load_timeout");
    assert_eq!(
        callers(&doc, "app/engine.py", "load_timeout"),
        set(&["app/engine.py::enter", "app/store.py::start"]),
        "{doc}"
    );
    assert!(
        changed(&doc, "app/engine.py", "load_timeout")["unresolved"]
            .as_str()
            .is_some_and(|u| u.contains("1 read(s) of `.load_timeout`")),
        "{doc}"
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "app/engine.py", "load_timeout"),
        set(&["app/engine.py::enter", "app/store.py::start"]),
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "app/engine.py", "Engine"),
        set(&["app/store.py::start"]),
        "{doc}"
    );
    // Control: a method, which the base already models.
    let doc = symbol(tmp.path(), "Engine.enter");
    assert_eq!(
        callers(&doc, "app/engine.py", "enter"),
        set(&["app/store.py::start"]),
        "{doc}"
    );
}

/// `path::symbol evidence reason` of every caller listed for `path::name`.
fn evidenced(doc: &serde_json::Value, path: &str, name: &str) -> BTreeSet<String> {
    changed(doc, path, name)["callers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{} {} {}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or(""),
                c["evidence"].as_str().unwrap_or(""),
                c["reason"].as_str().unwrap_or("-")
            )
        })
        .collect()
}

/// #578: a method only the framework calls (`do_GET` of a
/// `BaseHTTPRequestHandler`) names that route, and a test that hands its
/// class to the server reaches it through the class. The same handler in a
/// module no test reaches keeps "no static test link", a test spelling its
/// class in a string and a comment links nothing, and a plain class's
/// method keeps its zero. Control: `serve`'s caller answers exactly.
#[test]
fn a_method_the_framework_dispatches_is_reached_through_its_class() {
    let handler = |status: u32| {
        format!(
            "from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer\n\n\nclass Handler(BaseHTTPRequestHandler):\n    def do_GET(self):\n        self.send_response({status})\n        self.end_headers()\n\n\ndef serve(port):\n    return ThreadingHTTPServer((\"127.0.0.1\", port), Handler)\n"
        )
    };
    let quiet = |status: u32| {
        format!(
            "from http.server import BaseHTTPRequestHandler\n\n\nclass Quiet(BaseHTTPRequestHandler):\n    def do_GET(self):\n        self.send_response({status})\n"
        )
    };
    let plain = |n: u32| format!("class Plain:\n    def tally(self):\n        return {n}\n");
    let tmp = repo(
        &[
            ("server.py", &handler(200)),
            ("quiet.py", &quiet(200)),
            ("plain.py", &plain(1)),
            (
                "tests/test_server.py",
                "import http.client\nimport threading\n\nimport server\n\n\ndef test_get():\n    srv = server.serve(0)\n    threading.Thread(target=srv.serve_forever, daemon=True).start()\n    conn = http.client.HTTPConnection(\"127.0.0.1\", srv.server_address[1])\n    conn.request(\"GET\", \"/\")\n    assert conn.getresponse().status == 200\n",
            ),
            (
                "tests/test_words.py",
                "def test_words():\n    # Quiet().do_GET(), Plain().tally()\n    assert \"Quiet().do_GET() Plain().tally()\"\n",
            ),
        ],
        &[
            ("server.py", &handler(201)),
            ("quiet.py", &quiet(201)),
            ("plain.py", &plain(2)),
        ],
    );
    let doc = impact(tmp.path());
    let reached = changed(&doc, "server.py", "do_GET");
    assert_eq!(reached["untested"], serde_json::Value::Null, "{doc}");
    assert!(
        reached["unresolved"]
            .as_str()
            .is_some_and(|u| u.contains("`BaseHTTPRequestHandler`")),
        "{doc}"
    );
    let unreached = changed(&doc, "quiet.py", "do_GET");
    assert_eq!(unreached["untested"], true, "{doc}");
    assert!(
        unreached["unresolved"]
            .as_str()
            .is_some_and(|u| u.contains("`BaseHTTPRequestHandler`")),
        "{doc}"
    );
    assert!(
        doc["changed"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|c| c["path"] != "plain.py"),
        "{doc}"
    );
    assert_eq!(
        doc["untested_unlisted"],
        serde_json::json!(["plain.py::tally (2-3)"]),
        "{doc}"
    );
    // Control: a function, which the base already models.
    let doc = symbol(tmp.path(), "server.py:serve");
    assert_eq!(
        callers(&doc, "server.py", "serve"),
        set(&["tests/test_server.py::test_get"]),
        "{doc}"
    );
}

/// #553, #573: pytest puts its configured `pythonpath` (pyproject, or
/// `setup.cfg`'s `[tool:pytest]`) and whatever a `conftest.py` above a test
/// inserts on `sys.path`, so a test's `import metrics` is `src/metrics.py`.
/// A same-named module elsewhere, a test outside that configuration, and a
/// test spelling the call in a string and a comment stay off. Control: a
/// package import answers exactly.
#[test]
fn a_test_importing_through_pytest_configuration_reaches_the_module() {
    let module = |out: &str| format!("def render_metrics():\n    return \"{out}\"\n");
    let tmp = repo(
        &[
            (
                "svc/pyproject.toml",
                "[project]\nname = \"svc\"\nversion = \"0.1.0\"\n\n[tool.pytest.ini_options]\npythonpath = [\"src\"]\n",
            ),
            ("svc/src/metrics.py", &module("m")),
            (
                "svc/tests/test_metrics.py",
                "import metrics\n\n\ndef test_render():\n    assert metrics.render_metrics() == \"m\"\n",
            ),
            (
                "svc/tests/test_words.py",
                "def test_words():\n    # metrics.render_metrics()\n    assert \"metrics.render_metrics()\"\n",
            ),
            ("tools/metrics.py", &module("decoy")),
            (
                "tools/tests/test_tools.py",
                "import metrics\n\n\ndef test_tools():\n    assert metrics.render_metrics()\n",
            ),
            (
                "job/setup.cfg",
                "[metadata]\nname = job\n\n[tool:pytest]\npythonpath =\n    lib\n    .\n",
            ),
            ("job/lib/report.py", "def render_report():\n    return 1\n"),
            ("other/report.py", "def render_report():\n    return 2\n"),
            (
                "job/tests/test_report.py",
                "import report\n\n\ndef test_report():\n    assert report.render_report() == 1\n",
            ),
            (
                "child-a/transcribe/transcribe.py",
                "def recognise_track(path):\n    return path\n",
            ),
            ("child-b/src/pkg/__init__.py", ""),
            (
                "child-b/src/pkg/transcribe.py",
                "def recognise_track(path):\n    return None\n",
            ),
            (
                "child-a/tests/conftest.py",
                "import sys\nfrom pathlib import Path\n\nROOT = Path(__file__).resolve().parents[1]\nif str(ROOT / \"transcribe\") not in sys.path:\n    sys.path.insert(0, str(ROOT / \"transcribe\"))\n",
            ),
            (
                "child-a/tests/test_languages.py",
                "import transcribe\n\n\ndef test_monologue():\n    assert transcribe.recognise_track(\"a\") == \"a\"\n",
            ),
            ("pkgs/__init__.py", ""),
            ("pkgs/util.py", "def helper():\n    return 1\n"),
            (
                "tests/test_util.py",
                "from pkgs.util import helper\n\n\ndef test_helper():\n    assert helper() == 1\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "svc/src/metrics.py:render_metrics");
    assert_eq!(
        evidenced(&doc, "svc/src/metrics.py", "render_metrics"),
        set(&["svc/tests/test_metrics.py::test_render resolved -"]),
        "{doc}"
    );
    let doc = symbol(tmp.path(), "tools/metrics.py:render_metrics");
    assert_eq!(
        evidenced(&doc, "tools/metrics.py", "render_metrics"),
        set(&["tools/tests/test_tools.py::test_tools resolved -"]),
        "{doc}"
    );
    let doc = symbol(tmp.path(), "job/lib/report.py:render_report");
    assert_eq!(
        evidenced(&doc, "job/lib/report.py", "render_report"),
        set(&["job/tests/test_report.py::test_report resolved -"]),
        "{doc}"
    );
    let doc = symbol(
        tmp.path(),
        "child-a/transcribe/transcribe.py:recognise_track",
    );
    assert_eq!(
        evidenced(&doc, "child-a/transcribe/transcribe.py", "recognise_track"),
        set(&["child-a/tests/test_languages.py::test_monologue resolved -"]),
        "{doc}"
    );
    // Control: a package import, which the base already models.
    let doc = symbol(tmp.path(), "pkgs/util.py:helper");
    assert_eq!(
        callers(&doc, "pkgs/util.py", "helper"),
        set(&["tests/test_util.py::test_helper"]),
        "{doc}"
    );
}

/// #557: a uv path or workspace package re-exporting a function from its
/// `__init__` passes it on: `from pkg import client_ip` and `pkg.client_ip`
/// in a consumer whose `[tool.uv.sources]` names the package are callers,
/// though another module named `pkg` exists. A consumer without the source
/// is a possible caller (the module is ambiguous), never none; another
/// project's own `client_ip` keeps its callers, and a string, a comment and
/// the lines of a parenthesised import stay off. Control: the package's own
/// caller answers exactly.
#[test]
fn a_uv_package_reexporting_from_its_init_passes_the_name_on() {
    let consumer = |name: &str, sources: &str| {
        format!(
            "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [\"pkg\"]\n{sources}"
        )
    };
    let tmp = repo(
        &[
            (
                "pyproject.toml",
                "[tool.uv.workspace]\nmembers = [\"apps/*\", \"shared/*\"]\n",
            ),
            (
                "shared/pkg/pyproject.toml",
                "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
            ),
            (
                "shared/pkg/src/pkg/__init__.py",
                "from pkg.ip import client_ip\n\n__all__ = [\"client_ip\"]\n",
            ),
            (
                "shared/pkg/src/pkg/ip.py",
                "def client_ip(headers, peer):\n    return headers.get(\"x-forwarded-for\", peer)\n",
            ),
            (
                "shared/pkg/src/pkg/request_log.py",
                "from pkg.ip import client_ip\n\n\ndef answer_fields(request):\n    return {\"ip\": client_ip(request.headers, request.peer)}\n",
            ),
            (
                "apps/svc/pyproject.toml",
                &consumer(
                    "svc",
                    "\n[tool.uv.sources]\npkg = { path = \"../../shared/pkg\" }\n",
                ),
            ),
            ("apps/svc/src/svc/__init__.py", ""),
            (
                "apps/svc/src/svc/api.py",
                "from pkg import client_ip\n\n\ndef handle(request):\n    # client_ip(request) in a comment\n    return client_ip(request.headers, request.client.host)\n",
            ),
            (
                "apps/svc/src/svc/qual.py",
                "import pkg\n\n\ndef qualified(request):\n    return pkg.client_ip(request.headers, None)\n\n\ndef says():\n    return \"pkg.client_ip(headers)\"\n",
            ),
            (
                "apps/svc/src/svc/multi.py",
                "from pkg import (\n    client_ip,\n)\n\n\ndef multi(request):\n    return client_ip(request.headers, None)\n",
            ),
            (
                "apps/ws/pyproject.toml",
                &consumer("ws", "\n[tool.uv.sources]\npkg = { workspace = true }\n"),
            ),
            ("apps/ws/src/ws/__init__.py", ""),
            (
                "apps/ws/src/ws/audit.py",
                "from pkg import client_ip\n\n\ndef record(request):\n    return client_ip(request.headers, None)\n",
            ),
            ("apps/loose/pyproject.toml", &consumer("loose", "")),
            ("apps/loose/src/loose/__init__.py", ""),
            (
                "apps/loose/src/loose/log.py",
                "from pkg import client_ip\n\n\ndef stamp(request):\n    return client_ip(request.headers, None)\n",
            ),
            (
                "apps/other/pyproject.toml",
                "[project]\nname = \"other\"\nversion = \"0.1.0\"\n",
            ),
            ("apps/other/src/other/__init__.py", ""),
            (
                "apps/other/src/other/pkg.py",
                "def client_ip(headers, peer):\n    return peer\n\n\ndef own(request):\n    return client_ip(request.headers, None)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "shared/pkg/src/pkg/ip.py:client_ip");
    assert_eq!(
        evidenced(&doc, "shared/pkg/src/pkg/ip.py", "client_ip"),
        set(&[
            "apps/loose/src/loose/log.py::stamp candidate ambiguous_module",
            "apps/svc/src/svc/api.py::handle resolved -",
            "apps/svc/src/svc/multi.py::multi resolved -",
            "apps/svc/src/svc/qual.py::qualified resolved -",
            "apps/ws/src/ws/audit.py::record resolved -",
            "shared/pkg/src/pkg/request_log.py::answer_fields resolved -",
        ]),
        "{doc}"
    );
    let doc = symbol(tmp.path(), "apps/other/src/other/pkg.py:client_ip");
    assert_eq!(
        evidenced(&doc, "apps/other/src/other/pkg.py", "client_ip"),
        set(&[
            "apps/loose/src/loose/log.py::stamp candidate ambiguous_module",
            "apps/other/src/other/pkg.py::own resolved -",
        ]),
        "{doc}"
    );
}

/// #546: a function defined inside another is named only there, so a file
/// over the size cap that spells its name is no possible caller of it. A
/// module-level function of the same commit, which such a file could call,
/// keeps the hedge.
#[test]
fn a_nested_function_has_no_callers_outside_its_function() {
    let mut big: String = (0..4000)
        .map(|i| format!("def big_{i}(value):\n    return value + {i}\n\n"))
        .collect();
    big.push_str("from lib.policy import cache_control\n\n\ndef use(h):\n    return cache_control(h, \"/\")\n");
    let policy = |unit: &str, stripped: &str| {
        format!(
            "def cache_control(status, uri):\n    return f\"{{status}} {{uri}}{unit}\"\n\n\ndef value_of(header):\n    def value(variable):\n        for part in header.split(\",\"):\n            if part.strip().startswith(variable):\n                return part{stripped}\n        return \"\"\n\n    return value(\"max-age\")\n"
        )
    };
    let tmp = repo(
        &[
            ("child/src/pkg/__init__.py", ""),
            ("child/src/pkg/domain.py", &big),
            ("lib/__init__.py", ""),
            ("lib/policy.py", &policy("", "")),
        ],
        &[("lib/policy.py", &policy("s", ".strip()"))],
    );
    let doc = impact(tmp.path());
    let listed: BTreeSet<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(listed, set(&["lib/policy.py::cache_control"]), "{doc}");
    assert_eq!(
        changed(&doc, "lib/policy.py", "cache_control")["missed_in"],
        serde_json::json!([{"path": "child/src/pkg/domain.py", "language": "python", "reason": "too_large"}]),
        "{doc}"
    );
}

/// #536: a deleted function is read off the syntax at the base, and only a
/// use the binding model resolves to it is a caller: a parameter of the
/// same name, a docstring word, another module's own function of that name
/// and `json.load` are not. A deleted function still imported and called elsewhere
/// lists that caller.
#[test]
fn a_deleted_function_is_not_called_by_a_parameter_or_a_docstring() {
    let tail = "def align(reference, utterances):\n    \"\"\"Pair each utterance with its regions.\"\"\"\n    out = []\n    for u in utterances:\n        out.append((reference, u))\n    return out\n";
    let head = "\"\"\"Evaluation helpers.\n\nAligns reference utterances against detected\nregions of speech, loaded by load.\n\"\"\"\n\nimport json\n\n\n";
    let removed = "def utterances(run_dir):\n    return json.load(open(run_dir))\n\n\ndef regions(session):\n    return session[\"regions\"]\n\n\ndef load(path):\n    return json.load(open(path))\n\n\n";
    let tmp = repo(
        &[
            ("pkg/__init__.py", ""),
            ("pkg/eval.py", &format!("{head}{removed}{tail}")),
            (
                "pkg/report.py",
                "from pkg.eval import load\n\n\ndef report(path):\n    return load(path)\n",
            ),
            (
                "pkg/other.py",
                "import json\n\n\ndef load(path):\n    return path\n\n\ndef use():\n    return load(\"x\")\n\n\ndef raw(f):\n    return json.load(f)\n",
            ),
            (
                "tests/test_eval.py",
                "from pkg.eval import align\n\n\ndef test_align():\n    assert align(\"r\", [\"a\"]) == [(\"r\", \"a\")]\n",
            ),
        ],
        &[("pkg/eval.py", &format!("{head}{tail}"))],
    );
    let doc = impact(tmp.path());
    let listed: BTreeSet<String> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}::{}",
                c["path"].as_str().unwrap_or(""),
                c["symbol"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(listed, set(&["pkg/eval.py::load"]), "{doc}");
    assert_eq!(
        evidenced(&doc, "pkg/eval.py", "load"),
        set(&["pkg/report.py::report resolved -"]),
        "{doc}"
    );
}

/// #400: a field removed from a class is read off the syntax at the base,
/// and a read through the instance its module builds (`from svc.config
/// import settings`, `settings = AppSettings()`) is a caller of it. A read
/// of a field the class keeps, the same field name on another class's
/// instance, and the name in a string and a comment are not. Control: the
/// class keeps its constructor callers.
#[test]
fn a_removed_field_is_read_through_the_instance_its_module_builds() {
    let config = |field: &str| {
        format!(
            "from pydantic_settings import BaseSettings\n\n\nclass AppSettings(BaseSettings):\n    alpha: str = \"\"\n{field}    charlie: bool = False\n\n\nsettings = AppSettings()\n"
        )
    };
    let tmp = repo(
        &[
            ("svc/__init__.py", ""),
            ("svc/config.py", &config("    bravo: str = \"\"\n")),
            (
                "svc/app.py",
                "from svc.config import settings\n\n\ndef origin():\n    return settings.alpha\n",
            ),
            (
                "svc/job.py",
                "from svc.config import settings\n\n\ndef run():\n    return settings.bravo\n",
            ),
            (
                "svc/other.py",
                "class Other:\n    bravo = 1\n\n\nother = Other()\n",
            ),
            (
                "svc/peer.py",
                "from svc.other import other\n\n\ndef peer():\n    # settings.bravo\n    return other.bravo, \"settings.bravo\"\n",
            ),
            (
                "svc/factory.py",
                "from svc.config import AppSettings\n\n\ndef build():\n    return AppSettings(alpha=\"x\")\n",
            ),
        ],
        &[("svc/config.py", &config(""))],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        evidenced(&doc, "svc/config.py", "bravo"),
        set(&["svc/job.py::run resolved -"]),
        "{doc}"
    );
    assert_eq!(
        callers(&doc, "svc/config.py", "AppSettings"),
        set(&["svc/config.py::settings", "svc/factory.py::build"]),
        "{doc}"
    );
}
/// #421, closed on its own shape: a method whose last line calls a
/// same-class method, past a `with` block, is that method's caller.
#[test]
fn a_same_class_call_on_the_last_line_after_a_with_block_is_a_caller() {
    let tmp = repo(
        &[
            ("svc/__init__.py", ""),
            (
                "svc/service.py",
                "class Service:\n    def __init__(self, s):\n        self.s = s\n\n    def apply_result(self, x):\n        return x\n\n    def commit(self, x):\n        with self.s.tx():\n            y = x + 1\n        return self.apply_result(y)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "apply_result");
    assert_eq!(
        callers(&doc, "svc/service.py", "apply_result"),
        set(&["svc/service.py::commit"]),
        "{doc}"
    );
}

/// #467, closed on its own shape: apps that reach a shared package through
/// `[tool.uv.sources]` path entries, importing a name its `__init__`
/// re-exports, are callers of what the change edits there.
#[test]
fn apps_linked_by_uv_path_sources_are_callers_in_the_shared_package() {
    let manifest = |name: &str| {
        format!(
            "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [\"kit\"]\n\n[tool.uv.sources]\nkit = {{ path = \"../../shared/kit\" }}\n"
        )
    };
    let serve = "from kit import auth_settings\n\n\ndef serve():\n    return auth_settings()\n";
    let auth = "def auth_settings():\n    return {\"ttl\": 60}\n";
    let (a, b) = (manifest("a"), manifest("b"));
    let tmp = repo(
        &[
            (
                "shared/kit/pyproject.toml",
                "[project]\nname = \"kit\"\nversion = \"0.1.0\"\n",
            ),
            (
                "shared/kit/src/kit/__init__.py",
                "from kit.auth import auth_settings\n",
            ),
            ("shared/kit/src/kit/auth.py", auth),
            ("apps/a/pyproject.toml", &a),
            ("apps/a/src/a/__init__.py", ""),
            ("apps/a/src/a/serve.py", serve),
            ("apps/b/pyproject.toml", &b),
            ("apps/b/src/b/__init__.py", ""),
            ("apps/b/src/b/serve.py", serve),
        ],
        &[("shared/kit/src/kit/auth.py", &auth.replace("60", "120"))],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "shared/kit/src/kit/auth.py", "auth_settings"),
        set(&[
            "apps/a/src/a/serve.py::serve",
            "apps/b/src/b/serve.py::serve"
        ]),
        "{doc}"
    );
}

/// #468, closed on its own shape: a file that imports names and calls them
/// defines none of them, even when the change edits its import line; the
/// view around the calls is what changed, and its caller says so.
#[test]
fn an_imported_name_is_no_changed_definition_of_the_importer() {
    let tmp = repo(
        &[
            ("lib/__init__.py", ""),
            (
                "lib/patching.py",
                "def apply_patch(p):\n    return p\n\n\ndef revert(p):\n    return p\n",
            ),
            ("api/__init__.py", ""),
            (
                "api/issues.py",
                "from flask import Blueprint\n\nfrom lib.patching import apply_patch\n\nbp = Blueprint(\"issues\", __name__)\n\n\n@bp.route(\"/patch\")\ndef view(p):\n    return apply_patch(p)\n",
            ),
            (
                "api/app.py",
                "from api.issues import view\n\n\ndef handle(p):\n    return view(p)\n",
            ),
        ],
        &[(
            "api/issues.py",
            "from flask import Blueprint\n\nfrom lib.patching import apply_patch, revert\n\nbp = Blueprint(\"issues\", __name__)\n\n\n@bp.route(\"/patch\")\ndef view(p):\n    return revert(apply_patch(p))\n",
        )],
    );
    let doc = impact(tmp.path());
    let defined: BTreeSet<&str> = doc["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["path"] == "api/issues.py")
        .filter_map(|c| c["symbol"].as_str())
        .collect();
    assert_eq!(
        callers(&doc, "api/issues.py", "view"),
        set(&["api/app.py::handle"]),
        "{doc}"
    );
    assert!(
        !defined.contains("apply_patch") && !defined.contains("revert"),
        "{defined:?}: {doc}"
    );
}

/// A removed module-level constant breaks every importer that still reads
/// it; the removal read as a clean zero because only functions and classes
/// counted as declarations (found with #491). Another module's own constant
/// of that name, a class attribute and the name in a string are no readers.
#[test]
fn a_removed_module_constant_names_the_readers_it_breaks() {
    let tmp = repo(
        &[
            ("pkg/__init__.py", ""),
            (
                "pkg/settings.py",
                "MAX_RETRIES = 3\n\n\ndef keep():\n    return 1\n",
            ),
            (
                "pkg/client.py",
                "from pkg.settings import MAX_RETRIES\n\n\ndef retry():\n    return MAX_RETRIES\n",
            ),
            (
                "pkg/worker.py",
                "from pkg import settings\n\n\ndef work():\n    return settings.MAX_RETRIES\n",
            ),
            (
                "pkg/other.py",
                "MAX_RETRIES = 9\n\n\nclass Policy:\n    MAX_RETRIES = 1\n\n\ndef own():\n    return MAX_RETRIES, \"MAX_RETRIES\"\n",
            ),
        ],
        &[("pkg/settings.py", "def keep():\n    return 1\n")],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "pkg/settings.py", "MAX_RETRIES"),
        set(&["pkg/client.py::retry", "pkg/worker.py::work"]),
        "{doc}"
    );
}
