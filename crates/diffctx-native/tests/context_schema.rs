//! The checked-in JSON Schemas for `diffctx.context.v1` and
//! `diffctx.locate.v1` are generated from the artifact types, never edited by
//! hand. Regenerate with `DIFFCTX_UPDATE_SCHEMAS=1 cargo test --test context_schema`.

use std::path::PathBuf;

fn assert_pinned(file: &str, schema: &serde_json::Value) {
    let generated = serde_json::to_string_pretty(schema).unwrap() + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../schemas")
        .join(file);
    if std::env::var_os("DIFFCTX_UPDATE_SCHEMAS").is_some() {
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
        "diffctx.context.v1.json",
        &_diffctx::render::context_schema(),
    );
}

#[test]
fn the_checked_in_locate_schema_is_the_generated_one() {
    let schema = _diffctx::locate::locate_schema();
    assert_pinned("diffctx.locate.v1.json", &schema);
    let required: Vec<&str> = schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for key in [
        "schema",
        "name",
        "budget_tokens",
        "summary",
        "item_count",
        "items",
    ] {
        assert!(
            required.contains(&key),
            "{key} must be required; got {required:?}"
        );
    }
    assert!(schema["properties"]["coverage"].is_object());
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
    for key in ["schema", "name", "type", "fragment_count", "fragments"] {
        assert!(
            required.contains(&key),
            "{key} must be required; got {required:?}"
        );
    }
    assert!(schema["properties"]["provenance"].is_object());
    assert!(schema["properties"]["coverage"].is_object());
}
