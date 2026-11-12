use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

fn is_erlang_file(path: &Path) -> bool {
    base::has_ext(path, &[".erl", ".hrl"])
}

static MODULE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-module\((\w+)\)").unwrap());
static INCLUDE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)^-include(?:_lib)?\(\s*"([^"]+)""#).unwrap());
static BEHAVIOUR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-behaviou?r\((\w+)\)").unwrap());
static IMPORT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-import\((\w+),").unwrap());
static FUNC_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^([a-z]\w*)\s*\(").unwrap());
static REMOTE_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([a-z]\w*):(\w+)\s*\(").unwrap());

fn extract_refs(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    refs.extend(base::captures1(&INCLUDE_RE, content));
    refs.extend(
        BEHAVIOUR_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    refs.extend(base::captures1(&IMPORT_RE, content));
    refs.extend(
        REMOTE_CALL_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    refs
}

fn extract_modules(content: &str) -> FxHashSet<String> {
    base::captures1(&MODULE_RE, content).collect()
}

fn extract_func_defs(content: &str) -> FxHashSet<String> {
    base::captures1(&FUNC_DEF_RE, content).collect()
}

pub(crate) struct ErlangEdgeBuilder;

impl EdgeBuilder for ErlangEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_erlang_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let include_w = EDGE_WEIGHTS["erlang_include"].forward;
        let behaviour_w = EDGE_WEIGHTS["erlang_behaviour"].forward;
        let call_w = EDGE_WEIGHTS["erlang_call"].forward;
        let reverse_factor = EDGE_WEIGHTS["erlang_include"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let mods = base::DefIndex::new(&frags, reverse_factor, extract_modules);
        let fns = base::DefIndex::new(&frags, reverse_factor, extract_func_defs);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_fns = extract_func_defs(&f.content);
            for cap in INCLUDE_RE.captures_iter(&f.content) {
                base::link_by_name(&f.id, &cap[1], &idx, &mut edges, include_w, reverse_factor);
            }
            let behaviours = base::captures1(&BEHAVIOUR_RE, &f.content);
            mods.link(&mut edges, &f.id, behaviours, behaviour_w);
            mods.link(
                &mut edges,
                &f.id,
                base::captures1(&IMPORT_RE, &f.content),
                include_w,
            );
            let remote = base::captures1(&REMOTE_CALL_RE, &f.content);
            mods.link(&mut edges, &f.id, remote, call_w);
            fns.link_except(&mut edges, &f.id, &f.identifiers, &self_fns, call_w);
        }
        edges
    }

    discover_by_refs!(is_erlang_file, extract_refs);
}
