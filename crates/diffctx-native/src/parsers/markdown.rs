use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::config::tokenization::TOKENIZATION;
use crate::types::{Fragment, FragmentId, FragmentKind, extract_identifiers};

use super::FragmentationStrategy;

const MARKDOWN_EXTENSIONS: &[&str] = &[".md", ".markdown", ".mdx"];

static HEADING_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(#{1,6})(?:\s(.+))?$").unwrap());

fn file_extension_lower(path: &str) -> String {
    if let Some(dot_pos) = path.rfind('.') {
        path[dot_pos..].to_ascii_lowercase()
    } else {
        String::new()
    }
}

pub struct MarkdownStrategy;

impl FragmentationStrategy for MarkdownStrategy {
    fn can_handle(&self, path: &str, _content: &str) -> bool {
        let ext = file_extension_lower(path);
        MARKDOWN_EXTENSIONS.iter().any(|&e| e == ext)
    }

    fn fragment(&self, path: Arc<str>, content: &str) -> Vec<Fragment> {
        let lines: Vec<&str> = content.split('\n').collect();
        if lines.is_empty() {
            return Vec::new();
        }

        let mut headings: Vec<(u32, u32, String)> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            if let Some(caps) = HEADING_RE.captures(line) {
                let level = caps.get(1).unwrap().as_str().len() as u32;
                let title = caps
                    .get(2)
                    .map(|m| m.as_str().trim().to_string())
                    .unwrap_or_default();
                headings.push((i as u32 + 1, level, title));
            }
        }

        if headings.is_empty() {
            return Vec::new();
        }

        let total_lines = lines.len() as u32;
        let mut fragments: Vec<Fragment> = Vec::new();

        for (idx, &(start_line, _level, _)) in headings.iter().enumerate() {
            let end_line = find_section_end(&headings, idx, total_lines);
            if end_line < start_line {
                continue;
            }

            let start_idx = (start_line - 1) as usize;
            let end_idx = end_line as usize;
            if start_idx >= lines.len() || end_idx > lines.len() {
                continue;
            }

            let mut snippet = lines[start_idx..end_idx].join("\n");
            if snippet.trim().is_empty() {
                continue;
            }
            if !snippet.ends_with('\n') {
                snippet.push('\n');
            }

            let identifiers =
                extract_identifiers(&snippet, TOKENIZATION.fragment_min_identifier_length);
            fragments.push(Fragment {
                id: FragmentId::new(Arc::clone(&path), start_line, end_line),
                kind: FragmentKind::Section,
                content: Arc::from(snippet),
                identifiers,
                token_count: 0,
                symbol_name: None,
            });
        }

        fragments
    }
}

/// A heading's own fragment ends at the very next heading, regardless of
/// level — not at the next heading of the *same or higher* level. The old
/// same-or-higher rule made a heading's span include every nested
/// subsection beneath it, so a lone top-level `#` heading (the common case:
/// one H1 title, many `##`/`###` children) spanned the entire rest of the
/// document. A one-line change in the H1's own preamble then flagged that
/// all-encompassing fragment as `role: "changed"`, dumping the whole file
/// instead of just the touched paragraph (#91).
fn find_section_end(headings: &[(u32, u32, String)], idx: usize, total_lines: u32) -> u32 {
    match headings.get(idx + 1) {
        Some(&(next_line, _, _)) => next_line - 1,
        None => total_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for #91: a lone H1 title followed only by `##` children
    /// must not produce a single fragment spanning the whole document — the
    /// H1's own fragment is just its direct preamble, up to the first child.
    #[test]
    fn h1_fragment_stops_at_first_child_heading_not_eof() {
        let content =
            "# Title\n\nPreamble line.\n\n## Section One\nBody one.\n\n## Section Two\nBody two.\n";
        let fragments = MarkdownStrategy.fragment(Arc::from("doc.md"), content);

        let title = fragments
            .iter()
            .find(|f| f.content.starts_with("# Title"))
            .expect("Title fragment present");
        assert_eq!(title.id.start_line, 1);
        assert_eq!(
            title.id.end_line, 4,
            "H1 fragment should stop before '## Section One', not run to EOF"
        );
    }

    #[test]
    fn sibling_headings_still_each_get_their_own_fragment() {
        let content =
            "## Section One\nBody one.\n\n## Section Two\nBody two.\n### Nested\nNested body.\n";
        let fragments = MarkdownStrategy.fragment(Arc::from("doc.md"), content);
        assert_eq!(fragments.len(), 3);

        let nested = fragments
            .iter()
            .find(|f| f.content.starts_with("### Nested"))
            .expect("Nested fragment present");
        assert_eq!(nested.id.start_line, 6);
        assert_eq!(nested.id.end_line, 8);
    }
}
