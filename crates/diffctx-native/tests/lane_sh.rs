// Shell and SQL callers (#506, shell+SQL lane): each gate pairs the true
// answer with look-alikes that must not join it — a same-named function of
// a script nobody sources, the name in an `echo` argument, a string or a
// comment, a same-named function of another schema, a table named like the
// routine — and a control that has to come back exactly.

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

/// A repository with one commit per step.
fn repo(steps: &[&[(&str, &str)]]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    for (step, files) in steps.iter().enumerate() {
        for (rel, content) in *files {
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
        .args(["-q", "-f", "json", "--timeout", "600"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("impact JSON")
}

fn impact(repo: &Path) -> serde_json::Value {
    run(repo, &["--diff", "HEAD~1..HEAD", "--mode", "impact"])
}

fn symbol(repo: &Path, name: &str) -> serde_json::Value {
    run(repo, &[".", "--symbol", name])
}

fn changed<'a>(out: &'a serde_json::Value, path: &str, name: &str) -> &'a serde_json::Value {
    out["changed"]
        .as_array()
        .expect("changed")
        .iter()
        .find(|c| c["path"] == path && c["symbol"] == name)
        .unwrap_or_else(|| panic!("{path}::{name} not in {out:#}"))
}

/// The callers listed under `path::name`: `path::symbol`, or `path@sites`
/// for one without a name, `possibly … (reason)` for a candidate.
fn callers(out: &serde_json::Value, path: &str, name: &str) -> BTreeSet<String> {
    changed(out, path, name)["callers"]
        .as_array()
        .expect("callers")
        .iter()
        .map(|c| {
            let file = c["path"].as_str().unwrap();
            let at = match c["symbol"].as_str() {
                Some(s) => format!("{file}::{s}"),
                None => {
                    let sites: Vec<String> = c["sites"]
                        .as_array()
                        .map(|s| s.iter().map(ToString::to_string).collect())
                        .unwrap_or_default();
                    format!("{file}@{}", sites.join(","))
                }
            };
            match c["evidence"].as_str() {
                Some("candidate") => format!("possibly {at} ({})", c["reason"].as_str().unwrap()),
                _ => at,
            }
        })
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

const LIB: &str = "fetch() {\n  curl -s \"$1\"\n}\n";
const MY_LIB: &str = "login() {\n  echo token\n}\n";

/// The libraries and every way the issue spells sourcing them, with the
/// look-alikes beside them.
fn sourcing_scripts() -> Vec<(&'static str, &'static str)> {
    vec![
        ("scripts/lib.sh", LIB),
        ("scripts/my-lib.sh", MY_LIB),
        ("scripts/plain.sh", ". ./lib.sh\nfetch x\n"),
        (
            "scripts/dyn.sh",
            "source \"$(dirname \"$0\")/lib.sh\"\nfetch y\n",
        ),
        (
            "scripts/run.sh",
            "source \"$(dirname \"${BASH_SOURCE[0]}\")/lib.sh\"\nsource \"$(dirname \"${BASH_SOURCE[0]}\")/my-lib.sh\"\nmain() {\n  fetch \"https://example.org\"\n}\nlogin\nmain \"$@\"\n",
        ),
        ("scripts/plain2.sh", ". ./my-lib.sh\nlogin\n"),
        (
            "scripts/var.sh",
            "#!/bin/bash\nSCRIPT_DIR=\"$(cd \"$(dirname \"${BASH_SOURCE[0]}\")\" && pwd)\"\nsource \"$SCRIPT_DIR/my-lib.sh\"\nif login; then echo ok; fi\n",
        ),
        (
            "scripts/env.sh",
            "source \"$LIB_HOME/my-lib.sh\"\ntoken=$(login)\n",
        ),
        // Decoys: its own `fetch`, a `fetch` command nothing defines, and
        // the names as words, in a comment and in strings.
        ("scripts/other.sh", "fetch() {\n  wget \"$1\"\n}\nfetch z\n"),
        ("scripts/standalone.sh", "fetch q\n"),
        (
            "scripts/words.sh",
            ". ./lib.sh\n. ./my-lib.sh\necho fetch login\n# fetch it, then login\nprintf '%s\\n' \"fetch\"\nmsg=\"login\"\n",
        ),
        // Sources something no file name says: a call may reach any library.
        ("scripts/config.sh", "source \"$CONFIG_FILE\"\nlogin\n"),
        (
            "tests/test_lib.sh",
            "source \"$(dirname \"$0\")/../scripts/lib.sh\"\nfetch t\n",
        ),
    ]
}

/// #533: a library sourced through `$(dirname "$0")`, `BASH_SOURCE`, a
/// variable the script sets from either, a variable nothing sets, or a
/// literal path to a hyphenated name had no callers; every call there is
/// one, at top level, in a function or in a condition.
#[test]
fn a_sourced_library_binds_its_callers_however_the_path_is_spelled() {
    let files = sourcing_scripts();
    let tmp = repo(&[&files]);
    let fetch = symbol(tmp.path(), "fetch");
    assert_eq!(
        callers(&fetch, "scripts/lib.sh", "fetch"),
        set(&[
            "scripts/dyn.sh@2",
            "scripts/plain.sh@2",
            "scripts/run.sh::main",
            "tests/test_lib.sh@2",
        ]),
        "{fetch:#}"
    );
    assert_eq!(
        callers(&fetch, "scripts/other.sh", "fetch"),
        set(&["scripts/other.sh@4"]),
        "{fetch:#}"
    );
    let login = symbol(tmp.path(), "login");
    assert_eq!(
        callers(&login, "scripts/my-lib.sh", "login"),
        set(&[
            "possibly scripts/config.sh@2 (import_unresolved)",
            "scripts/env.sh@2",
            "scripts/plain2.sh@2",
            "scripts/run.sh@6",
            "scripts/var.sh@4",
        ]),
        "{login:#}"
    );
}

/// #533: a commit that changes only a library function reached nothing
/// outside the diff.
#[test]
fn a_changed_library_function_names_the_scripts_that_source_it() {
    let files = sourcing_scripts();
    let tmp = repo(&[
        &files,
        &[("scripts/my-lib.sh", "login() {\n  echo session\n}\n")],
    ]);
    let out = impact(tmp.path());
    assert_eq!(
        callers(&out, "scripts/my-lib.sh", "login"),
        set(&[
            "possibly scripts/config.sh@2 (import_unresolved)",
            "scripts/env.sh@2",
            "scripts/plain2.sh@2",
            "scripts/run.sh@6",
            "scripts/var.sh@4",
        ]),
        "{out:#}"
    );
}

/// #533: a file name two libraries carry, behind a directory nothing
/// resolves, is said to be ambiguous rather than bound or dropped; a path
/// relative to the script names one of them.
#[test]
fn a_library_name_two_files_carry_is_said_to_be_ambiguous() {
    let files = [
        ("a/util.sh", "helper() {\n  echo a\n}\n"),
        ("b/util.sh", "helper() {\n  echo b\n}\n"),
        ("c/use.sh", "source \"$ROOT/util.sh\"\nhelper\n"),
        (
            "d/pinned.sh",
            "source \"$(dirname \"$0\")/../a/util.sh\"\nhelper\n",
        ),
    ];
    let tmp = repo(&[&files]);
    let out = symbol(tmp.path(), "helper");
    assert_eq!(
        callers(&out, "a/util.sh", "helper"),
        set(&["d/pinned.sh@2", "possibly c/use.sh@2 (ambiguous_module)"]),
        "{out:#}"
    );
    assert_eq!(
        callers(&out, "b/util.sh", "helper"),
        set(&["possibly c/use.sh@2 (ambiguous_module)"]),
        "{out:#}"
    );
}

/// #533: a library calling a function of another library, which the
/// scripts that source it source beside it, calls it; a script nothing
/// sources calling the same name does not.
#[test]
fn a_library_calls_what_its_sourcing_script_sources_beside_it() {
    let files = [
        ("lib/log-lib.sh", "log() {\n  echo \"$*\" >&2\n}\n"),
        ("lib/util.sh", "tidy() {\n  log \"tidying\"\n}\n"),
        (
            "bin/deploy",
            "#!/bin/bash\nDIR=\"$(dirname \"$0\")/../lib\"\n. \"$DIR/log-lib.sh\"\n. \"$DIR/util.sh\"\ntidy\n",
        ),
        ("lib/orphan.sh", "sweep() {\n  log \"sweeping\"\n}\n"),
    ];
    let tmp = repo(&[&files]);
    let out = symbol(tmp.path(), "log");
    assert_eq!(
        callers(&out, "lib/log-lib.sh", "log"),
        set(&["lib/util.sh::tidy"]),
        "{out:#}"
    );
}

const DISPATCHER: &str = "#!/bin/bash\nfield() { python3 -c \"import json,sys; print(json.load(sys.stdin)[sys.argv[1]])\" \"$1\"; }\ncase \"${1:-}\" in\n  status) echo \"{}\" | field state ;;\n  restart) report ;;\nesac\nreport() {\n  echo ok\n}\n";

const DAEMON: &str = "#!/bin/bash\ndevices_ready() {\n  test -e /dev/null\n}\nfallback_output() {\n  echo \"$1\"\n}\nrestore_output() {\n  echo \"$1\"\n}\nprobe_loopback() {\n  true\n}\nlesson_check() {\n  echo lesson\n}\nunused_fn() {\n  echo never\n}\nmain() {\n  local ok=1\n  if ! devices_ready; then\n    exit 1\n  fi\n  until devices_ready; do sleep 1; done\n  target=\"$(fallback_output \"$chain\")\"\n  probe_loopback || ok=0\n  while true; do\n    if [ -e \"$marker\" ]; then\n      restore_output \"$chain\" && rm -f \"$marker\"\n    fi\n  done\n}\n# unused_fn is kept for later\necho unused_fn\nsudo unused_fn\ncase \"${1:-}\" in\n  lesson) time -p lesson_check; exit ;;\nesac\nmain \"$@\"\n";

/// #541: a call in a `case` arm, a pipeline, an `if`/`until` condition, a
/// command substitution, an `&&`/`||` list or after `time` is a caller, in
/// an extensionless script too; a name in a comment, an `echo` or after
/// `sudo` is not.
#[test]
fn calls_inside_compound_commands_are_callers() {
    let tmp = repo(&[
        &[("bin/call", DISPATCHER), ("bin/daemon.sh", DAEMON)],
        &[
            (
                "bin/call",
                &DISPATCHER
                    .replace("print(json", "print( json")
                    .replace("echo ok", "echo fine"),
            ),
            (
                "bin/daemon.sh",
                &DAEMON
                    .replace("test -e", "test -r")
                    .replace("echo \"$1\"\n}\nrestore", "printf '%s' \"$1\"\n}\nrestore")
                    .replace("echo never", "echo nothing"),
            ),
        ],
    ]);
    let out = impact(tmp.path());
    assert_eq!(
        callers(&out, "bin/call", "field"),
        set(&["bin/call@4"]),
        "{out:#}"
    );
    assert_eq!(
        callers(&out, "bin/call", "report"),
        set(&["bin/call@5"]),
        "{out:#}"
    );
    for name in ["devices_ready", "fallback_output"] {
        assert_eq!(
            callers(&out, "bin/daemon.sh", name),
            set(&["bin/daemon.sh::main"]),
            "{name}: {out:#}"
        );
    }
    assert!(
        out["changed"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["symbol"] != "unused_fn"),
        "{out:#}"
    );
    assert!(
        out["unanalysed_files"]
            .as_array()
            .is_none_or(|u| u.iter().all(|f| f["path"] != "bin/call")),
        "an extensionless script read whole is no unanalysed file: {out:#}"
    );
    let daemon = symbol(tmp.path(), "restore_output");
    assert_eq!(
        callers(&daemon, "bin/daemon.sh", "restore_output"),
        set(&["bin/daemon.sh::main"]),
        "{daemon:#}"
    );
    let probe = symbol(tmp.path(), "probe_loopback");
    assert_eq!(
        callers(&probe, "bin/daemon.sh", "probe_loopback"),
        set(&["bin/daemon.sh::main"]),
        "{probe:#}"
    );
    let lesson = symbol(tmp.path(), "lesson_check");
    assert_eq!(
        callers(&lesson, "bin/daemon.sh", "lesson_check"),
        set(&["bin/daemon.sh@38"]),
        "{lesson:#}"
    );
    let unused = symbol(tmp.path(), "unused_fn");
    assert_eq!(
        callers(&unused, "bin/daemon.sh", "unused_fn"),
        set(&[]),
        "{unused:#}"
    );
}

/// #521: tree-sitter-bash breaks on an escaped quote in `"${VAR:=…}"`, and
/// every name the script spelled became a hedge. The broken span is local:
/// a name in an `echo` argument after it was read; one inside it was not.
#[test]
fn a_broken_parse_hedges_only_the_names_it_could_not_read() {
    let tmp = repo(&[
        &[
            ("lib.sh", "helper() {\n  echo one\n}\n"),
            (
                "a.sh",
                "#!/bin/bash\n: \"${SEL:=input[type=\\\"text\\\"], input[type=\\\"email\\\"]}\"\necho \"$SEL\" helper\n",
            ),
            (
                "b.sh",
                "#!/bin/bash\n: \"${SEL:=input[type=text], input[type=email]}\"\necho \"$SEL\" helper\n",
            ),
            (
                "c.sh",
                "#!/bin/bash\n: \"${SEL:=a[x=\\\"t\\\"], helper[y=\\\"u\\\"]}\"\n",
            ),
        ],
        &[("lib.sh", "helper() {\n  echo two\n}\n")],
    ]);
    let out = impact(tmp.path());
    let missed: Vec<&str> = changed(&out, "lib.sh", "helper")["missed_in"]
        .as_array()
        .map(|m| m.iter().filter_map(|f| f["path"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(missed, ["c.sh"], "{out:#}");
}

const V1: &str = "CREATE SCHEMA lib;\n\nCREATE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER\nLANGUAGE plpgsql AS $$\nDECLARE\n  n INTEGER;\nBEGIN\n  UPDATE books SET refreshed = now() WHERE album = target_album;\n  GET DIAGNOSTICS n = ROW_COUNT;\n  RETURN n;\nEND;\n$$;\n";
const V2: &str = "CREATE OR REPLACE FUNCTION lib.refresh_all() RETURNS void\nLANGUAGE plpgsql AS $$\nDECLARE\n  album UUID;\nBEGIN\n  FOR album IN SELECT id FROM albums LOOP\n    PERFORM lib.refresh_book(album);\n  END LOOP;\nEND;\n$$;\n\nSELECT lib.refresh_book(b.album_id) FROM books b;\n";
const DECOYS: &str = "-- lib.refresh_book(x) in a comment\nSELECT 'lib.refresh_book(y)' AS s, $tag$lib.refresh_book(z)$tag$ AS t;\nSELECT other.refresh_book(1);\nCREATE TABLE refresh_book (id int);\nINSERT INTO refresh_book (id) VALUES (1);\nDROP FUNCTION IF EXISTS lib.refresh_book(uuid);\n";
const VIEW: &str = "CREATE OR REPLACE VIEW lib.book_view AS\n  SELECT b.id, lib.refresh_book(b.album) AS n FROM books b;\n";
const PROCEDURE: &str = "CREATE PROCEDURE lib.rebuild()\nLANGUAGE plpgsql AS $body$\nBEGIN\n  IF true THEN\n    PERFORM refresh_book(NULL);\n  END IF;\nEND;\n$body$;\n";
const JDBC: &str =
    "class Refresh {\n    static final String SQL = \"SELECT lib.refresh_book(?)\";\n}\n";

fn migrations() -> Vec<(&'static str, &'static str)> {
    vec![
        ("db/V1__fn.sql", V1),
        ("db/V2__use.sql", V2),
        ("db/V4__decoys.sql", DECOYS),
        ("db/V5__view.sql", VIEW),
        (
            "db/V6__report.sql",
            "SELECT n FROM lib.book_view WHERE n > 0;\n",
        ),
        ("db/V7__proc.sql", PROCEDURE),
        ("worker/Refresh.java", JDBC),
    ]
}

/// #459: `CREATE FUNCTION lib.refresh_book` was no definition, so
/// `--symbol` found none, and the migrations calling it from PL/pgSQL, a
/// view and a top-level statement were no callers. An unqualified call is
/// left to the search path; a comment, a string, another schema's routine,
/// a table of that name and a DROP are none.
#[test]
fn sql_routines_and_views_are_definitions_with_callers() {
    let files = migrations();
    let tmp = repo(&[&files]);
    for query in ["refresh_book", "lib.refresh_book"] {
        let out = symbol(tmp.path(), query);
        assert_eq!(
            callers(&out, "db/V1__fn.sql", "refresh_book"),
            set(&[
                "db/V2__use.sql::refresh_all",
                "db/V2__use.sql@12",
                "db/V5__view.sql::book_view",
                "possibly db/V7__proc.sql::rebuild (search_path)",
            ]),
            "{query}: {out:#}"
        );
        assert!(
            out["contracts"].as_array().is_none_or(Vec::is_empty),
            "a query changes no schema: {out:#}"
        );
    }
    let view = symbol(tmp.path(), "book_view");
    let referenced: Vec<&str> = changed(&view, "db/V5__view.sql", "book_view")["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["path"].as_str().unwrap())
        .collect();
    assert_eq!(referenced, ["db/V6__report.sql"], "{view:#}");
}

/// #459: a later `CREATE OR REPLACE` redefines the routine; its callers are
/// the routine's.
#[test]
fn a_redefined_sql_routine_keeps_its_callers() {
    let files = migrations();
    let tmp = repo(&[
        &files,
        &[(
            "db/V3__redo.sql",
            "CREATE OR REPLACE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER\nLANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 0;\nEND;\n$$;\n",
        )],
    ]);
    let out = impact(tmp.path());
    assert_eq!(
        callers(&out, "db/V3__redo.sql", "refresh_book"),
        set(&[
            "db/V2__use.sql::refresh_all",
            "db/V2__use.sql@12",
            "db/V5__view.sql::book_view",
            "possibly db/V7__proc.sql::rebuild (search_path)",
        ]),
        "{out:#}"
    );
}
