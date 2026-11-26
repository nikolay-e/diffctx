//! The `DIFFCTX_DEBUG_OCCURRENCES` writer, through the binary on a real Python
//! repository: one line per occurrence and per (definition, relation). The
//! accounting runs for every answer, so the knob only writes what it found: the
//! answer is the same with and without it. The expectations are read off the
//! sources below, never off a run of diffctx.

mod support;

use serde_json::Value;
use support::families::{Family, Files, Planted, Query, Repo};
use tempfile::TempDir;

const CART: &str = "from shop.pricing import total


def checkout(items):
    # total is computed in pricing
    note = \"the total\"
    return total(items)
";

fn files(entries: &[(&str, &str)]) -> Files {
    entries
        .iter()
        .map(|(path, text)| ((*path).to_string(), (*text).to_string()))
        .collect()
}

const PYPROJECT: (&str, &str) = (
    "pyproject.toml",
    "[project]\nname = \"shop\"\nversion = \"0.1.0\"\n",
);

fn python(base: &[(&str, &str)], change: &[(&str, &str)], definition: &str) -> Repo {
    Repo {
        family: Family::Python,
        base: files(base),
        change: files(change),
        caller: None,
        definition: definition.to_string(),
        symbol: "total".to_string(),
    }
}

/// The answer without the knob, the answer and the lines with it.
fn shadow_of(repo: Repo) -> (Value, Value, Vec<Value>) {
    shadow_under(repo, &[])
}

fn shadow_under(repo: Repo, env: &[(&str, &str)]) -> (Value, Value, Vec<Value>) {
    let planted = Planted::new(repo);
    let plain = planted.ask(Query::Impact, &[]).expect("the plain answer");
    let out = TempDir::new().expect("tempdir");
    let sink = out.path().join("occurrences.jsonl");
    let mut knob = vec![("DIFFCTX_DEBUG_OCCURRENCES", sink.to_str().expect("a path"))];
    knob.extend_from_slice(env);
    let shadow = planted
        .ask(Query::Impact, &knob)
        .expect("the shadow answer");
    let text = std::fs::read_to_string(&sink).expect("the knob writes the file");
    let lines = text
        .lines()
        .map(|line| serde_json::from_str(line).expect("a JSON line"))
        .collect();
    (plain, shadow, lines)
}

fn shadowed(pricing: &str) -> (Value, Value, Vec<Value>) {
    shadow_of(python(
        &[
            PYPROJECT,
            (
                "shop/pricing.py",
                "def total(items):\n    return sum(items)\n",
            ),
            ("shop/cart.py", CART),
        ],
        &[("shop/pricing.py", pricing)],
        "shop/pricing.py",
    ))
}

fn of<'a>(lines: &'a [Value], def: &str, side: &str) -> Vec<&'a Value> {
    lines
        .iter()
        .filter(|line| line["def"] == def && line["side"] == side)
        .collect()
}

#[test]
fn the_shadow_names_each_occurrence_and_leaves_the_answer_alone() {
    let (plain, shadow, lines) = shadowed("def total(items):\n    return sum(items) * 2\n");
    assert_eq!(
        plain["definitions"], shadow["definitions"],
        "the knob changed the answer"
    );
    let ours = of(&lines, "shop/pricing.py::total", "head");
    let occurrences: Vec<&&Value> = ours.iter().filter(|l| l["file"].is_string()).collect();
    for line in &occurrences {
        assert_eq!(line["family"], "python");
        let (start, end) = (
            line["bytes"][0].as_u64().expect("start") as usize,
            line["bytes"][1].as_u64().expect("end") as usize,
        );
        if line["file"] == "shop/cart.py" {
            assert_eq!(&CART[start..end], "total", "a range that spells the name");
        }
    }
    let cart: Vec<u64> = occurrences
        .iter()
        .filter(|l| l["file"] == "shop/cart.py")
        .map(|l| l["line"].as_u64().expect("a line"))
        .collect();
    // The comment on line 5 is no occurrence, the string on line 6 a mention.
    assert!(
        cart.iter().all(|line| matches!(line, 1 | 7)),
        "cart.py: {cart:?}"
    );
    let call = occurrences
        .iter()
        .find(|l| l["file"] == "shop/cart.py" && l["line"] == 7)
        .expect("the call on line 7");
    assert_eq!(call["class"], "code");
    assert_eq!(call["position"], "call/function");
    let declaration = occurrences
        .iter()
        .find(|l| l["file"] == "shop/pricing.py")
        .expect("the declaration");
    assert_eq!(declaration["outcome"], "declaration");
    assert_eq!(declaration["position"], "function_definition/name");
    let pairs: Vec<&&Value> = ours
        .iter()
        .filter(|l| l["occurrences"].is_string())
        .collect();
    for relation in [
        "calls",
        "reads",
        "writes",
        "instantiates",
        "passes_as_value",
        "overrides",
    ] {
        let pair = pairs
            .iter()
            .find(|l| l["relation"] == relation)
            .unwrap_or_else(|| panic!("no pair line for {relation}"));
        let said = pair["occurrences"].as_str().expect("a verdict");
        assert!(matches!(said, "complete" | "partial"), "{said}");
        assert!(pair["today"].is_null(), "one verdict, not two: {pair}");
        let both = pair["occurrences"] == "complete" && pair["gaps"] == serde_json::json!([]);
        assert_eq!(pair["complete"], both, "{pair}");
    }
}

