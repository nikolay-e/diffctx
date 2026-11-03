//! What a range did to the definitions, on real git repositories. The extractor
//! is the fixture's stand-in for Python (see `facts_fixture`); the language's own
//! shapes are held by the language files' tests.

mod facts_fixture;
mod snapshot_fixture;

use std::sync::atomic::Ordering;

use _diffctx::facts::{
    DefChange, Delta, Disambig, FactsCache, Language, LocalKey, Name, Namespace, Origin, Registry,
    SemanticChangeSet, TypeName, semantic_changes,
};
use _diffctx::snapshot::{Loss, RepoPath, RunBudget, SideTag};
use facts_fixture::{Toy, key, key_in, key_of, review};
use snapshot_fixture::Repo;

const APP: &str = "\
def greet(name):
    \"\"\"Say hello.\"\"\"
    return \"hello \" + name

def _helper(x):
    return x + 1
";

fn changed(origin: Origin, delta: Delta) -> DefChange {
    DefChange::Changed { origin, delta }
}

fn same(delta: Delta) -> DefChange {
    changed(Origin::Same, delta)
}

fn body() -> Delta {
    Delta {
        body: true,
        ..Delta::default()
    }
}

fn path(text: &str) -> RepoPath {
    RepoPath::new(text).expect("a path")
}

/// A set as the tests read it: each definition's key and change.
#[derive(Debug, PartialEq)]
struct View {
    defs: Vec<(_diffctx::facts::DefKey, DefChange)>,
    losses: Vec<Loss>,
}

fn view(set: SemanticChangeSet) -> View {
    View {
        defs: set.defs.into_iter().map(|d| (d.key, d.change)).collect(),
        losses: set.losses,
    }
}

/// `before` committed as the base and `after` as the head, both of `file`.
fn edit(file: &str, before: &str, after: &str) -> View {
    let repo = Repo::init();
    repo.write(file, before);
    let base = repo.commit("base");
    repo.write(file, after);
    let head = repo.commit("head");
    run(&repo, &base, &head)
}

fn changes(
    repo: &Repo,
    base: &_diffctx::snapshot::Oid,
    head: &_diffctx::snapshot::Oid,
) -> SemanticChangeSet {
    let toy = Toy::new();
    let ctx = review(repo, base, head);
    let cache = FactsCache::disabled().with_registry(toy.registry());
    semantic_changes(&ctx, &cache).expect("semantic changes")
}

fn run(repo: &Repo, base: &_diffctx::snapshot::Oid, head: &_diffctx::snapshot::Oid) -> View {
    view(changes(repo, base, head))
}

#[test]
fn a_body_edit_is_a_body_change_of_that_definition_alone() {
    let set = edit("app.py", APP, &APP.replace("x + 1", "x + 2"));
    assert_eq!(set.defs, vec![(key("app", "_helper"), same(body()))]);
    assert!(set.losses.is_empty(), "{:?}", set.losses);
}

#[test]
fn a_new_parameter_is_a_signature_change_never_a_removal_and_an_addition() {
    let set = edit(
        "app.py",
        APP,
        &APP.replace("greet(name)", "greet(name, polite)"),
    );
    assert_eq!(
        set.defs,
        vec![(
            key("app", "greet"),
            same(Delta {
                signature: true,
                ..Delta::default()
            })
        )]
    );
}

#[test]
fn a_docstring_edit_is_a_doc_change_and_leaves_the_body_alone() {
    let set = edit("app.py", APP, &APP.replace("Say hello.", "Greet someone."));
    assert_eq!(
        set.defs,
        vec![(
            key("app", "greet"),
            same(Delta {
                doc: true,
                ..Delta::default()
            })
        )]
    );
}

#[test]
fn comments_blank_lines_and_a_uniform_re_indent_are_changes_with_an_empty_delta() {
    let commented = APP.replace(
        "    return x + 1\n",
        "    # a note\n\n    return x + 1  # inline\n",
    );
    let set = edit("app.py", APP, &commented);
    assert_eq!(
        set.defs,
        vec![(key("app", "_helper"), same(Delta::default()))]
    );

    let narrow = APP.replace("    return \"hello \"", "  return \"hello \"");
    let narrow = narrow.replace("    \"\"\"Say hello.\"\"\"", "  \"\"\"Say hello.\"\"\"");
    let set = edit("app.py", APP, &narrow);
    assert_eq!(
        set.defs,
        vec![(key("app", "greet"), same(Delta::default()))],
        "two spaces instead of four moves no statement between blocks"
    );
}

