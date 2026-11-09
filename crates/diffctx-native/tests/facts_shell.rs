//! The shell extractor, fact kind by fact kind, on scripts small enough to read
//! the right answer off. Nothing here runs diffctx to learn an answer.

use _diffctx::facts::lang::shell::{ShellExtractor, UnreadShellExtractor};
use _diffctx::facts::shell::{PathHead, PathUse};
use _diffctx::facts::{BlobFacts, Def, DefKind, Extractor, Fact, Language, UseKind};
use _diffctx::snapshot::{Loss, RepoPath, RunBudget};

fn extract(source: &str) -> BlobFacts {
    ShellExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn defs(blob: &BlobFacts) -> Vec<&Def> {
    blob.defs().collect()
}

fn names_of(blob: &BlobFacts, use_kind: UseKind) -> Vec<(String, u32)> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Reference(r) if r.use_kind == use_kind => {
                Some((r.name.to_string(), r.site.lines.0))
            }
            _ => None,
        })
        .collect()
}

const SCRIPT: &str = "#!/bin/bash
LIB=\"$(dirname \"$0\")/lib\"
source \"$LIB/log.sh\"
. ./util.sh
./run.sh --now
bash tools/build.sh
greet() {
  local name=\"$1\"
  count=1
  echo \"hello $name\"
  log \"$count\"
  time -p tidy
  sudo hidden
}
greet \"$GLOBAL\"
GLOBAL=x
GLOBAL=y
";

#[test]
fn the_language_of_a_script_is_its_extension_or_its_shebang() {
    let path = |p: &str| RepoPath::new(p).expect("a path");
    assert_eq!(
        Language::from_path(&path("bin/run.sh")),
        Some(Language::Shell)
    );
    assert_eq!(Language::from_path(&path("x.bash")), Some(Language::Shell));
    assert_eq!(
        Language::detect(&path("bin/call"), b"#!/bin/bash\n"),
        Some(Language::Shell)
    );
    assert_eq!(
        Language::detect(&path("bin/call"), b"#!/usr/bin/env sh\n"),
        Some(Language::Shell)
    );
    assert_eq!(
        Language::detect(&path("bin/call"), b"#!/usr/bin/env python3\n"),
        Some(Language::Python)
    );
    assert_eq!(Language::detect(&path("bin/call"), b"echo hi\n"), None);
}

#[test]
fn a_script_for_another_shell_is_of_another_language_by_extension_and_by_shebang() {
    let path = |p: &str| RepoPath::new(p).expect("a path");
    for name in ["x.zsh", "x.ksh", "x.fish"] {
        assert_eq!(
            Language::from_path(&path(name)),
            Some(Language::ShellUnread),
            "{name}"
        );
    }
    assert_eq!(
        Language::detect(&path("bin/call"), b"#!/usr/bin/env zsh\n"),
        Some(Language::ShellUnread)
    );
    let blob = UnreadShellExtractor.extract(b"f() { :; }\n", &RunBudget::default());
    assert!(blob.facts.is_empty());
    assert!(matches!(blob.losses.as_slice(), [Loss::Unsupported { .. }]));
}

#[test]
fn a_function_and_a_global_variable_are_definitions_and_a_local_is_not() {
    let blob = extract(SCRIPT);
    assert!(blob.losses.is_empty(), "{:?}", blob.losses);
    let seen: Vec<(&str, DefKind, u32)> = defs(&blob)
        .iter()
        .map(|d| (d.local.name.as_str(), d.kind, d.span.lines.0))
        .collect();
    // `count` is assigned in `greet` without `local`: a global, defined where it
    // is first assigned. `name` is local, `GLOBAL`'s second assignment a write.
    assert_eq!(
        seen,
        [
            ("<script>", DefKind::Module, 1),
            ("LIB", DefKind::Variable, 2),
            ("greet", DefKind::Function, 7),
            ("count", DefKind::Variable, 9),
            ("GLOBAL", DefKind::Variable, 16),
        ]
    );
    let locals: Vec<&str> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::LocalBinding(l) => Some(l.name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(locals, ["name"]);
}

#[test]
fn a_command_word_is_a_call_and_a_name_after_time_is_one_and_after_sudo_is_not() {
    let blob = extract(SCRIPT);
    let calls: Vec<(String, u32)> = names_of(&blob, UseKind::Call);
    let expected = [
        ("dirname", 2),
        ("echo", 10),
        ("log", 11),
        ("tidy", 12),
        ("sudo", 13),
        ("greet", 15),
    ];
    assert_eq!(calls, expected.map(|(n, l)| (n.to_string(), l)).to_vec());
}

#[test]
fn a_variable_is_read_by_its_expansion_and_a_local_is_no_read_of_the_global() {
    let blob = extract(SCRIPT);
    let reads: Vec<(String, u32)> = names_of(&blob, UseKind::Read);
    assert_eq!(
        reads,
        [("LIB", 3), ("count", 11), ("GLOBAL", 15)]
            .map(|(n, l)| (n.to_string(), l))
            .to_vec()
    );
    assert_eq!(
        names_of(&blob, UseKind::Write),
        [("GLOBAL".to_string(), 17)]
    );
}

#[test]
fn a_sourced_or_run_path_is_spelled_as_a_head_and_a_tail() {
    let blob = extract(SCRIPT);
    let paths: Vec<(PathUse, PathHead, &str, u32)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::ShellPath(p) => Some((p.how, p.head, p.tail.as_str(), p.site.lines.0)),
            _ => None,
        })
        .collect();
    assert_eq!(
        paths,
        [
            (PathUse::Source, PathHead::ScriptDir, "/lib/log.sh", 3),
            (PathUse::Source, PathHead::Literal, "./util.sh", 4),
            (PathUse::Run, PathHead::Literal, "./run.sh", 5),
            (PathUse::Run, PathHead::Literal, "tools/build.sh", 6),
        ]
    );
}

