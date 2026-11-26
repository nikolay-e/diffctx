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

#[test]
fn a_call_no_local_of_its_name_holds_is_not_a_local_whatever_other_locals_are_in_force() {
    let lib = "mod util;\n\npub fn early() -> i64 {\n    total(1);\n    let total = 2;\n    total\n}\n\npub fn beside(other: i64) -> i64 {\n    let held = 5;\n    total(held + other)\n}\n";
    let world = snapshot_in(
        Lang::Rust,
        &[
            ("src/lib.rs", lib),
            ("src/util.rs", "pub fn total(x: i64) -> i64 {\n    x\n}\n"),
        ],
    );
    let found = world.outcomes(world.def("src/util.rs", "total"), "src/lib.rs", &["total"]);
    assert!(
        !is_local(only(&found, 11)),
        "beside locals of other names: {found:#?}"
    );
    assert!(
        !is_local(entries_on(&found, 4)[0]),
        "before its own local: {found:#?}"
    );
}

fn local_site_line(entry: &Entry) -> Option<u32> {
    match &entry.outcome {
        Outcome::NotOurs {
            witness: Witness::Local(site),
            ..
        } => Some(site.lines.0),
        _ => None,
    }
}

const UTIL: &str = "pub fn total(x: i64) -> i64 {\n    x\n}\n";

fn outcomes_in_lib(lib: &str) -> FileOutcomes {
    let world = snapshot_in(Lang::Rust, &[("src/lib.rs", lib), ("src/util.rs", UTIL)]);
    world.outcomes(world.def("src/util.rs", "total"), "src/lib.rs", &["total"])
}

#[test]
fn a_call_of_a_closure_held_in_a_let_names_that_let_and_not_another_local_in_force() {
    let found = outcomes_in_lib(
        "mod util;\n\npub fn run() -> i64 {\n    let held = 5;\n    let total = |x: i64| x + held;\n    total(1)\n}\n",
    );
    assert_eq!(
        local_site_line(only(&found, 6)),
        Some(5),
        "the closure's own let, not `held` on line 4: {found:#?}"
    );
}

#[test]
fn a_closure_of_a_closed_block_holds_its_name_no_longer() {
    let found = outcomes_in_lib(
        "mod util;\n\npub fn run() -> i64 {\n    {\n        let total = |x: i64| x;\n        total(1);\n    }\n    total(2)\n}\n",
    );
    assert!(is_local(only(&found, 6)), "inside its block: {found:#?}");
    assert!(
        !is_local(only(&found, 8)),
        "after its block closed: {found:#?}"
    );
}

#[test]
fn a_closure_stays_in_force_past_a_block_opened_and_closed_after_it() {
    let found = outcomes_in_lib(
        "mod util;\n\npub fn run(flag: bool) -> i64 {\n    let total = |x: i64| x;\n    if flag {\n        prepare();\n    }\n    total(1)\n}\n",
    );
    assert!(is_local(only(&found, 8)), "{found:#?}");
}

#[test]
fn a_semicolon_inside_a_block_of_the_initializer_does_not_end_the_statement() {
    let found = outcomes_in_lib(
        "mod util;\n\npub fn run(flag: bool) -> i64 {\n    let total = if flag { prepare(); 1 } else { total(2) };\n    total\n}\n",
    );
    let call = entries_on(&found, 4);
    assert!(
        call.iter().skip(1).all(|entry| !is_local(entry)),
        "the call in the initializer reads the outer name: {found:#?}"
    );
}

fn java_item_beside(method: &str) -> FileOutcomes {
    let source = format!(
        "package shop;\n\npublic class Other {{\n    void run(Item first) {{\n{method}    }}\n}}\n"
    );
    let world = snapshot_in(
        Lang::Java,
        &[
            (
                "src/main/java/shop/Item.java",
                "package shop;\n\npublic class Item {\n}\n",
            ),
            ("src/main/java/shop/Other.java", source.as_str()),
        ],
    );
    world.outcomes(
        world.def("src/main/java/shop/Item.java", "Item"),
        "src/main/java/shop/Other.java",
        &["Item"],
    )
}

#[test]
fn a_read_of_a_local_after_its_declaration_is_no_declaration() {
    let found = java_item_beside("        Item Item = first;\n        System.out.println(Item);\n");
    for entry in entries_on(&found, 6) {
        assert_ne!(entry.outcome, Outcome::Declaration, "{found:#?}");
    }
    assert_eq!(
        shape(entries_on(&found, 5)[1]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
}

#[test]
fn a_name_read_before_a_later_local_of_it_is_no_declaration() {
    let found = java_item_beside("        System.out.println(Item);\n        Item Item = first;\n");
    for entry in entries_on(&found, 5) {
        assert_ne!(entry.outcome, Outcome::Declaration, "{found:#?}");
    }
}

#[test]
fn a_qualified_type_beside_a_local_of_its_name_is_no_declaration() {
    let found = java_item_beside("        outside.Item Item = first;\n");
    let line = entries_on(&found, 5);
    assert_eq!(line.len(), 2, "{found:#?}");
    assert_ne!(line[0].outcome, Outcome::Declaration, "{found:#?}");
    assert_eq!(
        shape(line[1]),
        ("declaration", "local_site", None),
        "{found:#?}"
    );
}

#[test]
fn a_loop_binding_of_the_name_of_a_module_level_static_keeps_its_hedge() {
    let world = snapshot_in(
        Lang::Rust,
        &[
            (
                "src/lib.rs",
                "mod util;\n\npub fn run(items: Vec<i64>) -> i64 {\n    for count in items {\n        prepare(count);\n    }\n    0\n}\n",
            ),
            ("src/util.rs", "pub static mut count: i64 = 0;\n"),
        ],
    );
    let found = world.outcomes(world.def("src/util.rs", "count"), "src/lib.rs", &["count"]);
    for entry in entries_on(&found, 4) {
        assert_ne!(entry.outcome, Outcome::Declaration, "{found:#?}");
    }
    assert!(
        !entries_on(&found, 4).is_empty(),
        "the binding is read: {found:#?}"
    );
}
