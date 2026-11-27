"""A truth pair is listed when the answer names its site, whatever key the answer files the caller under."""

from answer import V2, parse_answer
from compare import compare, make_resolver
from model import Definition, Relation


def definition(path, name, start, end, kind="function"):
    return Definition(path, kind, name, (), start, end, start - 1, 4, "()", name, name)


def resolver(*definitions):
    by_path: dict[str, list[Definition]] = {}
    for d in definitions:
        by_path.setdefault(d.path, []).append(d)
    return make_resolver(lambda path: by_path.get(path, []))


def row(path, symbol=None, lines="1-1", sites=(), **extra):
    found = {"path": path, "lines": lines, "sites": list(sites), "class": "call", "standing": "resolved", **extra}
    return {**found, "symbol": symbol} if symbol else found


def answer_of(target, rows=(), also_changed=(), resolve=None):
    path, name, lines = target
    entry = {
        "path": path,
        "symbol": name,
        "lines": lines,
        "rows": list(rows),
        "also_changed": [{"path": p, "lines": span} for p, span in also_changed],
        "coverage": {"calls": {"complete": True}, "reads": {"complete": True}},
        "shed": {},
    }
    doc = {"schema": V2, "definitions": [entry], "coverage": {"scope": {"complete": True}, "limits": []}, "summary": {"shed": 0}}
    return parse_answer(doc, resolve or resolver())


def scored(target_key, truth, answer, changed=None):
    cells, details = compare(target_key, truth, answer, changed)
    return {k: sum(c[k] for c in cells) for k in ("truth", "tp", "fn", "fp", "incomplete_complete")}, details


TARGET = definition("lib/utils.js", "escapeHtml", 1, 5)
TARGET_ENTRY = ("lib/utils.js", "escapeHtml", "1-5")


def pair(from_key, site, kind="calls", target=TARGET):
    return Relation(from_key, target.key, kind, site)


def test_a_use_the_answer_files_under_a_local_binding_is_listed_at_its_site_and_is_no_false_positive():
    # the oracle keys a use inside a closure to its enclosing definition (here the module); the engine names the binding
    found = answer_of(TARGET_ENTRY, [row("examples/map.js", "list", "40-50", [45])])

    counts, details = scored(TARGET, [pair("examples/map.js::<module>", 45)], found)

    assert counts == {"truth": 1, "tp": 1, "fn": 0, "fp": 0, "incomplete_complete": 0}
    assert details[0]["verdict"] == "tp"


def test_a_row_with_no_symbol_is_filed_under_the_definition_holding_its_lines():
    caller = definition("help.go", "HelpPrinterCustom", 40, 50)
    found = answer_of(TARGET_ENTRY, [row("help.go", lines="42-42")], resolve=resolver(caller))

    counts, _ = scored(TARGET, [pair(caller.key, 42)], found)

    assert (counts["tp"], counts["fn"], counts["fp"]) == (1, 0, 0)


def test_one_row_that_names_two_sites_lists_the_truth_pairs_of_both_nested_definitions():
    found = answer_of(TARGET_ENTRY, [row("source/create.ts", "got", "20-90", [66, 80])])

    counts, _ = scored(
        TARGET,
        [pair("source/create.ts::create.got.lastHandler", 66), pair("source/create.ts::create.got.iterateHandlers", 80)],
        found,
    )

    assert (counts["truth"], counts["tp"], counts["fn"], counts["fp"], counts["incomplete_complete"]) == (2, 2, 0, 0, 0)


def test_a_changed_definition_the_answer_lists_as_also_changed_names_the_changed_lines_inside_it():
    found = answer_of(TARGET_ENTRY, also_changed=[("src/css.ts", "190-196")])

    counts, _ = scored(TARGET, [pair("src/css.ts::DefaultContextType.Style", 192, "type_use")], found, {"src/css.ts": {191, 192}})

    assert (counts["tp"], counts["fn"], counts["fp"]) == (1, 0, 0)


def test_an_unchanged_use_inside_a_big_also_changed_definition_is_a_miss_the_answer_calls_complete():
    # a class spanning 2-400 with a changed line 3 and an unchanged real call at 300: the engine lists the class whole
    found = answer_of(TARGET_ENTRY, also_changed=[("src/b.ts", "2-400")])
    truth = [pair("src/b.ts::Svc.limit", 3), pair("src/b.ts::Svc.run", 300)]

    counts, details = scored(TARGET, truth, found, {"src/b.ts": {3}})

    assert (counts["truth"], counts["tp"], counts["fn"], counts["incomplete_complete"]) == (2, 1, 1, 1)
    assert [d["from"] for d in details if d["incomplete_complete"]] == ["src/b.ts::Svc.run"]


