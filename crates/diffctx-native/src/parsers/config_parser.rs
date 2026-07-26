use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::config::tokenization::TOKENIZATION;
use crate::types::{Fragment, FragmentId, FragmentKind, extract_identifiers};

use super::FragmentationStrategy;

const CONFIG_EXTENSIONS: &[&str] = &[".yaml", ".yml", ".toml", ".json"];

static YAML_TOP_LEVEL_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_.-]*\s*:").unwrap());
static TOML_SECTION_HEADER: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\[").unwrap());
static JSON_TOP_LEVEL_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s{0,4}"[^"]+"\s*:"#).unwrap());

fn file_extension_lower(path: &str) -> String {
    if let Some(dot_pos) = path.rfind('.') {
        path[dot_pos..].to_ascii_lowercase()
    } else {
        String::new()
    }
}

pub struct ConfigStrategy;

impl FragmentationStrategy for ConfigStrategy {
    fn can_handle(&self, path: &str, _content: &str) -> bool {
        let ext = file_extension_lower(path);
        CONFIG_EXTENSIONS.iter().any(|&e| e == ext)
    }

    fn fragment(&self, path: Arc<str>, content: &str) -> Vec<Fragment> {
        let ext = file_extension_lower(&path);
        match ext.as_str() {
            ".yaml" | ".yml" => fragment_yaml(path, content),
            ".toml" => fragment_toml(path, content),
            ".json" => fragment_json(path, content),
            _ => Vec::new(),
        }
    }
}

fn fragment_yaml(path: Arc<str>, content: &str) -> Vec<Fragment> {
    split_at_top_level_pattern(path, content, &YAML_TOP_LEVEL_KEY)
}

fn fragment_toml(path: Arc<str>, content: &str) -> Vec<Fragment> {
    split_at_top_level_pattern(path, content, &TOML_SECTION_HEADER)
}

fn fragment_json(path: Arc<str>, content: &str) -> Vec<Fragment> {
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut boundaries: Vec<usize> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if JSON_TOP_LEVEL_KEY.is_match(line) {
            boundaries.push(i);
        }
    }

    if boundaries.len() < 2 {
        return make_single_fragment(path, &lines);
    }

    let mut fragments: Vec<Fragment> = Vec::new();
    for (idx, &start_idx) in boundaries.iter().enumerate() {
        let end_idx = if idx + 1 < boundaries.len() {
            boundaries[idx + 1] - 1
        } else {
            lines.len() - 1
        };

        let end_idx = trim_trailing_blanks(&lines, start_idx, end_idx);
        let start_line = start_idx as u32 + 1;
        let end_line = end_idx as u32 + 1;

        let mut snippet = lines[start_idx..=end_idx].join("\n");
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
            kind: FragmentKind::Chunk,
            content: Arc::from(snippet),
            identifiers,
            token_count: 0,
            symbol_name: None,
        });
    }

    if fragments.is_empty() {
        return make_single_fragment(path, &lines);
    }
    fragments
}

fn split_at_top_level_pattern(path: Arc<str>, content: &str, pattern: &Regex) -> Vec<Fragment> {
    let lines: Vec<&str> = content.split('\n').collect();
    if lines.is_empty() {
        return Vec::new();
    }

    let mut boundaries: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if pattern.is_match(line) {
            boundaries.push(i);
        }
    }

    if boundaries.len() < 2 {
        return make_single_fragment(path, &lines);
    }

    let mut fragments: Vec<Fragment> = Vec::new();
    for (idx, &start_idx) in boundaries.iter().enumerate() {
        let end_idx = if idx + 1 < boundaries.len() {
            boundaries[idx + 1] - 1
        } else {
            lines.len() - 1
        };

        let end_idx = trim_trailing_blanks(&lines, start_idx, end_idx);
        let start_line = start_idx as u32 + 1;
        let end_line = end_idx as u32 + 1;

        let mut snippet = lines[start_idx..=end_idx].join("\n");
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
            kind: FragmentKind::Chunk,
            content: Arc::from(snippet),
            identifiers,
            token_count: 0,
            symbol_name: None,
        });
    }

    if fragments.is_empty() {
        return make_single_fragment(path, &lines);
    }
    fragments
}

fn trim_trailing_blanks(lines: &[&str], start_idx: usize, mut end_idx: usize) -> usize {
    while end_idx > start_idx && lines[end_idx].trim().is_empty() {
        end_idx -= 1;
    }
    end_idx
}

fn make_single_fragment(path: Arc<str>, lines: &[&str]) -> Vec<Fragment> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut snippet = lines.join("\n");
    if snippet.trim().is_empty() {
        return Vec::new();
    }
    if !snippet.ends_with('\n') {
        snippet.push('\n');
    }
    let end_line = lines.len() as u32;
    let identifiers = extract_identifiers(&snippet, TOKENIZATION.fragment_min_identifier_length);
    vec![Fragment {
        id: FragmentId::new(path, 1, end_line),
        kind: FragmentKind::Chunk,
        content: Arc::from(snippet),
        identifiers,
        token_count: 0,
        symbol_name: None,
    }]
}
