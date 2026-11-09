//! The shell resolver on real git repositories: a call lands on the function of
//! its own script or of a file the script sources, however the path is spelled,
//! and what the sources cannot settle is a candidate. The expectations are the
//! fixtures of the shell lane (`lane_sh.rs`) and of the impact corpus, read at the
//! resolver; nothing here runs diffctx to learn an answer.

mod snapshot_fixture;

use _diffctx::facts::{DefKey, FactsCache};
use _diffctx::resolve::{Binding, CandidateReason, Relation, Resolution, ShellResolver};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag};
use snapshot_fixture::Repo;

struct Fixture {
    _repo: Repo,
    head: ShellResolver,
}

fn open(files: &[(&str, &str)]) -> Fixture {
    let repo = Repo::init();
    for (path, text) in files {
        repo.write(path, text);
    }
    let head = repo.commit("snapshot");
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        RangeSpec::At(Side::Tree(head)),
        RunBudget::default(),
    )
    .expect("an analysis context");
    let head = ShellResolver::open(&ctx, SideTag::Head, &FactsCache::disabled())
        .expect("the head resolves");
    Fixture { _repo: repo, head }
}

impl Fixture {
    fn where_is(&self, key: &DefKey) -> String {
        let found = self
            .head
            .definitions()
            .iter()
            .find(|d| d.key == *key)
            .expect("a resolved target is a definition");
        format!("{} in {}", key.local.name, found.file)
    }

    /// What each use of `name` in `file` lands on, by line.
    fn lands(&self, file: &str, name: &str, relation: Relation) -> Vec<(u32, String)> {
        self.head
            .bindings()
            .iter()
            .filter(|b| {
                b.file.as_str() == file && b.name.as_str() == name && b.relation == relation
            })
            .map(|b| (b.site.lines.0, self.describe(b)))
            .collect()
    }

    fn describe(&self, binding: &Binding) -> String {
        match &binding.resolution {
            Resolution::Resolved { target, .. } => self.where_is(target),
            Resolution::Candidate { targets, reason } => {
                let mut names: Vec<String> = targets.iter().map(|t| self.where_is(t)).collect();
                names.sort();
                format!("candidate({reason:?}) {}", names.join(" | "))
            }
            Resolution::Module { file, .. } => format!("module {file}"),
            Resolution::Unresolved { reason } => format!("unresolved({reason:?})"),
        }
    }
}

const LIB: &str = "fetch() {\n  curl -s \"$1\"\n}\n";
const MY_LIB: &str = "login() {\n  echo token\n}\n";

fn library_fixture() -> Vec<(&'static str, &'static str)> {
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
            "source \"$LIB_HOME/my-lib.sh\"\nticket=$(login)\n",
        ),
        ("scripts/other.sh", "fetch() {\n  wget \"$1\"\n}\nfetch z\n"),
        ("scripts/standalone.sh", "fetch q\n"),
        (
            "scripts/words.sh",
            ". ./lib.sh\n. ./my-lib.sh\necho fetch login\n# fetch it, then login\nprintf '%s\\n' \"fetch\"\nmsg=\"login\"\n",
        ),
        ("scripts/config.sh", "source \"$CONFIG_FILE\"\nlogin\n"),
        (
            "tests/test_lib.sh",
            "source \"$(dirname \"$0\")/../scripts/lib.sh\"\nfetch t\n",
        ),
    ]
}

#[test]
fn a_sourced_library_binds_its_callers_however_the_path_is_spelled() {
    let fixture = open(&library_fixture());
    let fetch = |file: &str| fixture.lands(file, "fetch", Relation::Calls);
    let login = |file: &str| fixture.lands(file, "login", Relation::Calls);
    let lib = "fetch in scripts/lib.sh".to_string();
    let my_lib = "login in scripts/my-lib.sh".to_string();
    assert_eq!(fetch("scripts/plain.sh"), [(2, lib.clone())]);
    assert_eq!(fetch("scripts/dyn.sh"), [(2, lib.clone())]);
    assert_eq!(fetch("scripts/run.sh"), [(4, lib.clone())]);
    assert_eq!(fetch("tests/test_lib.sh"), [(2, lib)]);
    assert_eq!(login("scripts/run.sh"), [(6, my_lib.clone())]);
    assert_eq!(login("scripts/plain2.sh"), [(2, my_lib.clone())]);
    // A variable set from the script's directory, and one nothing sets (the
    // file name decides).
    assert_eq!(login("scripts/var.sh"), [(4, my_lib.clone())]);
    assert_eq!(login("scripts/env.sh"), [(2, my_lib)]);
}