#[test]
fn a_removed_definition_is_accounted_at_the_base_over_the_heads_files() {
    let (_, _, lines) = shadowed("def other(items):\n    return sum(items)\n");
    let removed = of(&lines, "shop/pricing.py::total", "base");
    assert!(
        removed
            .iter()
            .any(|l| l["file"] == "shop/cart.py" && l["line"] == 7 && l["class"] == "code"),
        "the call that may have broken is not accounted: {removed:?}"
    );
}

#[test]
fn a_pair_is_written_for_every_relation_an_unexplained_occurrence_may_be_a_use_through() {
    let (_, _, lines) = shadow_of(python(
        &[
            PYPROJECT,
            ("shop/model.py", "class Price:\n    rate = 1\n"),
            (
                "shop/cart.py",
                "from shop.model import Price\n\n\ndef cost(p: Price):\n    return p\n",
            ),
        ],
        &[("shop/model.py", "class Price:\n    rate = 2\n")],
        "shop/model.py",
    ));
    let pairs: Vec<&Value> = of(&lines, "shop/model.py::Price", "head")
        .into_iter()
        .filter(|l| l["occurrences"].is_string())
        .collect();
    let relations: Vec<&str> = pairs
        .iter()
        .map(|l| l["relation"].as_str().expect("a relation"))
        .collect();
    for relation in ["instantiates", "type_use", "passes_as_value"] {
        assert!(relations.contains(&relation), "{relation}: {relations:?}");
    }
}

#[test]
fn a_line_is_written_once_however_often_the_cell_is_asked() {
    let (_, _, lines) = shadowed("def total(items):\n    return sum(items) * 2\n");
    let mut seen = std::collections::HashSet::new();
    for line in &lines {
        let key = (
            line["def"].to_string(),
            line["side"].to_string(),
            line["relation"].to_string(),
            line["file"].to_string(),
            line["bytes"].to_string(),
        );
        assert!(seen.insert(key), "written twice: {line}");
    }
}

#[test]
fn a_holder_is_complete_only_when_what_it_holds_is_whole_and_unused() {
    let repo = Repo {
        family: Family::TypeScript,
        base: files(&[
            (
                "src/t.ts",
                "export interface Retry {\n  attempt: number;\n}\n",
            ),
            (
                "src/u.ts",
                "export function next(r) {\n  const { attempt } = r;\n  return attempt + 1;\n}\n",
            ),
        ]),
        change: files(&[(
            "src/t.ts",
            "export interface Retry {\n  attempt: string;\n}\n",
        )]),
        caller: None,
        definition: "src/t.ts".to_string(),
        symbol: "Retry".to_string(),
    };
    let (_, _, lines) = shadow_of(repo);
    let holder: Vec<&Value> = of(&lines, "src/t.ts::Retry", "head")
        .into_iter()
        .filter(|l| l["held"].is_u64())
        .collect();
    assert_eq!(holder.len(), 1, "one holder line: {holder:?}");
    assert_eq!(holder[0]["held"], 1);
    // The destructuring of `attempt` is a candidate no row of `Retry` mentions.
    assert_eq!(holder[0]["complete"], false, "{}", holder[0]);
}

#[test]
fn a_holder_is_not_complete_while_what_it_holds_has_a_user() {
    let repo = Repo {
        family: Family::TypeScript,
        base: files(&[
            (
                "src/t.ts",
                "export interface Retry {\n  attempt: number;\n}\n",
            ),
            (
                "src/u.ts",
                "import { Retry } from \"./t\";\n\nexport function next(r: Retry) {\n  return r.attempt + 1;\n}\n",
            ),
        ]),
        change: files(&[(
            "src/t.ts",
            "export interface Retry {\n  attempt: string;\n}\n",
        )]),
        caller: None,
        definition: "src/t.ts".to_string(),
        symbol: "Retry".to_string(),
    };
    let (_, _, lines) = shadow_of(repo);
    let holder: Vec<&Value> = of(&lines, "src/t.ts::Retry", "head")
        .into_iter()
        .filter(|l| l["held"].is_u64())
        .collect();
    assert_eq!(holder.len(), 1, "one holder line: {holder:?}");
    assert_eq!(holder[0]["held"], 1);
    // `r.attempt` reads the held field: a holder is whole only over what nothing uses.
    assert_eq!(holder[0]["complete"], false, "{}", holder[0]);
}

