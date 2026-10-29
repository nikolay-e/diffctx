use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{DiffHunk, Fragment, FragmentId, FragmentKind, extract_identifiers};

// The changed lines plus a little surrounding context, cut out of a core
// fragment's own text.
//
// Originally a budget fallback for the kinds with no signature variant (chunk,
// section — flat data files, unparsed languages, parse degradation), where an
// oversized core would otherwise be dropped and the change signal lost (#103).
// It is now also the *downshift* target for a core that is mostly unchanged:
// emitting a 264-line function for a two-line edit is the over-dump behind #105,
// #107 and #149, and the signature variant is not a substitute there because it
// drops the changed lines entirely.
const CONTEXT_LINES: u32 = 3;
const MIN_PARENT_LINES: u32 = 12;
// A core is downshifted to its excerpt when the excerpt covers no more than
// this share of it — i.e. when most of what the parent would contribute is
// unchanged context. Deliberately stricter than `MAX_SHARE_OF_PARENT`: that
// one only decides whether an excerpt is worth cutting at all, this one
// decides whether to prefer it over the real fragment.
const DOWNSHIFT_MAX_SHARE: f64 = 0.5;
// Above this share of the parent the excerpt saves too little to be worth
// rendering as a separate fragment — the parent itself is the better answer.
const MAX_SHARE_OF_PARENT: f64 = 0.7;
// A gap of fewer lines than this is shown, not elided: a marker for it costs
// more than the lines it hides.
const MIN_ELIDED_LINES: u32 = 2;

fn hunk_window(parent: &Fragment, hunks: &[DiffHunk]) -> Option<(u32, u32)> {
    let (mut lo, mut hi) = (u32::MAX, 0u32);
    for hunk in hunks {
        if hunk.path.as_ref() != parent.path() {
            continue;
        }
        let (h_start, h_end) = hunk.core_selection_range();
        if h_start > parent.end_line() || h_end < parent.start_line() {
            continue;
        }
        lo = lo.min(h_start.max(parent.start_line()));
        hi = hi.max(h_end.min(parent.end_line()));
    }
    if lo == u32::MAX { None } else { Some((lo, hi)) }
}

fn excerpt_from(parent: &Fragment, hunks: &[DiffHunk]) -> Option<Fragment> {
    if parent.kind.is_stub() {
        return None;
    }
    if parent.line_count() < MIN_PARENT_LINES {
        return None;
    }
    let (hunk_lo, hunk_hi) = hunk_window(parent, hunks)?;

    let start = hunk_lo
        .saturating_sub(CONTEXT_LINES)
        .max(parent.start_line());
    let end = (hunk_hi + CONTEXT_LINES).min(parent.end_line());
    let span = end - start + 1;
    if f64::from(span) > f64::from(parent.line_count()) * MAX_SHARE_OF_PARENT {
        return sparse_excerpt(parent, hunks);
    }

    let offset = (start - parent.start_line()) as usize;
    let lines: Vec<&str> = parent.content.lines().collect();
    let take = span as usize;
    if offset >= lines.len() {
        return None;
    }
    let content: String = lines[offset..(offset + take).min(lines.len())].join("\n");
    if content.trim().is_empty() {
        return None;
    }

    Some(Fragment {
        id: FragmentId::new(parent.id.path.clone(), start, end),
        kind: FragmentKind::Excerpt,
        identifiers: extract_identifiers(&content, 3),
        content: Arc::from(content),
        token_count: 0,
        symbol_name: parent.symbol_name.clone(),
    })
}

