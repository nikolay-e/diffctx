//! What kind of change a file carries, stated explicitly so the selection
//! policy can rank evidence by it and the artifact can say what it decided.
//!
//! A range that mixes an image updater's one-line `tag:` bumps with a
//! hand-written manifest rewrite used to lose the manifests: every core
//! competed on token cost and the cheap bumps won (#263). The class is a
//! priority, never a filter — a file of any class stays in the inventory —
//! and `Unknown` is treated like `Content`, because a wrong "mechanical"
//! verdict on a real one-line fix would cost the reader the fix.

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashMap;
use schemars::JsonSchema;
use serde::Serialize;

use crate::types::DiffHunk;

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// Hand-written, multi-line or otherwise substantive.
    Content,
    /// Not classified with confidence; ranked with `Content`.
    Unknown,
    /// One-line pin/tag/version/digest edits of the kind an updater writes.
    Mechanical,
    /// A file whose header says it is generated.
    Generated,
}

impl ChangeClass {
    /// Lower sorts first when evidence competes for budget.
    pub fn priority(self) -> u8 {
        match self {
            ChangeClass::Content | ChangeClass::Unknown => 0,
            ChangeClass::Mechanical => 1,
            ChangeClass::Generated => 2,
        }
    }
}

/// A single-line change that names a version, a tag, a digest or a hash.
static MECHANICAL_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?ix)
        (^\s*(tag|version|rev|ref|sha|digest|image|newTag|newName|commit|checksum|integrity)\s*[:=])
        | (sha256:[0-9a-f]{12,})
        | (\bv?\d+\.\d+\.\d+(?:[-+.][0-9A-Za-z.]+)?\b)
        | (\b[0-9a-f]{7,64}\b)
        "#,
    )
    .expect("mechanical line regex")
});

pub fn classify(
    hunks: &[&DiffHunk],
    changed_lines: &[String],
    generated: bool,
) -> (ChangeClass, &'static str) {
    if generated {
        return (ChangeClass::Generated, "generated-file header");
    }
    if hunks.is_empty() {
        return (ChangeClass::Unknown, "no hunks");
    }
    let single_line = hunks.iter().all(|h| h.new_len <= 1 && h.old_len <= 1);
    if !single_line {
        return (ChangeClass::Content, "multi-line hunks");
    }
    if changed_lines.is_empty() {
        return (ChangeClass::Unknown, "single-line hunks, no diff text");
    }
    if changed_lines.iter().all(|l| MECHANICAL_LINE_RE.is_match(l)) {
        (
            ChangeClass::Mechanical,
            "single-line version/tag/digest edits",
        )
    } else {
        (ChangeClass::Unknown, "single-line hunks")
    }
}

/// The added and removed lines of a unified diff, per `b/` path — enough to
/// tell a version bump from a one-line fix without a second git call.
pub fn changed_lines_by_file(diff_text: &str) -> FxHashMap<String, Vec<String>> {
    let mut by_file: FxHashMap<String, Vec<String>> = FxHashMap::default();
    let mut current: Option<String> = None;
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let path = rest
                .strip_prefix("b/")
                .unwrap_or(rest)
                .split('\t')
                .next()
                .unwrap_or("")
                .to_string();
            current = if path == "/dev/null" {
                None
            } else {
                Some(path)
            };
            continue;
        }
        if line.starts_with("--- ") || line.starts_with("diff ") || line.starts_with("@@") {
            continue;
        }
        let Some(path) = current.as_ref() else {
            continue;
        };
        if (line.starts_with('+') || line.starts_with('-')) && line.len() > 1 {
            by_file
                .entry(path.clone())
                .or_default()
                .push(line[1..].to_string());
        }
    }
    by_file
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn hunk(new_len: u32, old_len: u32) -> DiffHunk {
        DiffHunk {
            path: Arc::from("x"),
            new_start: 1,
            new_len,
            old_start: 1,
            old_len,
        }
    }

    #[test]
    fn a_tag_bump_is_mechanical_and_a_one_line_fix_is_unknown() {
        let h = hunk(1, 1);
        let bump = vec![
            "    tag: main-abc1234".into(),
            "    tag: main-def5678".into(),
        ];
        assert_eq!(classify(&[&h], &bump, false).0, ChangeClass::Mechanical);
        let fix = vec!["    return x + 1".into(), "    return x + 2".into()];
        assert_eq!(classify(&[&h], &fix, false).0, ChangeClass::Unknown);
        let digest = vec!["image: r/app@sha256:0123456789abcdef".into()];
        assert_eq!(classify(&[&h], &digest, false).0, ChangeClass::Mechanical);
    }

    #[test]
    fn multi_line_hunks_are_content_and_generated_wins() {
        let h = hunk(5, 2);
        assert_eq!(classify(&[&h], &[], false).0, ChangeClass::Content);
        assert_eq!(classify(&[&h], &[], true).0, ChangeClass::Generated);
        assert!(ChangeClass::Unknown.priority() == ChangeClass::Content.priority());
        assert!(ChangeClass::Mechanical.priority() > ChangeClass::Content.priority());
    }

    #[test]
    fn changed_lines_are_grouped_by_the_new_side_path() {
        let diff = "diff --git a/a.yaml b/a.yaml\n--- a/a.yaml\n+++ b/a.yaml\n@@ -1 +1 @@\n-tag: 1\n+tag: 2\ndiff --git a/gone b/gone\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        let lines = changed_lines_by_file(diff);
        assert_eq!(
            lines["a.yaml"],
            vec!["tag: 1".to_string(), "tag: 2".to_string()]
        );
        assert!(!lines.contains_key("/dev/null"));
    }
}
