use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashMap;

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId, FragmentKind};

use super::EdgeDict;
use super::base::{EdgeBuilder, add_edge};

static HEADING_PREFIX_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^#+\s*").unwrap());
static MD_INTERNAL_LINK_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[.*?\]\(#([^)]+)\)").unwrap());
static CITATION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[@([^\]]+)\]").unwrap());

fn slugify(text: &str) -> String {
    let lower = text.to_lowercase();
    let mut result = String::with_capacity(lower.len());
    for ch in lower.chars() {
        if ch.is_alphanumeric() || ch == '-' {
            result.push(ch);
        } else if ch.is_whitespace() || ch == '_' {
            result.push('-');
        }
    }
    result.trim_matches('-').to_string()
}

fn is_document_fragment(kind: FragmentKind) -> bool {
    matches!(kind, FragmentKind::Section | FragmentKind::Chunk)
}

pub struct DocumentStructureEdgeBuilder;

impl EdgeBuilder for DocumentStructureEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let weight = EDGE_WEIGHTS["doc_structure"].forward;
        let reverse_factor = EDGE_WEIGHTS["doc_structure"].reverse_factor;

        let mut by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        for f in fragments {
            if is_document_fragment(f.kind) {
                by_path.entry(f.path()).or_default().push(f);
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (_path, frags) in &mut by_path {
            frags.sort_by_key(|f| f.start_line());
            for pair in frags.windows(2) {
                add_edge(&mut edges, &pair[0].id, &pair[1].id, weight, reverse_factor);
            }
        }

        edges
    }
}

pub struct AnchorLinkEdgeBuilder;

impl AnchorLinkEdgeBuilder {
    fn build_anchor_index<'a>(
        &self,
        fragments: &'a [Fragment],
    ) -> FxHashMap<String, &'a FragmentId> {
        let mut index: FxHashMap<String, &FragmentId> = FxHashMap::default();
        for f in fragments {
            if f.kind == FragmentKind::Section {
                let first_line = f.content.lines().next().unwrap_or("");
                let heading = HEADING_PREFIX_RE.replace(first_line, "");
                let slug = slugify(heading.trim());
                if !slug.is_empty() {
                    index.entry(slug).or_insert(&f.id);
                }
            }
        }
        index
    }
}

impl EdgeBuilder for AnchorLinkEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let weight = EDGE_WEIGHTS["anchor_link"].forward;
        let reverse_factor = EDGE_WEIGHTS["anchor_link"].reverse_factor;

        let anchor_index = self.build_anchor_index(fragments);
        let mut edges: EdgeDict = FxHashMap::default();

        for f in fragments {
            for cap in MD_INTERNAL_LINK_RE.captures_iter(&f.content) {
                let target_slug = slugify(&cap[1]);
                if let Some(target_id) = anchor_index.get(&target_slug) {
                    if **target_id != f.id {
                        add_edge(&mut edges, &f.id, target_id, weight, reverse_factor);
                    }
                }
            }
        }

        edges
    }
}

pub struct CitationEdgeBuilder;

impl EdgeBuilder for CitationEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let weight = EDGE_WEIGHTS["citation"].forward;

        let mut citation_to_frags: FxHashMap<String, Vec<&FragmentId>> = FxHashMap::default();
        for f in fragments {
            for cap in CITATION_RE.captures_iter(&f.content) {
                citation_to_frags
                    .entry(cap[1].to_string())
                    .or_default()
                    .push(&f.id);
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (_cit, frag_ids) in &citation_to_frags {
            if frag_ids.len() < 2 {
                continue;
            }
            let hub = frag_ids[0];
            for other in &frag_ids[1..] {
                let key_fwd = (hub.clone(), (*other).clone());
                let existing_fwd = edges.get(&key_fwd).copied().unwrap_or(0.0);
                if weight > existing_fwd {
                    edges.insert(key_fwd, weight);
                }
                let key_rev = ((*other).clone(), hub.clone());
                let existing_rev = edges.get(&key_rev).copied().unwrap_or(0.0);
                if weight > existing_rev {
                    edges.insert(key_rev, weight);
                }
            }
        }

        edges
    }
}

pub fn get_document_builders() -> Vec<Box<dyn EdgeBuilder>> {
    vec![
        Box::new(DocumentStructureEdgeBuilder),
        Box::new(AnchorLinkEdgeBuilder),
        Box::new(CitationEdgeBuilder),
    ]
}
