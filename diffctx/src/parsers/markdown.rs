use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;

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

        for (idx, &(start_line, level, _)) in headings.iter().enumerate() {
            let end_line = find_section_end(&headings, idx, level, total_lines);
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

            fragments.push(Fragment {
                id: FragmentId::new(Arc::clone(&path), start_line, end_line),
                kind: FragmentKind::Section,
                content: snippet.clone(),
                identifiers: extract_identifiers(&snippet, 2),
                token_count: 0,
                symbol_name: None,
            });
        }

        fragments
    }
}

fn find_section_end(
    headings: &[(u32, u32, String)],
    idx: usize,
    level: u32,
    total_lines: u32,
) -> u32 {
    for &(next_line, next_level, _) in &headings[idx + 1..] {
        if next_level <= level {
            return next_line - 1;
        }
    }
    total_lines
}
