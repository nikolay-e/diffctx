//! Which lines of which files a change touched, for the one reader that has
//! to tell a changed line from an untouched one in a changed file: the typed
//! key table. A changed file's untouched key is not part of the change.

use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default, Debug)]
pub struct ChangedLines {
    files: FxHashSet<String>,
    /// New-side line ranges the change added or rewrote, per file.
    added: FxHashMap<String, Vec<(u32, u32)>>,
    /// The text of the lines the change removed, per file.
    removed: FxHashMap<String, String>,
    /// The runs themselves, to put a file back as it was before the change.
    runs: FxHashMap<String, Vec<crate::change_class::HunkText>>,
}

impl ChangedLines {
    /// From a unified diff, the paths as the diff prints them
    /// (repository-relative).
    pub fn from_diff(diff_text: &str) -> Self {
        let mut lines = Self::default();
        for hunk in crate::change_class::hunk_texts(diff_text) {
            lines.files.insert(hunk.path.clone());
            if hunk.new_len > 0 {
                lines
                    .added
                    .entry(hunk.path.clone())
                    .or_default()
                    .push((hunk.new_start, hunk.new_start + hunk.new_len - 1));
            }
            lines
                .runs
                .entry(hunk.path.clone())
                .or_default()
                .push(hunk.clone());
            if !hunk.removed.is_empty() {
                let text = lines.removed.entry(hunk.path).or_default();
                for line in &hunk.removed {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
        lines
    }

    pub fn is_changed_file(&self, rel: &str) -> bool {
        self.files.contains(rel)
    }

    /// Whether the change added or rewrote a line in `from..=to` of `rel`.
    pub fn touches(&self, rel: &str, from: usize, to: usize) -> bool {
        self.added.get(rel).is_some_and(|ranges| {
            ranges
                .iter()
                .any(|&(s, e)| (s as usize) <= to && from <= (e as usize))
        })
    }

    /// The file as it was before the change, rebuilt from its text after, and
    /// the line ranges in that old text the change removed, each with the
    /// line of the new text it anchors on (the first line that replaced it, or
    /// the line above a pure removal). `None` when the runs do not describe
    /// the text.
    pub fn base_side(
        &self,
        rel: &str,
        new_text: &str,
    ) -> Option<(String, Vec<(usize, usize, usize)>)> {
        let runs = self.runs.get(rel)?;
        let old = crate::change_class::reverted(new_text, runs)?;
        let removed = runs
            .iter()
            .filter(|r| r.old_len > 0)
            .map(|r| {
                (
                    r.old_start as usize,
                    (r.old_start + r.old_len - 1) as usize,
                    r.new_range().0 as usize,
                )
            })
            .collect();
        Some((old, removed))
    }

    /// The new-side anchor of the first run that removed something: where the
    /// removed lines stood when the runs do not fit the text.
    pub fn first_removal_anchor(&self, rel: &str) -> Option<usize> {
        self.runs
            .get(rel)?
            .iter()
            .find(|r| r.old_len > 0)
            .map(|r| r.new_range().0 as usize)
    }

    pub fn removed_text(&self, rel: &str) -> Option<&str> {
        self.removed.get(rel).map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_added_line_is_touched_and_its_neighbour_is_not() {
        let diff = "diff --git a/e b/e\n--- a/e\n+++ b/e\n@@ -2,0 +3 @@ x\n+NEW=1\n";
        let lines = ChangedLines::from_diff(diff);
        assert!(lines.is_changed_file("e"));
        assert!(lines.touches("e", 3, 3));
        assert!(!lines.touches("e", 1, 2));
        assert!(!lines.is_changed_file("f"));
    }

    #[test]
    fn a_removed_line_keeps_its_text() {
        let diff = "diff --git a/e b/e\n--- a/e\n+++ b/e\n@@ -2 +1,0 @@\n-OLD=1\n";
        assert_eq!(
            ChangedLines::from_diff(diff).removed_text("e"),
            Some("OLD=1\n")
        );
    }
}