def test_an_also_changed_definition_names_nothing_when_the_diff_gives_no_changed_lines():
    found = answer_of(TARGET_ENTRY, also_changed=[("src/b.ts", "2-400")])

    counts, _ = scored(TARGET, [pair("src/b.ts::Svc.run", 300)], found)

    assert (counts["tp"], counts["fn"]) == (0, 1)


def test_a_row_that_stands_for_a_truth_pair_under_another_key_is_a_false_positive_at_its_sites_the_truth_lacks():
    # one row per call, all filed under the local `list`: the oracle has the use at 45 only (the others go through the binding)
    found = answer_of(TARGET_ENTRY, [row("examples/map.js", "list", f"{site}-{site}", [site]) for site in (45, 55, 56)])

    counts, details = scored(TARGET, [pair("examples/map.js::<module>", 45)], found)

    assert (counts["tp"], counts["fp"]) == (1, 1)
    assert [d["verdict"] for d in details] == ["tp", "fp"]


def test_a_site_the_answer_names_in_another_file_or_not_at_all_is_still_a_miss():
    found = answer_of(TARGET_ENTRY, [row("a.js", "g", "1-9", [45]), row("b.js", "h", "1-9", [46])])

    counts, _ = scored(TARGET, [pair("b.js::<module>", 45)], found)

    assert (counts["tp"], counts["fn"], counts["incomplete_complete"]) == (0, 1, 1)


def test_a_static_import_is_scored_only_where_the_file_has_no_other_row_for_the_target():
    imported = pair("src/ssg.ts::<module>", 11, "imports")
    used = answer_of(TARGET_ENTRY, [row("src/ssg.ts", "build", "60-80", [69])])
    unused = answer_of(TARGET_ENTRY, [row("src/other.ts", "build", "60-80", [69])])

    assert scored(TARGET, [imported], used)[0]["truth"] == 0
    counts, _ = scored(TARGET, [imported], unused)
    assert (counts["truth"], counts["fn"], counts["incomplete_complete"]) == (1, 1, 1)


def test_an_impl_header_the_answer_never_names_stays_an_incomplete_complete_beside_the_uses_it_lists():
    # hyperfine 2443383fc8: `impl MarkupFormatter for OrgmodeFormatter` (orgmode.rs:12) is a reference to the type, in no definition
    target = definition("src/export/orgmode.rs", "OrgmodeFormatter", 10, 10, "struct")
    uses = [
        row("src/export/orgmode.rs", "serialize", "34-46", [43], **{"class": "reference"}),
        row("src/export/orgmode.rs", "test_orgmode_formatter_table_data", "51-59", [52], **{"class": "reference"}),
        row("src/export/orgmode.rs", "test_orgmode_formatter_table_line", "63-71", [64], **{"class": "reference"}),
    ]
    found = answer_of(("src/export/orgmode.rs", "OrgmodeFormatter", "10-10"), uses)
    truth = [
        pair("src/export/orgmode.rs::<module>", 12, "type_use", target),
        *(
            pair(f"src/export/orgmode.rs::{name}", site, "type_use", target)
            for name, site in (
                ("serialize", 43),
                ("test_orgmode_formatter_table_data", 52),
                ("test_orgmode_formatter_table_line", 64),
            )
        ),
    ]

    counts, details = scored(target, truth, found)

    assert (counts["tp"], counts["fn"], counts["incomplete_complete"]) == (3, 1, 1)
    assert [d["from"] for d in details if d["incomplete_complete"]] == ["src/export/orgmode.rs::<module>"]


def test_a_struct_that_implements_a_changed_interface_stays_an_incomplete_complete_beside_the_uses_the_answer_lists():
    # cli 596e7c5a1a: `mapSource` (value_source.go:175) implements the new `MapSource` interface; the answer names only uses of the type
    interface = definition("value_source.go", "MapSource", 30, 38, "interface")
    found = answer_of(
        ("value_source.go", "MapSource", "30-38"),
        also_changed=[("value_source.go", "180"), ("value_source.go", "226"), ("value_source.go", "229")],
    )

    counts, details = scored(interface, [pair("value_source.go::mapSource", 175, "overrides", interface)], found)

    assert (counts["tp"], counts["fn"], counts["incomplete_complete"]) == (0, 1, 1)
    assert details[0]["incomplete_complete"]