#[test]
fn a_re_indent_that_moves_a_statement_between_blocks_is_a_body_change() {
    let before = "def run(items):\n    for x in items:\n        use(x)\n    done()\n";
    let after = "def run(items):\n    for x in items:\n        use(x)\n        done()\n";
    let set = edit("job.py", before, after);
    assert_eq!(set.defs, vec![(key("job", "run"), same(body()))]);
}

#[test]
fn a_definition_appended_to_a_file_is_the_only_entry() {
    let set = edit(
        "app.py",
        APP,
        &format!("{APP}\ndef extra():\n    return 0\n"),
    );
    assert_eq!(set.defs, vec![(key("app", "extra"), DefChange::Added)]);
}

#[test]
fn an_added_and_a_removed_definition_and_a_whole_file_of_each() {
    let repo = Repo::init();
    repo.write("app.py", APP);
    repo.write("old.py", "def gone():\n    return 1\n");
    let base = repo.commit("base");
    repo.write(
        "app.py",
        &APP.replace("def _helper(x):\n    return x + 1\n", ""),
    );
    repo.remove("old.py");
    repo.write("new.py", "def fresh():\n    return 2\n");
    let head = repo.commit("head");
    let set = run(&repo, &base, &head);
    assert_eq!(
        set.defs,
        vec![
            (key("app", "_helper"), DefChange::Removed),
            (key("new", "fresh"), DefChange::Added),
            (key("old", "gone"), DefChange::Removed),
        ]
    );
}

#[test]
fn a_file_moved_unchanged_to_another_module_is_a_move_of_each_definition() {
    let repo = Repo::init();
    repo.write("pkg/a.py", APP);
    repo.write("other.py", "def keep():\n    return 0\n");
    let base = repo.commit("base");
    repo.git(&["mv", "pkg/a.py", "pkg/b.py"]);
    let head = repo.commit("head");
    // git calls this a 100% rename; what is claimed comes from the keys.
    let ctx = review(&repo, &base, &head);
    assert_eq!(ctx.changes.changes.len(), 1);
    assert_eq!(ctx.changes.changes[0].similarity, Some(100));
    let set = run(&repo, &base, &head);
    let moved = |name: &str| {
        (
            key("pkg.b", name),
            changed(
                Origin::Moved {
                    from: key("pkg.a", name),
                },
                Delta::default(),
            ),
        )
    };
    assert_eq!(set.defs, vec![moved("_helper"), moved("greet")]);
}

#[test]
fn a_moved_file_with_identical_trivial_methods_pairs_them_by_container() {
    let both = "class A:\n    def get(self):\n        return 0\nclass B:\n    def get(self):\n        return 0\n";
    let repo = Repo::init();
    repo.write("x.py", both);
    let base = repo.commit("base");
    repo.git(&["mv", "x.py", "y.py"]);
    let head = repo.commit("head");
    let set = run(&repo, &base, &head);
    assert_eq!(set.defs.len(), 2, "{:?}", set.defs);
    for (new, change) in &set.defs {
        let DefChange::Changed {
            origin: Origin::Moved { from },
            delta,
        } = change
        else {
            panic!("expected a move, got {change:?}");
        };
        assert_eq!(from.module.as_str(), "x");
        assert_eq!(new.module.as_str(), "y");
        assert_eq!(from.local, new.local);
        assert_eq!(*delta, Delta::default());
    }
}

#[test]
fn a_renamed_definition_is_a_rename_and_not_a_signature_change() {
    let before = "def old_name(x):\n    return x * 2\n";
    let after = "def new_name(x):\n    return x * 2\n";
    let set = edit("m.py", before, after);
    assert_eq!(
        set.defs,
        vec![(
            key("m", "new_name"),
            changed(
                Origin::Renamed {
                    from: key("m", "old_name")
                },
                Delta::default()
            )
        )]
    );
}

