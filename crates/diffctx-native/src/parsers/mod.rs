mod config_parser;
mod generic;
mod markdown;
mod tree_sitter_strategy;

use std::sync::Arc;

use once_cell::sync::Lazy;

use crate::config::parsers::PARSERS;
use crate::config::tokenization::TOKENIZATION;
use crate::types::Fragment;

pub trait FragmentationStrategy: Send + Sync {
    fn can_handle(&self, path: &str, content: &str) -> bool;
    fn fragment(&self, path: Arc<str>, content: &str) -> Vec<Fragment>;
}

static STRATEGIES: Lazy<Vec<Box<dyn FragmentationStrategy>>> = Lazy::new(|| {
    vec![
        Box::new(tree_sitter_strategy::TreeSitterStrategy::new()),
        Box::new(markdown::MarkdownStrategy),
        Box::new(config_parser::ConfigStrategy),
        Box::new(generic::GenericStrategy),
    ]
});

pub fn fragment_file(path: Arc<str>, content: &str) -> Vec<Fragment> {
    for strategy in STRATEGIES.iter() {
        if strategy.can_handle(&path, content) {
            let fragments = strategy.fragment(Arc::clone(&path), content);
            if !fragments.is_empty() {
                return fragments;
            }
        }
    }

    Vec::new()
}

fn create_snippet(lines: &[&str], start_line: u32, end_line: u32) -> Option<String> {
    if start_line == 0 || end_line == 0 || start_line > end_line {
        return None;
    }
    let start_idx = (start_line - 1) as usize;
    let end_idx = end_line as usize;
    if start_idx >= lines.len() || end_idx > lines.len() {
        return None;
    }
    let mut snippet = lines[start_idx..end_idx].join("\n");
    if snippet.trim().is_empty() {
        return None;
    }
    if !snippet.ends_with('\n') {
        snippet.push('\n');
    }
    Some(snippet)
}

fn build_covered_set(covered: &[(u32, u32)]) -> rustc_hash::FxHashSet<u32> {
    let mut result = rustc_hash::FxHashSet::default();
    for &(start, end) in covered {
        for ln in start..=end {
            result.insert(ln);
        }
    }
    result
}

fn trim_blank_lines(lines: &[&str], mut start: u32, mut end: u32) -> (u32, u32) {
    while start <= end
        && lines
            .get((start - 1) as usize)
            .map_or(true, |l| l.trim().is_empty())
    {
        start += 1;
    }
    while end >= start
        && lines
            .get((end - 1) as usize)
            .map_or(true, |l| l.trim().is_empty())
    {
        end -= 1;
    }
    (start, end)
}

fn create_code_gap_fragments(
    path: Arc<str>,
    lines: &[&str],
    covered: &[(u32, u32)],
) -> Vec<Fragment> {
    if lines.is_empty() {
        return Vec::new();
    }

    let covered_set = build_covered_set(covered);
    let total = lines.len() as u32;

    let uncovered: Vec<u32> = (1..=total).filter(|ln| !covered_set.contains(ln)).collect();
    if uncovered.is_empty() {
        return Vec::new();
    }

    let mut gaps: Vec<(u32, u32)> = Vec::new();
    let mut gap_start = uncovered[0];
    let mut gap_end = uncovered[0];
    for &ln in &uncovered[1..] {
        if ln == gap_end + 1 {
            gap_end = ln;
        } else {
            gaps.push((gap_start, gap_end));
            gap_start = ln;
            gap_end = ln;
        }
    }
    gaps.push((gap_start, gap_end));

    let mut fragments = Vec::new();
    for (start, end) in gaps {
        let (start, end) = trim_blank_lines(lines, start, end);
        if start > end || end - start + 1 < PARSERS.min_fragment_lines {
            continue;
        }
        if let Some(snippet) = create_snippet(lines, start, end) {
            let identifiers = crate::types::extract_identifiers(
                &snippet,
                TOKENIZATION.fragment_min_identifier_length,
            );
            fragments.push(Fragment {
                id: crate::types::FragmentId::new(Arc::clone(&path), start, end),
                kind: crate::types::FragmentKind::Chunk,
                content: Arc::from(snippet),
                identifiers,
                token_count: 0,
                symbol_name: None,
            });
        }
    }

    fragments
}