/// The changed windows of a core whose changes lie far apart — each hunk with
/// its context, the unchanged lines between them elided by a marker — for a
/// core whose one contiguous window would be most of it. Only the budget
/// fallback uses it: a block scalar holding a dashboard's JSON is one
/// definition, and clipping its head showed none of the lines that changed
/// deep inside it (Forgejo #101). Its range runs from the first window to the
/// last, and is never the core's own, which would make it pass for the core.
fn sparse_excerpt(parent: &Fragment, hunks: &[DiffHunk]) -> Option<Fragment> {
    let mut windows: Vec<(u32, u32)> = hunks
        .iter()
        .filter(|h| h.path.as_ref() == parent.path())
        .map(|h| h.core_selection_range())
        .filter(|&(lo, hi)| lo <= parent.end_line() && hi >= parent.start_line())
        .map(|(lo, hi)| {
            (
                lo.saturating_sub(CONTEXT_LINES).max(parent.start_line()),
                (hi + CONTEXT_LINES).min(parent.end_line()),
            )
        })
        .collect();
    windows.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (lo, hi) in windows {
        match merged.last_mut() {
            Some(last) if lo <= last.1 + MIN_ELIDED_LINES => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    let shown: u32 = merged.iter().map(|(lo, hi)| hi - lo + 1).sum();
    if merged.len() < 2 || f64::from(shown) > f64::from(parent.line_count()) * MAX_SHARE_OF_PARENT {
        return None;
    }
    let (start, end) = (merged[0].0, merged[merged.len() - 1].1);
    if (start, end) == (parent.start_line(), parent.end_line()) {
        return None;
    }

    let lines: Vec<&str> = parent.content.lines().collect();
    let line = |n: u32| lines.get((n - parent.start_line()) as usize).copied();
    let mut content = String::new();
    for (i, &(lo, hi)) in merged.iter().enumerate() {
        if i > 0 {
            let gap = lo - merged[i - 1].1 - 1;
            content.push_str(&format!("… [{gap} unchanged lines]\n"));
        }
        for n in lo..=hi {
            content.push_str(line(n)?);
            content.push('\n');
        }
    }
    content.pop();
    Some(Fragment {
        id: FragmentId::new(parent.id.path.clone(), start, end),
        kind: FragmentKind::Excerpt,
        identifiers: extract_identifiers(&content, 3),
        content: Arc::from(content),
        token_count: 0,
        symbol_name: parent.symbol_name.clone(),
    })
}

/// Whether a core should be rendered as its excerpt rather than in full.
///
/// True when the hunk window covers only a small share of the parent, which is
/// exactly the over-dump shape: the parent's remaining lines are unchanged
/// context that the reader did not ask for. A change spread across most of the
/// fragment keeps the fragment.
pub fn is_downshift_worthwhile(parent: &Fragment, excerpt: &Fragment) -> bool {
    f64::from(excerpt.line_count()) <= f64::from(parent.line_count()) * DOWNSHIFT_MAX_SHARE
}

/// Hunk-window stand-ins for core fragments, keyed by the core each replaces.
/// Kept out of `all_fragments` on purpose: they must not become graph nodes or
/// ordinary context candidates, only substitutes for their own core.
pub fn generate_core_excerpts(
    all_fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    hunks: &[DiffHunk],
) -> FxHashMap<FragmentId, Fragment> {
    let mut excerpts = FxHashMap::default();
    for frag in all_fragments {
        if !core_ids.contains(&frag.id) {
            continue;
        }
        if let Some(excerpt) = excerpt_from(frag, hunks) {
            excerpts.insert(frag.id.clone(), excerpt);
        }
    }
    excerpts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(path: &str, start: u32, lines: u32) -> Fragment {
        let content: String = (start..start + lines)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        Fragment {
            id: FragmentId::new(Arc::from(path), start, start + lines - 1),
            kind: FragmentKind::Chunk,
            content: Arc::from(content),
            identifiers: FxHashSet::default(),
            token_count: 0,
            symbol_name: None,
        }
    }

    fn hunk(path: &str, start: u32, len: u32) -> DiffHunk {
        DiffHunk {
            path: Arc::from(path),
            new_start: start,
            new_len: len,
            old_start: start,
            old_len: len,
        }
    }

    #[test]
    fn excerpt_covers_the_changed_lines_with_context() {
        let parent = chunk("data.yml", 1, 200);
        let excerpt = excerpt_from(&parent, &[hunk("data.yml", 100, 2)]).expect("excerpt");

        assert_eq!(excerpt.kind, FragmentKind::Excerpt);
        assert_eq!(excerpt.start_line(), 97);
        assert_eq!(excerpt.end_line(), 104);
        assert!(excerpt.content.contains("line 100"));
        assert!(excerpt.content.contains("line 101"));
        assert!(!excerpt.content.contains("line 150"));
    }

    #[test]
    fn excerpt_is_clamped_to_the_parent_span() {
        let parent = chunk("data.yml", 50, 100);
        let excerpt = excerpt_from(&parent, &[hunk("data.yml", 51, 1)]).expect("excerpt");

        assert_eq!(excerpt.start_line(), 50);
        assert!(excerpt.content.starts_with("line 50"));
    }

    #[test]
    fn no_excerpt_when_it_would_cover_most_of_the_parent() {
        let parent = chunk("data.yml", 1, 20);
        assert!(excerpt_from(&parent, &[hunk("data.yml", 5, 12)]).is_none());
    }

    /// Kinds WITH a signature variant get an excerpt too. The signature is not
    /// a substitute for a mostly-unchanged core: it drops the changed lines
    /// entirely, which is the whole point of emitting the core in the first
    /// place. Excluding them was what made a two-line edit ship a 200-line
    /// function (#105/#107/#149).
    #[test]
    fn kinds_with_a_signature_variant_still_get_an_excerpt() {
        let mut parent = chunk("app.py", 1, 200);
        parent.kind = FragmentKind::Function;

        let excerpt = excerpt_from(&parent, &[hunk("app.py", 100, 2)]).expect("excerpt");
        assert_eq!(excerpt.kind, FragmentKind::Excerpt);
        assert!(excerpt.content.contains("line 100"));
        assert!(excerpt.content.contains("line 101"));
        assert!(crate::excerpt::is_downshift_worthwhile(&parent, &excerpt));
    }

    /// A change spread across most of the fragment keeps the fragment: there is
    /// no unchanged bulk to trim, and a window would just lose context.
    #[test]
    fn a_widely_spread_change_is_not_downshifted() {
        let parent = chunk("app.py", 1, 40);
        let excerpt = excerpt_from(&parent, &[hunk("app.py", 5, 20)]).expect("excerpt");
        assert!(!crate::excerpt::is_downshift_worthwhile(&parent, &excerpt));
    }

    /// Forgejo #101: two far-apart changes in one big definition show as two
    /// windows with the gap named, not as the head of the definition.
    #[test]
    fn far_apart_changes_become_windows_with_the_gap_named() {
        let parent = chunk("cm.yml", 1, 200);
        let excerpt = excerpt_from(&parent, &[hunk("cm.yml", 20, 1), hunk("cm.yml", 180, 1)])
            .expect("sparse excerpt");

        assert_eq!(excerpt.kind, FragmentKind::Excerpt);
        assert_eq!((excerpt.start_line(), excerpt.end_line()), (17, 183));
        assert!(excerpt.content.contains("line 20"));
        assert!(excerpt.content.contains("line 180"));
        assert!(excerpt.content.contains("… [153 unchanged lines]"));
        assert!(!excerpt.content.contains("line 100"));
        assert!(!is_downshift_worthwhile(&parent, &excerpt));
    }

    /// The sparse excerpt never takes the core's own range, which would make
    /// it pass for the core; nor is it cut when its windows are most of it.
    #[test]
    fn a_sparse_excerpt_is_none_when_it_would_pass_for_the_core_or_is_most_of_it() {
        let parent = chunk("cm.yml", 1, 200);
        let at_both_ends = [hunk("cm.yml", 1, 1), hunk("cm.yml", 200, 1)];
        assert!(excerpt_from(&parent, &at_both_ends).is_none());
        let most_of_it = [hunk("cm.yml", 5, 90), hunk("cm.yml", 150, 40)];
        assert!(excerpt_from(&parent, &most_of_it).is_none());
    }

    #[test]
    fn no_excerpt_when_no_hunk_touches_the_fragment() {
        let parent = chunk("data.yml", 1, 200);
        assert!(excerpt_from(&parent, &[hunk("other.yml", 100, 2)]).is_none());
    }

    #[test]
    fn only_core_fragments_get_excerpts() {
        let core = chunk("data.yml", 1, 200);
        let other = chunk("data.yml", 300, 200);
        let core_ids: FxHashSet<FragmentId> = [core.id.clone()].into_iter().collect();

        let excerpts = generate_core_excerpts(
            &[core.clone(), other],
            &core_ids,
            &[hunk("data.yml", 100, 2), hunk("data.yml", 350, 2)],
        );

        assert_eq!(excerpts.len(), 1);
        assert!(excerpts.contains_key(&core.id));
    }
}
