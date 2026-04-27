use std::sync::Arc;

use crate::config::parsers::PARSERS;
use crate::config::tokenization::TOKENIZATION;
use crate::types::{Fragment, FragmentId, FragmentKind, extract_identifiers};

use super::FragmentationStrategy;

pub struct GenericStrategy;

impl FragmentationStrategy for GenericStrategy {
    fn can_handle(&self, _path: &str, _content: &str) -> bool {
        true
    }

    fn fragment(&self, path: Arc<str>, content: &str) -> Vec<Fragment> {
        let lines: Vec<&str> = content.split('\n').collect();
        if lines.is_empty() {
            return Vec::new();
        }

        let total = lines.len();
        let mut fragments: Vec<Fragment> = Vec::new();
        let mut start_idx: usize = 0;

        while start_idx < total {
            let end_idx = (start_idx + PARSERS.generic_max_lines - 1).min(total - 1);

            let start_line = start_idx as u32 + 1;
            let end_line = end_idx as u32 + 1;

            let mut snippet = lines[start_idx..=end_idx].join("\n");
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

            start_idx = end_idx + 1;
        }

        fragments
    }
}