fn function_hash(source: &str, name: &str) -> u64 {
    defs(&extract(source))
        .iter()
        .find(|d| d.local.name.as_str() == name)
        .map(|d| d.body_hash)
        .expect("a definition")
}

#[test]
fn a_comment_is_layout_and_a_quote_style_is_not() {
    let hash = |source: &str| function_hash(source, "greet");
    let base = "greet() {\n  echo '$HOME'\n}\n";
    let comment = "greet() {\n  # said hello\n  echo '$HOME'\n}\n";
    let quoted = "greet() {\n  echo \"$HOME\"\n}\n";
    assert_eq!(hash(base), hash(comment));
    assert_ne!(hash(base), hash(quoted));
}

#[test]
fn a_script_for_another_shell_is_unread_and_says_so() {
    for shebang in [
        "#!/bin/zsh",
        "#!/usr/bin/env zsh",
        "#!/usr/bin/env fish",
        "#!/bin/ksh",
    ] {
        let blob = extract(&format!("{shebang}\nfoo() {{ :; }}\nfoo\n"));
        assert!(blob.facts.is_empty(), "{shebang}");
        assert!(
            blob.losses
                .iter()
                .any(|l| matches!(l, Loss::Unsupported { .. })),
            "{shebang}: {:?}",
            blob.losses
        );
    }
    let sh = extract("#!/bin/sh\nfoo() { :; }\n");
    assert!(sh.losses.is_empty());
    assert_eq!(defs(&sh).len(), 2);
}

#[test]
fn a_broken_parse_is_a_line_range_and_the_rest_is_still_read() {
    let blob = extract(
        "helper() {\n  echo one\n}\n: \"${SEL:=input[type=\\\"text\\\"], input[type=\\\"email\\\"]}\"\nhelper\n",
    );
    assert!(
        blob.losses
            .iter()
            .any(|l| matches!(l, Loss::ParseError { .. })),
        "{:?}",
        blob.losses
    );
    assert!(
        defs(&blob)
            .iter()
            .any(|d| d.local.name.as_str() == "helper")
    );
}

#[test]
fn a_function_or_an_assignment_edit_is_no_edit_of_the_script_even_as_text() {
    let text = |source: &str| {
        defs(&extract(source))
            .iter()
            .find(|d| d.local.name.as_str() == "<script>")
            .map(|d| d.text_hash)
            .expect("a definition")
    };
    let base = "#!/bin/bash\nf() {\n  echo one\n}\nFOO=1\necho start\nf\n";
    assert_eq!(text(base), text(&base.replace("echo one", "echo two")));
    assert_eq!(text(base), text(&base.replace("FOO=1", "FOO=2")));
    assert_eq!(text(base), text(&format!("{base}\ng() {{ :; }}\n")));
    assert_eq!(
        text(base),
        text(&base.replace("f() {\n  echo one\n}\n", ""))
    );
    // A comment in the program is layout of the script, and a command is its edit.
    assert_ne!(
        text(base),
        text(&base.replace("echo start", "# note\necho start"))
    );
    assert_ne!(text(base), text(&base.replace("echo start", "echo begin")));
}

#[test]
fn the_script_is_a_definition_and_only_its_program_moves_its_hash() {
    let script = |source: &str| function_hash(source, "<script>");
    let base = "#!/bin/bash\nf() {\n  echo one\n}\necho start\nf\n";
    // A function body is the function's own change, not the script's.
    assert_eq!(script(base), script(&base.replace("echo one", "echo two")));
    // A comment and an assignment are no change of the program.
    assert_eq!(
        script(base),
        script(&base.replace("echo start", "# note\necho start"))
    );
    assert_eq!(script(base), script(&format!("{base}FOO=1\n")));
    // A function added or removed is that function's own change.
    assert_eq!(script(base), script(&format!("{base}g() {{ :; }}\n")));
    assert_eq!(
        script(base),
        script(&base.replace("f() {\n  echo one\n}\n", ""))
    );
    // A command and a new shebang are the program's.
    assert_ne!(
        script(base),
        script(&base.replace("echo start", "echo begin"))
    );
    assert_ne!(
        script(base),
        script(&base.replace("#!/bin/bash", "#!/bin/sh"))
    );
    let blob = extract(base);
    let found = defs(&blob);
    assert_eq!(found[0].kind, DefKind::Module);
    assert_eq!(found[0].span.lines, (1, 6));
}
