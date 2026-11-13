//! The checked-in JSON Schemas for `diffctx.context.v2`, `diffctx.impact.v2` and
//! `diffctx.locate.v2` are generated from the artifact types, never edited by
//! hand. Regenerate with `DIFFCTX_UPDATE_SCHEMAS=1 cargo test --test context_schema`.

use std::path::PathBuf;

mod support;

fn assert_pinned(file: &str, schema: &serde_json::Value) {
    let generated = serde_json::to_string_pretty(schema).unwrap() + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(file);
    if std::env::var_os("DIFFCTX_UPDATE_SCHEMAS").is_some() {
        std::fs::create_dir_all(path.parent().expect("schemas directory")).expect("mkdir");
        std::fs::write(&path, &generated).expect("write schema");
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        on_disk, generated,
        "schemas/{file} drifted from the artifact type; regenerate with DIFFCTX_UPDATE_SCHEMAS=1"
    );
}

#[test]
fn the_checked_in_schema_is_the_generated_one() {
    assert_pinned(
        "diffctx.context.v2.json",
        &_diffctx::render::context_schema(),
    );
}

#[test]
fn the_checked_in_locate_schema_is_the_generated_one() {
    let schema = _diffctx::locate::locate_schema();
    assert_pinned("diffctx.locate.v2.json", &schema);
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for key in ["schema", "name", "range", "fit", "summary", "items"] {
        assert!(
            required.contains(&key),
            "{key} must be required; got {required:?}"
        );
    }
    assert!(schema["properties"]["coverage"].is_object());
    // A count is never stored beside the list it counts, and what v1 kept under
    // other names is under the impact answer's.
    for gone in [
        "item_count",
        "overflow_count",
        "budget_tokens",
        "budget_needed",
        "commit_messages",
        "changed_files",
    ] {
        assert!(
            schema["properties"].get(gone).is_none(),
            "`{gone}` survived into locate v2"
        );
    }
    let coverage = defs(&schema, "Coverage").to_string();
    assert!(!coverage.contains("confidence"), "{coverage}");
}

#[test]
fn the_checked_in_impact_schema_is_the_generated_one() {
    let schema = _diffctx::impact::impact_schema();
    assert_pinned("diffctx.impact.v2.json", &schema);
    // What every v2 impact document carries, whatever the change: who and what
    // was asked, the definitions, what was not covered, and what the cap did. A
    // list or container with nothing in it is left out of the JSON, so the
    // sections that hold one are optional, as are `symbol`, `spelled` and
    // `omitted`, which come only when they apply.
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let always = [
        "schema",
        "name",
        "provenance",
        "range",
        "definitions",
        "tests",
        "coverage",
        "empty",
        "summary",
        "fit",
    ];
    let mut got = required.clone();
    got.sort_unstable();
    let mut want = always.to_vec();
    want.sort_unstable();
    assert_eq!(got, want, "the keys every v2 document carries");
    for optional in [
        "inventory",
        "unlisted",
        "contracts",
        "stale",
        "named",
        "deleted_modules",
        "symbol",
        "spelled",
        "omitted",
    ] {
        assert!(
            schema["properties"][optional].is_object(),
            "{optional} is a property"
        );
        assert!(!required.contains(&optional), "{optional} is optional");
    }
}

#[test]
fn the_schema_names_the_artifact_and_its_required_keys() {
    let schema = _diffctx::render::context_schema();
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for key in ["schema", "name", "type", "range", "fragments"] {
        assert!(
            required.contains(&key),
            "{key} must be required; got {required:?}"
        );
    }
    assert!(schema["properties"]["provenance"].is_object());
    assert!(schema["properties"]["coverage"].is_object());
    for section in ["inventory", "fit", "changes"] {
        assert!(
            schema["properties"][section].is_object(),
            "no `{section}` in pack v2"
        );
    }
    for gone in [
        "fragment_count",
        "budget_needed",
        "commit_messages",
        "changed_files",
        "policy_excluded_count",
    ] {
        assert!(
            schema["properties"].get(gone).is_none(),
            "`{gone}` survived into pack v2"
        );
    }
}

// ---- diffctx.impact.v2 (not wired into the doors yet: built from v1 by `from_v1`)

fn defs<'a>(schema: &'a serde_json::Value, prefix: &str) -> &'a serde_json::Value {
    let all = schema["$defs"].as_object().expect("$defs");
    all.iter()
        .find(|(name, _)| name.as_str() == prefix || name.starts_with(&format!("{prefix}_for_")))
        .map(|(_, v)| v)
        .unwrap_or_else(|| panic!("no definition named {prefix}: {:?}", all.keys()))
}

fn required<'a>(def: &'a serde_json::Value) -> Vec<&'a str> {
    def["required"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default()
}