#[test]
fn a_removed_override_is_reached_through_the_member_it_overrode() {
    let repo = Repo {
        family: Family::Java,
        base: files(&[
            (
                "src/a/Notifying.java",
                "package a;\n\nimport java.util.ArrayList;\n\npublic abstract class Notifying<E> extends ArrayList<E> {\n    public abstract void onContentsChanged();\n\n    public boolean add(E e) {\n        onContentsChanged();\n        return super.add(e);\n    }\n}\n",
            ),
            (
                "src/a/Element.java",
                "package a;\n\npublic class Element {\n    Object cache;\n\n    static final class NodeList extends Notifying<String> {\n        public void onContentsChanged() {\n            System.out.println(\"changed\");\n        }\n    }\n}\n",
            ),
        ]),
        change: files(&[(
            "src/a/Element.java",
            "package a;\n\nimport java.util.ArrayList;\n\npublic class Element {\n    Object cache;\n\n    static final class NodeList extends ArrayList<String> {\n    }\n}\n",
        )]),
        caller: None,
        definition: "src/a/Element.java".to_string(),
        symbol: "onContentsChanged".to_string(),
    };
    let (_, _, lines) = shadow_of(repo);
    let removed = of(
        &lines,
        "src/a/Element.java::Element.NodeList.onContentsChanged",
        "base",
    );
    let call = removed
        .iter()
        .find(|l| l["file"] == "src/a/Notifying.java" && l["relation"] == "calls")
        .unwrap_or_else(|| panic!("the dispatching call is not accounted: {removed:?}"));
    assert_eq!(call["outcome"], "candidate", "{call}");
    assert_ne!(call["basis"], "other_definition", "{call}");
}

const LIB: &str = "def f(a):\n    return a\n";
const LIB_CHANGED: &str = "def f(a):\n    return a + 1\n";
const APP: &str = "from lib import f\n\n\ndef main():\n    return f(1)\n";

fn lib_and_app(env: &[(&str, &str)]) -> Vec<Value> {
    let repo = Repo {
        family: Family::Python,
        base: files(&[("lib.py", LIB), ("app.py", APP)]),
        change: files(&[("lib.py", LIB_CHANGED)]),
        caller: None,
        definition: "lib.py".to_string(),
        symbol: "f".to_string(),
    };
    shadow_under(repo, env).2
}

#[test]
fn a_pair_whose_run_was_limited_is_never_complete() {
    let lines = lib_and_app(&[("DIFFCTX_TEST_LOSS", "deadline:1")]);
    let pairs: Vec<&Value> = of(&lines, "lib.py::f", "head")
        .into_iter()
        .filter(|l| l["occurrences"].is_string())
        .collect();
    assert!(!pairs.is_empty(), "no pair lines: {lines:?}");
    for pair in pairs {
        assert_eq!(pair["occurrences"], "complete", "{pair}");
        assert_eq!(
            pair["gaps"],
            serde_json::json!(["the run was limited (deadline)"]),
            "{pair}"
        );
        assert_eq!(pair["complete"], false, "{pair}");
    }
}

#[test]
fn a_definition_holding_nothing_writes_no_holder_line() {
    let lines = lib_and_app(&[]);
    let holders: Vec<&Value> = lines.iter().filter(|l| l.get("holder").is_some()).collect();
    assert!(holders.is_empty(), "{holders:?}");
}

fn reasons_of(repo: Repo, def: &str, relation: &str) -> Vec<String> {
    let (_, _, lines) = shadow_of(repo);
    of(&lines, def, "head")
        .into_iter()
        .find(|l| l["relation"] == relation && l["occurrences"].is_string())
        .unwrap_or_else(|| panic!("no {relation} pair for {def}"))["reasons"]
        .as_array()
        .expect("reasons")
        .iter()
        .map(|r| r.as_str().expect("text").to_string())
        .collect()
}

fn one_file(family: Family, path: &str, base: &str, change: &str, symbol: &str) -> Repo {
    Repo {
        family,
        base: files(&[(path, base)]),
        change: files(&[(path, change)]),
        caller: None,
        definition: path.to_string(),
        symbol: symbol.to_string(),
    }
}

