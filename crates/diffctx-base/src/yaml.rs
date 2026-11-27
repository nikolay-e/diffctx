//! YAML text as JSON, the one reader of YAML the workspace has.
//!
//! Repository files reach it untrusted, so it is a safe-Rust parser (`saphyr`) behind one
//! bridge: a document becomes a [`serde_json::Value`], and typed readers go through that value.
//! The core schema decides scalars (`yes` and `on` are strings, and so is a zero-padded number
//! such as an account id), a mapping key is its scalar's text, a duplicate key, a second
//! document or a non-finite float is an error, and a tag is dropped (its node is read untagged).

use saphyr::{Scalar, Yaml, YamlLoader};
use saphyr_parser::{Parser, ScalarStyle};
use serde::de::DeserializeOwned;
use serde_json::{Map, Number, Value};

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct YamlError(String);

/// The document in `text`: null for an empty one.
pub fn to_value(text: &str) -> Result<Value, YamlError> {
    let mut loader = YamlLoader::<Yaml>::default();
    loader.early_parse(false);
    Parser::new_from_str(text)
        .load(&mut loader, true)
        .map_err(|e| YamlError(e.to_string()))?;
    if let Some(e) = loader.error() {
        return Err(YamlError(e.to_string()));
    }
    let mut documents = loader.into_documents();
    match documents.len() {
        0 => Ok(Value::Null),
        1 => json(&documents.remove(0)),
        _ => Err(YamlError("more than one document".to_string())),
    }
}

/// The document in `text` as a `T`.
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, YamlError> {
    serde_json::from_value(to_value(text)?).map_err(|e| YamlError(e.to_string()))
}

fn json(node: &Yaml) -> Result<Value, YamlError> {
    match node {
        Yaml::Value(scalar) => scalar_json(scalar),
        Yaml::Sequence(items) => items
            .iter()
            .map(json)
            .collect::<Result<_, _>>()
            .map(Value::Array),
        Yaml::Mapping(entries) => {
            let mut object = Map::new();
            for (key, value) in entries {
                object.insert(key_text(key)?, json(value)?);
            }
            Ok(Value::Object(object))
        }
        Yaml::Tagged(_, inner) => json(inner),
        Yaml::Representation(text, style, tag) => {
            if *style == ScalarStyle::Plain && tag.is_none() && zero_padded(text) {
                return Ok(Value::String(text.to_string()));
            }
            let scalar = Scalar::parse_from_cow_and_metadata(text.clone(), *style, tag.as_ref())
                .ok_or_else(|| YamlError(format!("{text} does not fit its tag")))?;
            scalar_json(&scalar)
        }
        Yaml::Alias(_) | Yaml::BadValue => Err(YamlError("an unresolved node".to_string())),
    }
}

fn zero_padded(text: &str) -> bool {
    let digits = text.strip_prefix(['+', '-']).unwrap_or(text);
    digits.len() > 1 && digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit())
}

fn scalar_json(scalar: &Scalar) -> Result<Value, YamlError> {
    Ok(match scalar {
        Scalar::Null => Value::Null,
        Scalar::Boolean(b) => Value::Bool(*b),
        Scalar::Integer(i) => Value::Number((*i).into()),
        Scalar::FloatingPoint(f) => Value::Number(
            Number::from_f64(f.0).ok_or_else(|| YamlError(format!("non-finite float {f}")))?,
        ),
        Scalar::String(s) => Value::String(s.to_string()),
    })
}

fn key_text(key: &Yaml) -> Result<String, YamlError> {
    match json(key)? {
        Value::String(s) => Ok(s),
        Value::Array(_) | Value::Object(_) => {
            Err(YamlError("a collection as a mapping key".to_string()))
        }
        scalar => Ok(scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_follow_the_core_schema_and_keep_account_ids_as_text() {
        let text = "a: yes\nb: on\nc: 0755\nd: 099720109477\ne: 1_000\nf: 0x1F\ng: 1.5\nh: '12'\ni: ~\nj: true\n";
        assert_eq!(
            to_value(text).unwrap(),
            json!({"a": "yes", "b": "on", "c": "0755", "d": "099720109477", "e": "1_000",
                   "f": 31, "g": 1.5, "h": "12", "i": null, "j": true})
        );
    }

    #[test]
    fn a_key_is_its_scalar_text_and_a_merge_key_stays_a_key() {
        let text = "1: a\ntrue: b\nbase: &x {k: 1}\nd:\n  <<: *x\n";
        assert_eq!(
            to_value(text).unwrap(),
            json!({"1": "a", "true": "b", "base": {"k": 1}, "d": {"<<": {"k": 1}}})
        );
    }

    #[test]
    fn a_tag_is_dropped_and_an_empty_document_is_null() {
        assert_eq!(to_value("a: !foo bar\n").unwrap(), json!({"a": "bar"}));
        assert_eq!(to_value("# nothing\n").unwrap(), Value::Null);
    }

    #[test]
    fn what_the_bridge_cannot_say_is_an_error() {
        for text in [
            "a: 1\na: 2\n",
            "a: 1\n---\nb: 2\n",
            "a: .inf\n",
            "? [a, b]\n: c\n",
            "a: !!int nope\n",
        ] {
            assert!(to_value(text).is_err(), "{text:?}");
        }
    }

    #[test]
    fn a_typed_reader_goes_through_the_value() {
        #[derive(serde::Deserialize)]
        struct Case {
            name: String,
            #[serde(default)]
            tags: Vec<String>,
        }
        let case: Case = from_str("name: x\ntags: [a, b]\n").unwrap();
        assert_eq!((case.name.as_str(), case.tags.len()), ("x", 2));
        assert!(from_str::<Case>("name: x\ntags:\n").is_err());
    }
}
