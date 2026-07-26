use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::types::{DiffHunk, Fragment, FragmentId, FragmentKind, extract_identifiers};

// A core fragment whose kind has no signature variant (chunk, section — the
// fallbacks for flat data files, unparsed languages and parse degradation) has
// no cheap stand-in, so when it does not fit the budget the change signal is
// dropped instead of shrunk (#103). The excerpt is that stand-in: the changed
// lines plus a little surrounding context, cut out of the parent's own text.
const CONTEXT_LINES: u32 = 3;
const MIN_PARENT_LINES: u32 = 12;
// Above this share of the parent the excerpt saves too little to be worth
// rendering as a separate fragment — the parent itself is the better answer.
const MAX_SHARE_OF_PARENT: f64 = 0.7;

fn has_signature_variant(kind: FragmentKind) -> bool {
    matches!(
        kind,
        FragmentKind::Function
            | FragmentKind::Class
            | FragmentKind::Struct
            | FragmentKind::Interface
            | FragmentKind::Enum
            | FragmentKind::Variable
    )
}

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
    if has_signature_variant(parent.kind) || parent.kind.is_stub() {
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
        return None;
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

/// Cheap stand-ins for the core fragments that have no signature variant,
/// keyed by the core they stand in for. Kept out of `all_fragments` on
/// purpose: they must not become graph nodes or ordinary context candidates,
/// only substitutes for a core that would otherwise vanish.
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

    #[test]
    fn no_excerpt_for_kinds_that_already_have_a_signature() {
        let mut parent = chunk("app.py", 1, 200);
        parent.kind = FragmentKind::Function;
        assert!(excerpt_from(&parent, &[hunk("app.py", 100, 2)]).is_none());
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