#[test]
fn a_rename_that_also_edits_the_body_is_a_removal_and_an_addition() {
    let before = "def old_name(x):\n    return x * 2\n";
    let after = "def new_name(x):\n    return x * 3\n";
    let set = edit("m.py", before, after);
    assert_eq!(
        set.defs,
        vec![
            (key("m", "new_name"), DefChange::Added),
            (key("m", "old_name"), DefChange::Removed),
        ],
        "equal bodies are the only evidence of a pairing"
    );
}

#[test]
fn several_equal_bodies_on_both_sides_pair_none_of_them() {
    let before = "def a():\n    return 7\ndef b():\n    return 7\n";
    let after = "def c():\n    return 7\ndef d():\n    return 7\n";
    let set = edit("m.py", before, after);
    assert_eq!(set.defs.len(), 4);
    for (_, change) in &set.defs {
        assert!(
            matches!(
                change,
                DefChange::Unknown {
                    loss: Loss::AmbiguousMatch { .. }
                }
            ),
            "{change:?}"
        );
    }
}

#[test]
fn a_redefinition_pairs_in_source_order() {
    let before = "def f():\n    return 1\ndef f():\n    return 2\n";
    let after = "def f():\n    return 1\ndef f():\n    return 3\n";
    let set = edit("m.py", before, after);
    assert_eq!(set.defs, vec![(key("m", "f"), same(body()))]);
}

fn local(name: &str, disambig: Disambig) -> LocalKey {
    LocalKey::value(name).with_disambig(disambig)
}

/// Twins that differ only by what a language's rules call them: each is its own
/// definition, and an edit to one is that one's change.
#[test]
fn definitions_that_differ_only_by_disambiguator_are_matched_to_their_twins() {
    let twins = |first: &str, second: &str, tail: &str| {
        format!("{first}\ndef f(a):\n    return 1\n{second}\ndef f(a):\n    return {tail}\n")
    };
    let cases: [(&str, &str, LocalKey); 4] = [
        (
            "@@overload 0",
            "@@overload 1",
            local("f", Disambig::Overload(1)),
        ),
        (
            "@@impl Display",
            "@@impl Debug",
            local("f", Disambig::ImplOf(TypeName::new("Debug"))),
        ),
        (
            "@@cfg unix",
            "@@cfg windows",
            local("f", Disambig::Cfg("windows".into())),
        ),
        (
            "@@cfg TYPE_CHECKING",
            "@@cfg runtime",
            local("f", Disambig::Cfg("runtime".into())),
        ),
    ];
    for (first, second, edited) in cases {
        let set = edit(
            "m.py",
            &twins(first, second, "2"),
            &twins(first, second, "3"),
        );
        assert_eq!(
            set.defs,
            vec![(key_of("m", edited), same(body()))],
            "{first} / {second}"
        );
    }
}

#[test]
fn a_type_and_a_value_of_one_name_are_two_definitions() {
    let both = |tail: &str| {
        format!(
            "@@ns type\ndef Point(x):\n    return {tail}\n@@ns value\ndef Point(x):\n    return 0\n"
        )
    };
    let set = edit("m.py", &both("1"), &both("2"));
    let type_key = LocalKey::new(Vec::new(), Name::new("Point"), Namespace::Type);
    assert_eq!(set.defs, vec![(key_of("m", type_key), same(body()))]);
}

#[test]
fn a_member_under_a_declared_container_keeps_the_container_in_its_key() {
    let source =
        |tail: &str| format!("@@container Outer.Companion\ndef create():\n    return {tail}\n");
    let set = edit("m.py", &source("1"), &source("2"));
    let companion = LocalKey::new(
        vec![Name::new("Outer"), Name::new("Companion")],
        Name::new("create"),
        Namespace::Value,
    );
    assert_eq!(set.defs, vec![(key_of("m", companion), same(body()))]);
}

#[test]
fn definitions_of_a_file_the_parser_lost_part_of_are_unknown_and_not_removed() {
    let before = "def f():\n    return 1\ndef g():\n    return 2\n";
    let after = "#parse-error\ndef f():\n    return 1\n";
    let set = edit("m.py", before, after);
    assert_eq!(set.defs.len(), 1, "{:?}", set.defs);
    assert_eq!(set.defs[0].0, key("m", "g"));
    assert!(
        matches!(
            &set.defs[0].1,
            DefChange::Unknown {
                loss: Loss::ParseError { .. }
            }
        ),
        "{:?}",
        set.defs[0].1
    );
    assert_eq!(
        set.losses,
        vec![Loss::ParseError {
            path: path("m.py"),
            lines: vec![(1, 1)]
        }]
    );
}

