use crate::{Entry, FileOutcomes, Lang, Outcome, Reason, Witness, shape, snapshot_in};

fn entries_on(found: &FileOutcomes, line: u32) -> Vec<&Entry> {
    found
        .entries
        .iter()
        .filter(|entry| entry.occurrence.line == line)
        .collect()
}

fn only(found: &FileOutcomes, line: u32) -> &Entry {
    let here = entries_on(found, line);
    assert_eq!(here.len(), 1, "line {line}: {found:#?}");
    here[0]
}

fn is_local(entry: &Entry) -> bool {
    matches!(
        &entry.outcome,
        Outcome::NotOurs {
            witness: Witness::Local(_),
            ..
        }
    )
}

const TOTAL: &str = "pub fn total() -> i64 {\n    1\n}\n\n";

#[test]
fn a_rust_closure_local_holds_the_name_to_the_end_of_its_block_and_no_further() {
    let source = format!(
        "{TOTAL}pub fn run() -> i64 {{\n    {{\n        let total = |x: i64| x;\n        total(1);\n    }}\n    let pair = ((1, total(2)), 0);\n    total(3) + pair.1\n}}\n"
    );
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", source.as_str())]);
    let found = world.outcomes(world.def("src/lib.rs", "total"), "src/lib.rs", &["total"]);
    assert_eq!(
        shape(only(&found, 7)),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
    assert!(is_local(only(&found, 8)), "{found:#?}");
    assert_eq!(only(&found, 10).outcome, Outcome::Ours, "{found:#?}");
    assert_eq!(only(&found, 11).outcome, Outcome::Ours, "{found:#?}");
}

#[test]
fn a_rust_local_is_still_in_force_after_a_block_of_the_same_function_closes() {
    let source = format!(
        "{TOTAL}pub fn run(flag: bool) -> i64 {{\n    let total = |x: i64| x;\n    if flag {{\n        prepare();\n    }}\n    total(1)\n}}\n"
    );
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", source.as_str())]);
    let found = world.outcomes(world.def("src/lib.rs", "total"), "src/lib.rs", &["total"]);
    assert!(is_local(only(&found, 10)), "{found:#?}");
}

#[test]
fn a_call_inside_the_initializer_of_a_local_of_its_name_reads_the_outer_name() {
    let source = format!(
        "{TOTAL}pub fn run(flag: bool) -> i64 {{\n    let total = if flag {{ prepare(); 1 }} else {{ total() }};\n    total\n}}\n"
    );
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", source.as_str())]);
    let found = world.outcomes(world.def("src/lib.rs", "total"), "src/lib.rs", &["total"]);
    let line = entries_on(&found, 6);
    assert_eq!(line.len(), 2, "{found:#?}");
    assert_eq!(
        shape(line[0]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
    assert_eq!(
        shape(line[1]).0,
        "ours",
        "the call in the initializer: {found:#?}"
    );
}

#[test]
fn a_parameter_holds_the_name_through_its_whole_function() {
    let source = format!("{TOTAL}pub fn run(total: fn() -> i64) -> i64 {{\n    total()\n}}\n");
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", source.as_str())]);
    let found = world.outcomes(world.def("src/lib.rs", "total"), "src/lib.rs", &["total"]);
    assert!(is_local(only(&found, 6)), "{found:#?}");
}

#[test]
fn a_local_of_another_name_or_of_another_function_holds_nothing_here() {
    let source = format!(
        "{TOTAL}pub fn other() -> i64 {{\n    let total = |x: i64| x;\n    total(1)\n}}\n\npub fn run() -> i64 {{\n    let other = 5;\n    total(other)\n}}\n"
    );
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", source.as_str())]);
    let found = world.outcomes(world.def("src/lib.rs", "total"), "src/lib.rs", &["total"]);
    assert!(is_local(only(&found, 7)), "{found:#?}");
    assert_eq!(only(&found, 12).outcome, Outcome::Ours, "{found:#?}");
}

#[test]
fn a_shell_word_is_a_read_of_a_local_only_with_its_sigil_a_target_or_an_arithmetic() {
    let source = "#!/bin/bash\ntotal() {\n  echo 1\n}\nrun() {\n  local total=0\n  on_exit total EXIT\n  total=5\n  echo $((total + 1))\n}\n";
    let world = snapshot_in(Lang::Shell, &[("run.sh", source)]);
    let found = world.outcomes(world.def("run.sh", "total"), "run.sh", &["total"]);
    assert_eq!(
        only(&found, 7).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{found:#?}"
    );
    assert!(is_local(only(&found, 8)), "{found:#?}");
    assert!(is_local(only(&found, 9)), "{found:#?}");
}

#[test]
fn a_member_spelled_like_a_php_local_is_no_read_of_it() {
    let source = "<?php\n\nfunction total() {\n    return 1;\n}\n\nfunction run($a) {\n    $total = 0;\n    $a->total = 5;\n    return $total;\n}\n";
    let world = snapshot_in(Lang::Php, &[("src/total.php", source)]);
    let found = world.outcomes(
        world.def("src/total.php", "total"),
        "src/total.php",
        &["total"],
    );
    assert!(!is_local(only(&found, 9)), "{found:#?}");
    assert!(is_local(only(&found, 10)), "{found:#?}");
}

#[test]
fn a_local_a_field_shares_its_name_with_declares_no_claim_about_the_field() {
    let file = "src/main/java/shop/Pricing.java";
    let source = "package shop;\n\npublic class Pricing {\n    private int total;\n\n    public int run() {\n        int total = 2;\n        return total;\n    }\n}\n";
    let world = snapshot_in(Lang::Java, &[(file, source)]);
    let found = world.outcomes(world.def(file, "Pricing.total"), file, &["total"]);
    assert_eq!(
        only(&found, 7).outcome,
        Outcome::Candidate(Reason::Unexplained),
        "{found:#?}"
    );
}

const OTHER: &str = "package shop;\n\npublic class Other {\n    Item make() {\n        return null;\n    }\n\n    void run() {\n        Item a = make();\n        int n = 1;\n    }\n\n    void two() {\n        Item first = make();\n        Item Item = first;\n        Item again = first;\n    }\n}\n";

#[test]
fn a_type_is_no_declaration_for_a_local_beside_it_before_or_after_the_local_of_its_name() {
    let world = snapshot_in(
        Lang::Java,
        &[
            (
                "src/main/java/shop/Item.java",
                "package shop;\n\npublic class Item {\n}\n",
            ),
            ("src/main/java/shop/Other.java", OTHER),
        ],
    );
    let found = world.outcomes(
        world.def("src/main/java/shop/Item.java", "Item"),
        "src/main/java/shop/Other.java",
        &["Item"],
    );
    for (line, why) in [
        (9, "beside locals of other names"),
        (14, "before the local of its name"),
        (16, "after the local of its name"),
    ] {
        assert_eq!(
            only(&found, line).outcome,
            Outcome::Ours,
            "{why}: {found:#?}"
        );
    }
    let line = entries_on(&found, 15);
    assert_eq!(line.len(), 2, "{found:#?}");
    assert_eq!(line[0].outcome, Outcome::Ours, "{found:#?}");
    assert_eq!(
        shape(line[1]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
}

#[test]
fn a_method_named_like_its_parameter_is_declared_by_its_name_not_by_the_parameter() {
    let file = "src/main/java/shop/Pricing.java";
    let source = "package shop;\n\npublic class Pricing {\n    public static int total(int total) {\n        return total;\n    }\n}\n";
    let world = snapshot_in(Lang::Java, &[(file, source)]);
    let found = world.outcomes(world.def(file, "Pricing.total"), file, &["total"]);
    let header = entries_on(&found, 4);
    assert_eq!(header.len(), 2, "{found:#?}");
    assert_eq!(
        shape(header[0]),
        ("declaration", "definition_name", None),
        "{found:#?}"
    );
    assert_eq!(
        shape(header[1]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
}

#[test]
fn a_kotlin_declarator_in_a_lambda_opened_right_after_an_equals_is_still_a_declaration() {
    let file = "src/main/kotlin/shop/P.kt";
    let source = "package shop\n\nfun total(): Int = 1\n\nval f ={ val total = 1; total }\n";
    let world = snapshot_in(Lang::Kotlin, &[(file, source)]);
    let found = world.outcomes(world.def(file, "total"), file, &["total"]);
    assert_eq!(
        shape(entries_on(&found, 5)[0]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
}
