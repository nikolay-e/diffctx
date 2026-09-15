//! The checked-in JSON Schema for `diffctx.context.v1` is generated from the
//! artifact type, never edited by hand. Regenerate with
//! `DIFFCTX_UPDATE_SCHEMAS=1 cargo test --test context_schema`.

use std::path::PathBuf;

fn schema_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/diffctx.context.v1.json")
}

#[test]
fn the_checked_in_schema_is_the_generated_one() {
    let generated =
        serde_json::to_string_pretty(&_diffctx::render::context_schema()).unwrap() + "\n";
    let path = schema_path();
    if std::env::var_os("DIFFCTX_UPDATE_SCHEMAS").is_some() {
        std::fs::write(&path, &generated).expect("write schema");
    }
    let on_disk = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        on_disk, generated,
        "schemas/diffctx.context.v1.json drifted from the artifact type; regenerate with DIFFCTX_UPDATE_SCHEMAS=1"
    );
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
