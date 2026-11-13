//! The `DIFFCTX_DEBUG_OCCURRENCES` writer, through the binary on a real Python
//! repository: one line per occurrence and per (definition, relation), written
//! while the answer stays what it was. The expectations are read off the sources
//! below, never off a run of diffctx.

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
    let planted = Planted::new(repo);
    let plain = planted.ask(Query::Impact, &[]).expect("the plain answer");
    let out = TempDir::new().expect("tempdir");
    let sink = out.path().join("occurrences.jsonl");
    let knob = [("DIFFCTX_DEBUG_OCCURRENCES", sink.to_str().expect("a path"))];
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
    let pairs: Vec<&&Value> = ours.iter().filter(|l| l["today"].is_string()).collect();
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
        for key in ["today", "occurrences"] {
            let said = pair[key].as_str().expect("a verdict");
            assert!(matches!(said, "complete" | "partial"), "{key}: {said}");
        }
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
    let uncovered = pairs
        .iter()
        .find(|l| l["relation"] == "type_use")
        .expect("type_use");
    assert!(uncovered["today"].is_null(), "{uncovered}");
}
