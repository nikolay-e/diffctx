// Python definitions, callers and test reach read off the syntax (#506):
// each gate pairs the true sites with look-alikes that must not become one
// (a same-named attribute of another class, the name in a string and in a
// comment, a same-named definition in another module) and a control the
// base already models, answered exactly.

use std::collections::BTreeSet;
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
    let out = Command::new(&*BIN)
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