/// The shape the W6-A design gives: the sections, the closed vocabularies, and
/// what travels together.
#[test]
fn the_v2_impact_schema_has_the_shape_the_design_gives() {
    let schema = _diffctx::answer::v2::impact_v2_schema();
    let top = schema["properties"].as_object().expect("properties");
    for section in [
        "schema",
        "name",
        "provenance",
        "range",
        "inventory",
        "definitions",
        "unlisted",
        "contracts",
        "stale",
        "named",
        "deleted_modules",
        "tests",
        "coverage",
        "empty",
        "summary",
        "omitted",
        "fit",
    ] {
        assert!(top.contains_key(section), "no `{section}` in v2");
    }
    // v1's sentinel entry and its scattered counters are gone.
    for gone in [
        "changed",
        "truncated",
        "callers_omitted",
        "budget_needed",
        "limits",
    ] {
        assert!(!top.contains_key(gone), "`{gone}` survived into v2");
    }
    assert_eq!(required(defs(&schema, "Count")), ["n", "exact"]);
    assert_eq!(required(defs(&schema, "Listing")), ["total", "items"]);
    let guard = defs(&schema, "GuardState").to_string();
    for state in [
        "linked",
        "file_only",
        "none",
        "registered",
        "entry_point",
        "unread",
        "no_tests",
        "not_established",
    ] {
        assert!(guard.contains(state), "guard state {state}");
    }
    let gap = defs(&schema, "Gap").to_string();
    for kind in [
        "unlanded",
        "stopped",
        "dispatched",
        "ambiguous",
        "not_modelled",
        "capped",
        "unaccounted",
        "templated",
        "source_flag",
        "unread_directories",
    ] {
        assert!(gap.contains(kind), "gap kind {kind}");
    }
    let row = required(defs(&schema, "Row"));
    for field in ["class", "standing", "guard"] {
        assert!(row.contains(&field), "row field {field}");
    }
    assert!(
        !row.contains(&"kind"),
        "a row states its kind only from a precise cell"
    );
}

/// Values built by hand serialize to what the schema says: absent beats empty, a
/// count states its bound, a listing derives what it omits.
#[test]
fn v2_values_serialize_to_the_documented_shape() {
    use _diffctx::answer::v2::{Count, DefRef, Gap, Listing, ShedRows};
    let listing = Listing::of(vec!["a.py".to_string()], 2);
    assert_eq!(listing.omitted(), 2);
    assert_eq!(
        serde_json::to_value(&listing).unwrap(),
        serde_json::json!({"total": 3, "items": ["a.py"]})
    );
    assert_eq!(
        serde_json::to_value(Count { n: 4, exact: false }).unwrap(),
        serde_json::json!({"n": 4, "exact": false})
    );
    assert_eq!(
        serde_json::to_value(Gap::Unlanded {
            name: "f".into(),
            uses: 3
        })
        .unwrap(),
        serde_json::json!({"kind": "unlanded", "name": "f", "uses": 3})
    );
    assert_eq!(
        serde_json::to_value(ShedRows {
            resolved: 2,
            untested: 1,
            ..ShedRows::default()
        })
        .unwrap(),
        serde_json::json!({"resolved": 2, "untested": 1})
    );
    for (text, def) in [
        (
            "src/a.py::f (3-9)",
            DefRef {
                path: "src/a.py".into(),
                symbol: Some("f".into()),
                lines: "3-9".into(),
            },
        ),
        (
            "src/a.py:3-9",
            DefRef {
                path: "src/a.py".into(),
                symbol: None,
                lines: "3-9".into(),
            },
        ),
        (
            "src/a.py::Cls.m",
            DefRef {
                path: "src/a.py".into(),
                symbol: Some("Cls.m".into()),
                lines: String::new(),
            },
        ),
    ] {
        assert_eq!(DefRef::parse(text), def, "{text}");
    }
}

