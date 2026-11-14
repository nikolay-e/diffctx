//! Which lines of which files a change touched, for the one reader that has
//! to tell a changed line from an untouched one in a changed file: the typed
//! key table. A changed file's untouched key is not part of the change.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::unquote_c_style;

/// One run of changed lines of a unified diff with its text, for what the
/// line ranges alone cannot say: whether the code or only its layout
/// changed, and what the change removed. Context lines split a hunk into
/// runs, so the ranges are the ones `-U0` would print.
#[derive(Clone, Debug, Default)]
pub struct HunkText {
    /// The new-side path as the diff prints it (repository-relative); the
    /// old-side path for a deleted file.
    pub path: String,
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    /// Only what a formatter changes, settled by [`crate::change_class::settle_layout`] against
    /// both whole versions of the file.
    pub layout: bool,
}

impl HunkText {
    /// The new-side lines the run occupies; a pure removal anchors on the
    /// new line right above it, as `-U0` numbers it.
    pub fn new_range(&self) -> (u32, u32) {
        if self.new_len == 0 {
            (self.new_start.max(1), self.new_start.max(1))
        } else {
            (self.new_start, self.new_start + self.new_len - 1)
        }
    }
}

fn hunk_start(spec: &str) -> Option<u32> {
    spec.split(',').next()?.parse().ok()
}

pub(crate) fn hunk_texts(diff_text: &str) -> Vec<HunkText> {
    let mut out: Vec<HunkText> = Vec::new();
    let mut old_path: Option<String> = None;
    let mut new_path: Option<String> = None;
    // `"b/src/caf\303\251.js"`: git quotes a non-ASCII path, and a quoted
    // path matched no fragment, so every check on that file went dark.
    let side = |rest: &str, prefix: &str| {
        let p = unquote_c_style(rest.split('\t').next().unwrap_or(""));
        (p != "/dev/null").then(|| p.strip_prefix(prefix).unwrap_or(&p).to_string())
    };
    // The next old and new line numbers, and whether the last body line
    // extended the current run.
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut open = false;
    let mut in_hunk = false;
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let mut parts = rest.split_whitespace();
            let old = parts.next().and_then(hunk_start);
            let new = parts
                .next()
                .and_then(|n| n.strip_prefix('+'))
                .and_then(hunk_start);
            if let (Some(o), Some(n)) = (old, new) {
                old_no = o;
                new_no = n;
                in_hunk = true;
                open = false;
            }
            continue;
        }
        if line.starts_with("diff ") {
            old_path = None;
            new_path = None;
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if let Some(rest) = line.strip_prefix("--- ") {
                old_path = side(rest, "a/");
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                new_path = side(rest, "b/");
            }
            continue;
        }
        let (removed, text) = match line.as_bytes().first() {
            Some(b'-') => (true, &line[1..]),
            Some(b'+') => (false, &line[1..]),
            Some(b'\\') => continue,
            _ => {
                old_no += 1;
                new_no += 1;
                open = false;
                continue;
            }
        };
        if !open {
            // A removal-only run sits after the new line before it, as -U0
            // numbers it.
            out.push(HunkText {
                path: new_path
                    .clone()
                    .or_else(|| old_path.clone())
                    .unwrap_or_default(),
                old_start: old_no,
                new_start: new_no,
                ..HunkText::default()
            });
            open = true;
        }
        let h = out.last_mut().expect("an open run");
        if removed {
            h.removed.push(text.to_string());
            h.old_len += 1;
            old_no += 1;
        } else {
            if h.new_len == 0 {
                h.new_start = new_no;
            }
            h.added.push(text.to_string());
            h.new_len += 1;
            new_no += 1;
        }
    }
    for h in &mut out {
        if h.new_len == 0 {
            h.new_start = h.new_start.saturating_sub(1);
        }
        if h.old_len == 0 {
            h.old_start = h.old_start.saturating_sub(1);
        }
    }
    out
}

/// `new` with the runs undone, or `None` when the runs do not describe it.
pub(crate) fn reverted(new: &str, runs: &[HunkText]) -> Option<String> {
    let lines: Vec<&str> = new.lines().collect();
    let mut old: Vec<&str> = Vec::with_capacity(lines.len());
    let mut next = 0usize;
    for run in runs {
        // A run without lines on a side sits after the line its start names.
        let at = |start: u32, len: u32| start as usize - usize::from(len > 0);
        let new_at = at(run.new_start, run.new_len);
        let new_end = new_at + run.new_len as usize;
        if new_at < next || new_end > lines.len() {
            return None;
        }
        old.extend(&lines[next..new_at]);
        let added = lines[new_at..new_end]
            .iter()
            .zip(&run.added)
            .all(|(line, added)| *line == added.trim_end_matches('\r'));
        if !added || old.len() != at(run.old_start, run.old_len) {
            return None;
        }
        old.extend(run.removed.iter().map(|l| l.trim_end_matches('\r')));
        next = new_end;
    }
    old.extend(&lines[next..]);
    let mut text = old.join("\n");
    text.push('\n');
    Some(text)
}

#[derive(Default, Debug)]
pub struct ChangedLines {
    files: FxHashSet<String>,
    /// New-side line ranges the change added or rewrote, per file.
    added: FxHashMap<String, Vec<(u32, u32)>>,
    /// The text of the lines the change removed, per file.
    removed: FxHashMap<String, String>,
    /// The runs themselves, to put a file back as it was before the change.
    runs: FxHashMap<String, Vec<HunkText>>,
}

impl ChangedLines {
    /// From a unified diff, the paths as the diff prints them
    /// (repository-relative).
    pub fn from_diff(diff_text: &str) -> Self {
        let mut lines = Self::default();
        for hunk in hunk_texts(diff_text) {
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
        let old = reverted(new_text, runs)?;
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

    #[test]
    fn hunk_texts_carry_both_sides_under_the_surviving_path() {
        let diff = "diff --git a/a.yaml b/a.yaml\n--- a/a.yaml\n+++ b/a.yaml\n@@ -1 +1 @@\n-tag: 1\n+tag: 2\ndiff --git a/gone b/gone\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        let hunks = hunk_texts(diff);
        assert_eq!(hunks[0].path, "a.yaml");
        assert_eq!(hunks[0].removed, vec!["tag: 1".to_string()]);
        assert_eq!(hunks[0].added, vec!["tag: 2".to_string()]);
        assert_eq!((hunks[1].path.as_str(), hunks[1].new_len), ("gone", 0));
        let context = "--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n a\n-b\n+B\n c\n d\n-e\n+E\n f\n";
        let runs: Vec<(u32, u32)> = hunk_texts(context).iter().map(|h| h.new_range()).collect();
        assert_eq!(runs, vec![(2, 2), (5, 5)]);
    }
}