#[test]
fn a_panicking_extractor_is_a_loss_of_that_file_and_the_run_goes_on() {
    let repo = Repo::init();
    repo.write("good.py", "def f():\n    return 1\n");
    repo.write("bad.py", "def g():\n    return 1\n");
    let base = repo.commit("base");
    repo.write("good.py", "def f():\n    return 2\n");
    repo.write("bad.py", "#panic\ndef g():\n    return 1\n");
    let head = repo.commit("head");
    let set = run(&repo, &base, &head);
    assert!(
        set.defs.contains(&(key("good", "f"), same(body()))),
        "{:?}",
        set.defs
    );
    assert!(
        set.losses.contains(&Loss::ExtractorFailed {
            path: path("bad.py")
        }),
        "{:?}",
        set.losses
    );
    assert!(
        matches!(
            set.defs.iter().find(|(k, _)| *k == key("bad", "g")),
            Some((_, DefChange::Unknown { .. }))
        ),
        "{:?}",
        set.defs
    );
}

#[test]
fn a_blob_cut_short_by_the_budget_is_a_deadline_and_never_kept() {
    let dir = tempfile::tempdir().expect("dir");
    let repo = Repo::init();
    repo.write("m.py", "def f():\n    return 1\n");
    let base = repo.commit("base");
    repo.write("m.py", "#deadline\ndef f():\n    return 2\n");
    let head = repo.commit("head");
    let toy = Toy::new();
    let cache = facts_fixture::cache_in(dir.path(), &toy);
    let set = semantic_changes(&review(&repo, &base, &head), &cache).expect("changes");
    assert!(
        set.losses
            .iter()
            .any(|l| matches!(l, Loss::Deadline { .. })),
        "{:?}",
        set.losses
    );
    // The complete base blob is kept; the cut-short head blob is not.
    assert_eq!(facts_fixture::entries(dir.path()).len(), 1);
}

#[test]
fn a_cancelled_run_stops_with_a_named_loss_and_no_claims() {
    let repo = Repo::init();
    repo.write("m.py", "def f():\n    return 1\n");
    let base = repo.commit("base");
    repo.write("m.py", "def f():\n    return 2\n");
    let head = repo.commit("head");
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let budget = RunBudget {
        cancel: Some(cancel.clone()),
        ..RunBudget::default()
    };
    let ctx = _diffctx::snapshot::AnalysisContext::new(
        repo.root.clone(),
        _diffctx::snapshot::RangeSpec::Review { base, head },
        budget,
    )
    .expect("context");
    cancel.store(true, Ordering::SeqCst);
    let toy = Toy::new();
    let cache = FactsCache::disabled().with_registry(toy.registry());
    let set = semantic_changes(&ctx, &cache).expect("changes");
    assert!(set.defs.is_empty(), "{:?}", set.defs);
    assert_eq!(
        set.losses,
        vec![Loss::Deadline {
            stage: _diffctx::snapshot::Stage::Extraction
        }]
    );
    assert_eq!(toy.calls(), 0);
}

#[test]
fn a_language_without_an_extractor_is_named_and_a_text_file_is_not() {
    let repo = Repo::init();
    repo.write("lib.rs", "fn f() {}\n");
    repo.write("notes.txt", "one\n");
    let base = repo.commit("base");
    repo.write("lib.rs", "fn f() { 1 }\n");
    repo.write("notes.txt", "two\n");
    let head = repo.commit("head");
    let set = run(&repo, &base, &head);
    assert!(set.defs.is_empty());
    assert_eq!(
        set.losses,
        vec![Loss::Unsupported {
            path: path("lib.rs")
        }]
    );
}

#[test]
fn withheld_changes_are_counted_and_never_named() {
    let repo = Repo::init();
    repo.write("m.py", "def f():\n    return 1\n");
    let base = repo.commit("base");
    repo.write("m.py", "def f():\n    return 2\n");
    repo.write("deploy/id_rsa", "not a key\n");
    let head = repo.commit("head");
    let set = run(&repo, &base, &head);
    assert_eq!(set.losses, vec![Loss::Withheld { count: 1 }]);
    assert_eq!(set.defs, vec![(key("m", "f"), same(body()))]);
}