#[test]
fn a_script_defining_the_name_calls_its_own_and_one_that_sources_nothing_calls_nothing() {
    let fixture = open(&library_fixture());
    assert_eq!(
        fixture.lands("scripts/other.sh", "fetch", Relation::Calls),
        [(4, "fetch in scripts/other.sh".to_string())]
    );
    assert_eq!(
        fixture.lands("scripts/standalone.sh", "fetch", Relation::Calls),
        [(1, "unresolved(External)".to_string())]
    );
    // The names as words: no command of that name.
    assert!(
        fixture
            .lands("scripts/words.sh", "fetch", Relation::Calls)
            .is_empty()
    );
    assert!(
        fixture
            .lands("scripts/words.sh", "login", Relation::Calls)
            .is_empty()
    );
}

#[test]
fn a_source_nothing_settles_makes_the_call_a_candidate_and_is_counted() {
    let fixture = open(&library_fixture());
    assert_eq!(
        fixture.lands("scripts/config.sh", "login", Relation::Calls),
        [(
            2,
            "candidate(ImportUnresolved) login in scripts/my-lib.sh".to_string()
        )]
    );
    assert_eq!(fixture.head.unknown_sources(), 1);
    assert_eq!(fixture.head.unread_scripts(), 0);
}

#[test]
fn a_file_name_two_files_carry_is_ambiguous_and_a_pinned_path_is_not() {
    let fixture = open(&[
        ("a/util.sh", "helper() {\n  echo a\n}\n"),
        ("b/util.sh", "helper() {\n  echo b\n}\n"),
        ("c/use.sh", "source \"$ROOT/util.sh\"\nhelper\n"),
        (
            "d/pinned.sh",
            "source \"$(dirname \"$0\")/../a/util.sh\"\nhelper\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("d/pinned.sh", "helper", Relation::Calls),
        [(2, "helper in a/util.sh".to_string())]
    );
    let ambiguous = fixture.lands("c/use.sh", "helper", Relation::Calls);
    assert_eq!(
        ambiguous,
        [(
            2,
            "candidate(AmbiguousModule) helper in a/util.sh | helper in b/util.sh".to_string()
        )]
    );
    assert!(matches!(
        fixture
            .head
            .bindings()
            .iter()
            .find(|b| b.file.as_str() == "c/use.sh" && b.relation == Relation::Calls)
            .map(|b| &b.resolution),
        Some(Resolution::Candidate {
            reason: CandidateReason::AmbiguousModule,
            ..
        })
    ));
}

#[test]
fn a_library_calls_what_the_scripts_that_source_it_source_beside_it() {
    let fixture = open(&[
        ("lib/log-lib.sh", "log() {\n  echo \"$*\" >&2\n}\n"),
        ("lib/util.sh", "tidy() {\n  log \"tidying\"\n}\n"),
        (
            "bin/deploy",
            "#!/bin/bash\nDIR=\"$(dirname \"$0\")/../lib\"\n. \"$DIR/log-lib.sh\"\n. \"$DIR/util.sh\"\ntidy\n",
        ),
        ("lib/orphan.sh", "sweep() {\n  log \"sweeping\"\n}\n"),
    ]);
    assert_eq!(
        fixture.lands("lib/util.sh", "log", Relation::Calls),
        [(2, "log in lib/log-lib.sh".to_string())]
    );
    assert_eq!(
        fixture.lands("lib/orphan.sh", "log", Relation::Calls),
        [(2, "unresolved(External)".to_string())]
    );
    assert_eq!(
        fixture.lands("bin/deploy", "tidy", Relation::Calls),
        [(5, "tidy in lib/util.sh".to_string())]
    );
}

#[test]
fn a_function_a_script_sources_a_second_time_is_the_scripts_own_and_not_the_librarys() {
    let fixture = open(&[
        ("lib.sh", "f() {\n  echo lib\n}\n"),
        ("a.sh", ". ./lib.sh\nf() {\n  echo mine\n}\nf\n"),
    ]);
    assert_eq!(
        fixture.lands("a.sh", "f", Relation::Calls),
        [(5, "f in a.sh".to_string())]
    );
}

#[test]
fn a_variable_a_sourced_file_assigns_is_read_there_and_a_local_is_not() {
    let fixture = open(&[
        (
            "lib.sh",
            "FOO='a|b'\nbar() {\n  local FOO=1\n  echo \"$FOO\"\n}\n",
        ),
        ("user.sh", ". ./lib.sh\necho \"$FOO\"\n"),
        ("own.sh", "X=1\nX=2\necho \"$X\"\n"),
    ]);
    assert_eq!(
        fixture.lands("user.sh", "FOO", Relation::Reads),
        [(2, "FOO in lib.sh".to_string())]
    );
    // `bar`'s FOO is its own.
    assert!(fixture.lands("lib.sh", "FOO", Relation::Reads).is_empty());
    // A second assignment in a script is a write of the one it defined.
    assert_eq!(
        fixture.lands("own.sh", "X", Relation::Writes),
        [(2, "X in own.sh".to_string())]
    );
    assert_eq!(
        fixture.lands("own.sh", "X", Relation::Reads),
        [(3, "X in own.sh".to_string())]
    );
}

#[test]
fn a_sourced_and_a_run_path_land_on_the_file_they_name() {
    let fixture = open(&[
        ("bin/lib.sh", "x() { :; }\n"),
        ("bin/tool.sh", "echo tool\n"),
        (
            "bin/main.sh",
            ". ./lib.sh\n./tool.sh\nbash bin/tool.sh\nbash -c './nothing'\n. /etc/profile\n",
        ),
    ]);
    assert_eq!(
        fixture.lands("bin/main.sh", "./lib.sh", Relation::Imports),
        [(1, "<script> in bin/lib.sh".to_string())]
    );
    assert_eq!(
        fixture.lands("bin/main.sh", "./tool.sh", Relation::Calls),
        [(2, "<script> in bin/tool.sh".to_string())]
    );
    assert_eq!(
        fixture.lands("bin/main.sh", "bin/tool.sh", Relation::Calls),
        [(3, "<script> in bin/tool.sh".to_string())]
    );
    assert_eq!(
        fixture.lands("bin/main.sh", "/etc/profile", Relation::Imports),
        [(5, "unresolved(External)".to_string())]
    );
}

#[test]
fn a_script_for_another_shell_by_shebang_or_extension_is_counted_unread() {
    let fixture = open(&[
        ("a.sh", "#!/bin/zsh\nf() { :; }\n"),
        ("b.sh", "#!/bin/bash\ng() { :; }\n"),
        ("c.zsh", "h() { :; }\n"),
    ]);
    assert_eq!(fixture.head.unread_scripts(), 2);
    assert!(
        fixture
            .head
            .definitions()
            .iter()
            .all(|d| d.file.as_str() == "b.sh")
    );
}

#[test]
fn what_sources_or_runs_a_script_reads_its_program() {
    let fixture = open(&[
        ("lib.sh", "echo loading\n"),
        ("a.sh", ". ./lib.sh\n"),
        ("b.sh", "bash lib.sh\n"),
        ("c.sh", "echo lib.sh\n"),
    ]);
    let script = fixture
        .head
        .definitions()
        .iter()
        .find(|d| d.file.as_str() == "lib.sh" && d.key.local.name.as_str() == "<script>")
        .expect("the script is a definition")
        .key
        .clone();
    let readers: Vec<(String, Relation)> = fixture
        .head
        .references_to(&script)
        .iter()
        .map(|b| (b.file.to_string(), b.relation))
        .collect();
    assert_eq!(
        readers,
        [
            ("a.sh".to_string(), Relation::Imports),
            ("b.sh".to_string(), Relation::Calls)
        ]
    );
}