/// The map from a real v1 answer: the caller is a row with one guard, a changed
/// caller is a named user and not a row, the search says what it covered, and the
/// totals the header would print are the summary.
#[test]
fn a_v1_answer_maps_to_v2_field_by_field() {
    let tmp = support::repo(
        &[
            ("shop/__init__.py", ""),
            (
                "shop/pricing.py",
                "def total(items):\n    return sum(items)\n",
            ),
            (
                "shop/checkout.py",
                "from shop.pricing import total\n\n\ndef charge(cart):\n    return total(cart)\n",
            ),
            (
                "tests/test_checkout.py",
                "from shop.checkout import charge\n\n\ndef test_charge():\n    assert charge([1]) == 1\n",
            ),
        ],
        &[(
            "shop/pricing.py",
            "def total(items):\n    return sum(items) + 0\n",
        )],
    );
    let v1 = _diffctx::pipeline::build_diff_context_impact(
        tmp.path(),
        Some("HEAD~1..HEAD"),
        &[],
        _diffctx::config::limits::DEFAULT_PPR_ALPHA,
        _diffctx::mode::ScoringMode::from_str(_diffctx::config::limits::DEFAULT_SCORING)
            .expect("the default scoring"),
        _diffctx::config::limits::DEFAULT_PIPELINE_TIMEOUT_SECONDS,
    )
    .expect("impact");
    let v2 = _diffctx::answer::v2::ImpactV2::from_v1(&v1, 2);
    let json = serde_json::to_value(&v2).unwrap();
    assert_eq!(json["schema"], "diffctx.impact.v2");
    assert_eq!(json["range"]["kind"], "committed");
    assert_eq!(json["inventory"]["changed"]["total"], 1);
    let total = json["definitions"]
        .as_array()
        .and_then(|d| d.iter().find(|d| d["symbol"] == "total"))
        .unwrap_or_else(|| panic!("`total` is not a definition: {json:#}"));
    assert_eq!(total["change"]["kind"], "edited");
    let row = &total["rows"][0];
    assert_eq!(row["path"], "shop/checkout.py");
    assert_eq!(row["standing"], "resolved");
    assert_eq!(row["class"], "call");
    assert_eq!(row["guard"]["state"], "linked");
    assert_eq!(row["guard"]["test"], "tests/test_checkout.py");
    assert_eq!(total["coverage"]["calls"]["complete"], true);
    assert_eq!(
        json["summary"]["callers"],
        serde_json::json!({"n": 1, "exact": true})
    );
    assert_eq!(json["summary"]["definitions"], 1);
}

/// The builders fill the ledger where rows are shed: every definition's shed rows
/// are what it lost, the rows of the definitions shed whole are named in the
/// omitted entry, and together they are every row the cap took. What the range did
/// to a definition comes from the cell, delta included.
#[test]
fn the_shed_ledger_is_filled_where_rows_are_shed() {
    use _diffctx::answer::v2::{ChangeKind, ImpactV2};
    let api = |k: u32| -> String {
        (0..15)
            .map(|i| format!("def op_{i:02}(x):\n    return x + {i} + {k}\n\n\n"))
            .collect()
    };
    let callers: Vec<(String, String)> = (0..15)
        .map(|i| {
            (
                format!("client/c_{i:02}.py"),
                format!(
                    "from lib.api import op_{i:02}\n\n\ndef run_{i:02}():\n    return op_{i:02}(1)\n"
                ),
            )
        })
        .collect();
    let (api0, api1) = (api(0), api(1));
    let mut base: Vec<(&str, &str)> = vec![("lib/__init__.py", ""), ("lib/api.py", &api0)];
    base.extend(callers.iter().map(|(p, t)| (p.as_str(), t.as_str())));
    let tmp = support::repo(&base, &[("lib/api.py", &api1)]);
    let mut v1 = _diffctx::pipeline::build_diff_context_impact(
        tmp.path(),
        Some("HEAD~1..HEAD"),
        &[],
        _diffctx::config::limits::DEFAULT_PPR_ALPHA,
        _diffctx::mode::ScoringMode::from_str(_diffctx::config::limits::DEFAULT_SCORING)
            .expect("the default scoring"),
        _diffctx::config::limits::DEFAULT_PIPELINE_TIMEOUT_SECONDS,
    )
    .expect("impact");
    // What the cell read of each edit, before the cap drops the definitions with
    // nothing left to say.
    let whole = ImpactV2::from_v1(&v1, 2);
    let edited = whole
        .definitions
        .iter()
        .find(|d| d.change.kind == ChangeKind::Edited)
        .expect("an edited definition");
    assert!(
        edited.change.delta.is_some_and(|d| d.body && !d.signature),
        "{:?}",
        edited.change
    );
    _diffctx::impact::deliver(&mut v1, false, Some(400), true);
    assert!(v1.truncated > 0, "the budget must shed something");
    for c in v1.changed.iter().filter(|c| c.kind != "omitted") {
        assert_eq!(
            c.shed.total(),
            c.callers_omitted,
            "{:?}: the ledger and the count of what it lost",
            c.symbol
        );
    }
    let v2 = ImpactV2::from_v1(&v1, 2);
    let listed: usize = v2.definitions.iter().map(|d| d.shed.total()).sum();
    // The names the omitted entry keeps are trimmed to fit; its ledger is not.
    let named: usize = v2.omitted.as_ref().map_or(0, |o| o.shed.total());
    assert_eq!(
        listed + named,
        v1.truncated,
        "every shed row is in the ledger"
    );
}