#[test]
fn two_runs_give_the_same_set() {
    let repo = Repo::init();
    repo.write("a.py", APP);
    repo.write("b.py", APP);
    let base = repo.commit("base");
    repo.write("a.py", &APP.replace("x + 1", "x + 5"));
    repo.write("b.py", &APP.replace("greet(name)", "greet(who)"));
    let head = repo.commit("head");
    assert_eq!(run(&repo, &base, &head), run(&repo, &base, &head));
}

#[test]
fn the_sides_of_the_context_are_base_and_head() {
    // facts_of reads the base side from the base snapshot and the head side from
    // the head's: a file that exists only in the head has no base facts.
    let repo = Repo::init();
    repo.write("a.py", APP);
    let base = repo.commit("base");
    repo.write("b.py", APP);
    let head = repo.commit("head");
    let toy = Toy::new();
    let ctx = review(&repo, &base, &head);
    let cache = FactsCache::disabled().with_registry(toy.registry());
    let b = path("b.py");
    assert!(
        _diffctx::facts::facts_of(&ctx, SideTag::Base, &cache, &b)
            .expect("base")
            .is_none()
    );
    assert!(
        _diffctx::facts::facts_of(&ctx, SideTag::Head, &cache, &b)
            .expect("head")
            .is_some()
    );
}

fn both_languages() -> Registry {
    Toy::new().registry_with(&Toy::typescript())
}

fn polyglot(
    repo: &Repo,
    base: &_diffctx::snapshot::Oid,
    head: &_diffctx::snapshot::Oid,
) -> SemanticChangeSet {
    let ctx = review(repo, base, head);
    let cache = FactsCache::disabled().with_registry(both_languages());
    semantic_changes(&ctx, &cache).expect("semantic changes")
}

#[test]
fn files_of_two_languages_in_one_module_name_never_share_a_definition() {
    let repo = Repo::init();
    repo.write("app.py", APP);
    repo.write("app.ts", APP);
    let base = repo.commit("base");
    repo.write("app.ts", &APP.replace("x + 1", "x + 2"));
    let head = repo.commit("head");
    let set = polyglot(&repo, &base, &head);
    assert_eq!(set.defs.len(), 1, "{:?}", set.defs);
    let only = &set.defs[0];
    assert_eq!(only.key, key_in(Language::TypeScript, "app", "_helper"));
    assert_eq!(only.change, same(body()));
    assert_eq!(only.base, Some(path("app.ts")));
    assert_eq!(only.head, Some(path("app.ts")));
}

#[test]
fn a_removal_in_one_language_and_an_addition_in_another_do_not_cancel_or_pair() {
    let repo = Repo::init();
    repo.write("app.py", "def a():\n    return 7\n");
    let base = repo.commit("base");
    repo.remove("app.py");
    repo.write("app.ts", "def a():\n    return 7\n");
    let head = repo.commit("head");
    let set = polyglot(&repo, &base, &head);
    let seen: Vec<_> = set
        .defs
        .iter()
        .map(|d| {
            (
                d.key.clone(),
                d.change.clone(),
                d.base.clone(),
                d.head.clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        vec![
            (
                key_in(Language::Python, "app", "a"),
                DefChange::Removed,
                Some(path("app.py")),
                None
            ),
            (
                key_in(Language::TypeScript, "app", "a"),
                DefChange::Added,
                None,
                Some(path("app.ts"))
            ),
        ]
    );
}

#[test]
fn a_moved_definition_names_the_file_on_each_side() {
    let repo = Repo::init();
    repo.write("pkg/a.py", "def f():\n    return 1\n");
    repo.write("keep.py", "def k():\n    return 0\n");
    let base = repo.commit("base");
    repo.git(&["mv", "pkg/a.py", "pkg/b.py"]);
    let head = repo.commit("head");
    let set = changes(&repo, &base, &head);
    assert_eq!(set.defs.len(), 1);
    assert_eq!(set.defs[0].base, Some(path("pkg/a.py")));
    assert_eq!(set.defs[0].head, Some(path("pkg/b.py")));
}
