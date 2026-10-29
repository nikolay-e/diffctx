use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::canon::{KeyCache, for_each_word_key};
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

    fn category_label(&self) -> Option<&str> {
        Some("key_match")
    }

    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        // Keyed on the canonical spelling, so `max_pool_size` in a config file
        // meets `maxPoolSize` in code. A fragment's `identifiers` are already
        // lowercased, which erased the case boundaries the canonical form
        // splits on: the words are read again from the text, and the stored
        // set still contributes what the text does not spell.
        let mut ident_index: FxHashMap<String, Vec<(&FragmentId, &str)>> = FxHashMap::default();
        let mut cache = KeyCache::default();

        for f in fragments {
            let path = f.path();
            // `self`, `None`, `def`: every code fragment of a language shares
            // them, and a fallback edge on a keyword is noise with weight.
            // Config files keep every word: there the fallback is the link.
            let code = crate::config::extensions::CODE_EXTENSIONS
                .contains(base::file_ext(Path::new(path)).as_str());
            let mut keys: FxHashSet<String> = FxHashSet::default();
            for ident in &f.identifiers {
                keys.insert(cache.key(ident).to_string());
            }
            for_each_word_key(&f.content, &mut cache, |k| {
                if !keys.contains(k) {
                    keys.insert(k.to_string());
                }
            });
            for key in keys {
                if key.len() >= TAGS_SEMANTIC.min_ident_len
                    && !(code && SYNTAX_WORDS.contains(key.as_str()))
                {
                    ident_index.entry(key).or_default().push((&f.id, path));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FragmentKind, extract_identifiers};
    use std::sync::Arc;

    fn frag(path: &str, content: &str) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), 1, 3),
            kind: FragmentKind::Chunk,
            content: Arc::from(content),
            identifiers: extract_identifiers(content, 3),
            token_count: 10,
            symbol_name: None,
        }
    }

    /// The fallback keys on the canonical spelling: a snake_case setting in a
    /// config file meets the same setting in camelCase code, and a name that
    /// only shares two of its words stays apart.
    #[test]
    fn one_name_in_two_spellings_is_one_identifier() {
        let cfg = frag("app.conf", "max_pool_size = 10\n");
        let camel = frag("a.xyz", "limit maxPoolSize here\n");
        let other = frag("b.xyz", "limit poolSize here\n");
        let edges = TagsEdgeBuilder.build(&[cfg.clone(), camel.clone(), other.clone()], None);
        assert!(edges.contains_key(&(cfg.id.clone(), camel.id.clone())));
        assert!(!edges.contains_key(&(cfg.id.clone(), other.id.clone())));
    }
}
