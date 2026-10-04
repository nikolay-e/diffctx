use std::path::Path;

use rustc_hash::FxHashMap;

use crate::config::edge_weights::TAGS_SEMANTIC;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder};

/// Words of a language's syntax and builtins. Narrower than the code
/// stopwords on purpose: `password` or `order` shared by a config and the
/// code that reads it is the link this fallback exists for.
static SYNTAX_WORDS: once_cell::sync::Lazy<rustc_hash::FxHashSet<&'static str>> =
    once_cell::sync::Lazy::new(|| {
        "self this cls super def fn func function lambda return none null nil undefined \
         true false void var let const val new class struct enum interface impl trait \
         type import from export package module use pub public private protected static \
         final async await yield int str bool float double char string object any"
            .split_whitespace()
            .collect()
    });

pub struct TagsEdgeBuilder;

impl EdgeBuilder for TagsEdgeBuilder {
    fn is_fallback(&self) -> bool {
        true
    }

    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let mut ident_index: FxHashMap<&str, Vec<(&FragmentId, &str)>> = FxHashMap::default();

        for f in fragments {
            let path = f.path();
            // `self`, `None`, `def`: every code fragment of a language shares
            // them, and a fallback edge on a keyword is noise with weight.
            // Config files keep every word: there the fallback is the link.
            let code = crate::config::extensions::CODE_EXTENSIONS
                .contains(base::file_ext(Path::new(path)).as_str());
            for ident in &f.identifiers {
                if ident.len() >= TAGS_SEMANTIC.min_ident_len
                    && !(code && SYNTAX_WORDS.contains(ident.to_lowercase().as_str()))
                {
                    ident_index
                        .entry(ident.as_str())
                        .or_default()
                        .push((&f.id, path));
                }
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for (_, holders) in &ident_index {
            if holders.len() < 2 || holders.len() > TAGS_SEMANTIC.max_fragments_per_ident {
                continue;
            }

            let mut cross_file_groups: FxHashMap<&str, Vec<&FragmentId>> = FxHashMap::default();
            for (fid, path) in holders {
                cross_file_groups.entry(path).or_default().push(fid);
            }

            if cross_file_groups.len() < 2 {
                continue;
            }

            let all_ids: Vec<&FragmentId> = holders.iter().map(|(fid, _)| *fid).collect();
            for i in 0..all_ids.len() {
                for j in (i + 1)..all_ids.len() {
                    let src = all_ids[i];
                    let dst = all_ids[j];
                    if src.path != dst.path {
                        base::add_edge(
                            &mut edges,
                            src,
                            dst,
                            TAGS_SEMANTIC.weight,
                            TAGS_SEMANTIC.reverse_factor,
                        );
                    }
                }
            }
        }

        edges
    }
}
