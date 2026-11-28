//! The changed definitions an impact answer does not list as entries, each for the
//! reason it is apart.

use schemars::JsonSchema;
use serde::Serialize;

use super::{DefRef, Listing};

#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Entered {
    pub def: DefRef,
    pub role: String,
}

/// A definition listed apart from the entries, with the role that gives it an
/// implicit caller when it has one (a runner, a registry): the role is why a missing
/// test link is a finding there.
#[derive(Serialize, JsonSchema, Clone, Debug)]
pub struct Unreached {
    #[serde(flatten)]
    pub def: DefRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// v1 wrote a definition and its role in one string: `path::symbol (lines) — role`.
pub(super) fn split_role(text: &str) -> (&str, Option<String>) {
    match text.split_once(" \u{2014} ") {
        Some((def, role)) => (def, Some(role.to_string())),
        None => (text, None),
    }
}

impl Unreached {
    pub(super) fn parse(text: &str) -> Self {
        let (def, role) = split_role(text);
        Self {
            def: DefRef::parse(def),
            role,
        }
    }
}

/// Changed definitions the answer does not list as entries, each for a reason.
#[derive(Serialize, JsonSchema, Clone, Debug, Default)]
pub struct Unlisted {
    /// Complete searches found no caller: the absence claim.
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub no_caller: Listing<Unreached>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub called_in_diff: Listing<Unreached>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub entered_by: Listing<Entered>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub layout_only: Listing<DefRef>,
}

impl Unlisted {
    pub fn is_empty(&self) -> bool {
        self.no_caller.is_empty()
            && self.called_in_diff.is_empty()
            && self.entered_by.is_empty()
            && self.layout_only.is_empty()
    }
}