#[test]
fn a_function_a_script_guard_runs_leaves_its_calls_complete() {
    let guarded = |n: u8| {
        format!("def main():\n    return {n}\n\n\nif __name__ == \"__main__\":\n    main()\n")
    };
    let repo = one_file(Family::Python, "cli.py", &guarded(1), &guarded(2), "main");
    assert_eq!(
        reasons_of(repo, "cli.py::main", "calls"),
        Vec::<String>::new()
    );
}

#[test]
fn a_constructor_in_a_file_with_main_is_still_called_implicitly() {
    let unit = |body: &str| {
        format!(
            "__attribute__((constructor)) static void setup(void) {{\n    {body}\n}}\n\nint main(void) {{\n    return 0;\n}}\n"
        )
    };
    let repo = one_file(
        Family::C,
        "a.c",
        &unit("return;"),
        &unit("int x = 1; (void)x;"),
        "setup",
    );
    assert_eq!(
        reasons_of(repo, "a.c::setup", "calls"),
        ["called implicitly (entry)"]
    );
}

#[test]
fn a_protocol_method_is_called_implicitly_by_the_protocol() {
    let class = |n: u8| format!("class Box:\n    def __len__(self):\n        return {n}\n");
    let repo = one_file(Family::Python, "m.py", &class(1), &class(2), "__len__");
    assert_eq!(
        reasons_of(repo, "m.py::Box.__len__", "calls"),
        ["called implicitly (protocol)"]
    );
}

#[test]
fn a_name_on_the_right_of_a_type_alias_is_the_alias_it_defines() {
    let repo = Repo {
        family: Family::TypeScript,
        base: files(&[
            (
                "src/f.ts",
                "export function f(): number {\n  return 1;\n}\n",
            ),
            (
                "src/h.ts",
                "import { f } from \"./f\";\n\nexport type Handler = typeof f;\n",
            ),
        ]),
        change: files(&[(
            "src/f.ts",
            "export function f(): number {\n  return 2;\n}\n",
        )]),
        caller: None,
        definition: "src/f.ts".to_string(),
        symbol: "f".to_string(),
    };
    let (_, _, lines) = shadow_of(repo);
    let aliased: Vec<&Value> = of(&lines, "src/f.ts::f", "head")
        .into_iter()
        .filter(|l| l["file"] == "src/h.ts" && l["line"] == 3 && l["outcome"] == "alias")
        .collect();
    assert_eq!(aliased.len(), 1, "{lines:?}");
    assert_eq!(aliased[0]["basis"], "type_alias");
    assert!(
        aliased[0]["detail"]
            .as_str()
            .expect("detail")
            .contains("Name(\"Handler\")"),
        "{}",
        aliased[0]
    );
}

#[test]
fn a_main_function_is_entered_by_the_runtime_and_leaves_its_calls_complete() {
    let unit = |n: u8| format!("int main(void) {{\n    return {n};\n}}\n");
    let repo = one_file(Family::C, "a.c", &unit(0), &unit(1), "main");
    assert_eq!(reasons_of(repo, "a.c::main", "calls"), Vec::<String>::new());
}

#[test]
fn a_holder_with_a_row_of_its_own_still_open_is_not_complete_whatever_it_holds() {
    let repo = Repo {
        family: Family::TypeScript,
        base: files(&[
            (
                "src/t.ts",
                "export class Base {}\n\nexport class Retry {\n  attempt: number = 1;\n}\n",
            ),
            (
                "src/u.ts",
                "export function build(factory) {\n  return new factory.Retry();\n}\n",
            ),
        ]),
        change: files(&[(
            "src/t.ts",
            "export class Base {}\n\nexport class Retry extends Base {\n  attempt: number = 2;\n}\n",
        )]),
        caller: None,
        definition: "src/t.ts".to_string(),
        symbol: "Retry".to_string(),
    };
    let (_, _, lines) = shadow_of(repo);
    let holder: Vec<&Value> = of(&lines, "src/t.ts::Retry", "head")
        .into_iter()
        .filter(|l| l["held"].is_u64())
        .collect();
    assert_eq!(holder.len(), 1, "one holder line: {lines:?}");
    let own_rows_open = of(&lines, "src/t.ts::Retry", "head")
        .into_iter()
        .any(|l| l["complete"] == false && l["relation"].is_string());
    assert!(own_rows_open, "a candidate stays on the class: {lines:?}");
    assert_eq!(holder[0]["complete"], false, "{}", holder[0]);
}
