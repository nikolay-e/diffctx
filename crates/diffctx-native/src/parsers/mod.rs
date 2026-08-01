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

/// Splits an over-long uncovered run into bounded chunks, preferring a blank
/// line as the cut point.
///
/// A gap used to become one fragment however long it was, so a file the grammar
/// extracts nothing from — a flat bash script, a `CMakeLists.txt`, any language
/// without a grammar — collapsed into a single whole-file chunk. Nothing
/// narrower could ever be selected, so a one-line diff rendered the entire file
/// as changed (#105, #107) and its unchanged remainder was unavailable as
/// context at any useful granularity.
///
/// Reuses the thresholds that already govern sub-fragmenting large definitions
/// (`sub_fragment_threshold_lines` / `sub_fragment_target_lines`) rather than
/// introducing a second size policy for the same question.
fn split_long_gap(lines: &[&str], start: u32, end: u32) -> Vec<(u32, u32)> {
    let threshold = PARSERS.sub_fragment_threshold_lines;
    let target = PARSERS.sub_fragment_target_lines.max(1);
    if end < start || end - start + 1 <= threshold {
        return vec![(start, end)];
    }

    let is_blank = |ln: u32| {
        lines
            .get((ln - 1) as usize)
            .is_some_and(|l| l.trim().is_empty())
    };

    let mut out = Vec::new();
    let mut chunk_start = start;
    while chunk_start <= end {
        let ideal_end = (chunk_start + target - 1).min(end);
        // Prefer a blank line near the target so a chunk boundary lands between
        // logical blocks rather than mid-statement. Search a window of up to
        // half the target on either side, then fall back to the hard cut.
        let slack = (target / 2).max(1);
        let mut chunk_end = ideal_end;
        if ideal_end < end {
            let lo = ideal_end.saturating_sub(slack).max(chunk_start);
            let hi = (ideal_end + slack).min(end);
            if let Some(blank) = (lo..=hi).rev().find(|&ln| is_blank(ln)) {
                chunk_end = blank;
            }
        }
        // The remainder is too small to stand alone: fold it into this chunk
        // instead of emitting a stub.
        if end - chunk_end < PARSERS.min_fragment_lines {
            chunk_end = end;
        }
        out.push((chunk_start, chunk_end));
        chunk_start = chunk_end + 1;
    }
    out
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
    for (start, end) in gaps
        .into_iter()
        .flat_map(|(s, e)| split_long_gap(lines, s, e))
    {
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

#[cfg(test)]
mod gap_tests {
    use super::*;

    fn numbered(n: usize) -> Vec<String> {
        (1..=n).map(|i| format!("line {i}")).collect()
    }

    fn refs(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    /// A gap shorter than the threshold is one chunk: splitting it would only
    /// fragment a block that already reads as a unit.
    #[test]
    fn a_short_gap_is_left_whole() {
        let owned = numbered(PARSERS.sub_fragment_threshold_lines as usize);
        let lines = refs(&owned);
        assert_eq!(
            split_long_gap(&lines, 1, lines.len() as u32),
            vec![(1, lines.len() as u32)]
        );
    }

    /// The defect behind #105/#107: an uncovered run became one fragment however
    /// long, so a file the grammar extracts nothing from had no sub-file
    /// granularity at all and a one-line diff rendered all of it.
    #[test]
    fn a_long_gap_is_split_into_bounded_chunks_covering_every_line() {
        let owned = numbered(300);
        let lines = refs(&owned);
        let chunks = split_long_gap(&lines, 1, 300);

        assert!(chunks.len() > 1, "a 300-line gap was left as one fragment");
        for &(start, end) in &chunks {
            assert!(start <= end, "inverted chunk {start}-{end}");
            assert!(
                end - start + 1 <= PARSERS.sub_fragment_threshold_lines * 2,
                "chunk {start}-{end} is far past the target size"
            );
        }
        // Contiguous and complete: no line may be dropped or duplicated, or the
        // file's content would silently go missing from the universe.
        assert_eq!(chunks.first().unwrap().0, 1);
        assert_eq!(chunks.last().unwrap().1, 300);
        for pair in chunks.windows(2) {
            assert_eq!(pair[1].0, pair[0].1 + 1, "gap or overlap at {pair:?}");
        }
    }

    /// Blank lines are the cheapest available proxy for a block boundary, so a
    /// cut should land on one rather than mid-statement when one is in reach.
    #[test]
    fn a_cut_prefers_a_blank_line_near_the_target() {
        let target = PARSERS.sub_fragment_target_lines as usize;
        let mut owned = numbered(200);
        // Put a blank line a couple of lines before the first ideal cut.
        let blank_at = target - 2;
        owned[blank_at - 1] = String::new();
        let lines = refs(&owned);

        let chunks = split_long_gap(&lines, 1, 200);
        assert_eq!(
            chunks[0].1, blank_at as u32,
            "the first cut ignored a blank line within reach: {chunks:?}"
        );
    }

    /// Splitting must not leave a stub behind: a remainder below the minimum
    /// fragment size is folded into the preceding chunk.
    #[test]
    fn a_tiny_remainder_is_folded_into_the_previous_chunk() {
        let target = PARSERS.sub_fragment_target_lines;
        let total = target * 2 + PARSERS.min_fragment_lines.saturating_sub(1);
        let owned = numbered(total as usize);
        let lines = refs(&owned);

        let chunks = split_long_gap(&lines, 1, total);
        assert!(
            chunks
                .iter()
                .all(|&(s, e)| e - s + 1 >= PARSERS.min_fragment_lines),
            "a chunk below the minimum size survived: {chunks:?}"
        );
    }
}
