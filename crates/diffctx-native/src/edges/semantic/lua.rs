use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

fn is_lua_file(path: &Path) -> bool {
    base::has_ext(path, &[".lua"])
}

static REQUIRE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)require\s*[\(]?\s*['"]([^'"]+)['"]"#).unwrap());
static DOFILE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?m)dofile\s*\(\s*['"]([^'"]+)['"]"#).unwrap());
static FUNC_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:local\s+)?function\s+([\w.:]+)").unwrap());
static LOCAL_FUNC_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*local\s+(\w+)\s*=\s*function").unwrap());
static METHOD_CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)[:.]\w+\s*\(").unwrap());

fn extract_requires(content: &str) -> FxHashSet<String> {
    let mut refs: FxHashSet<String> = base::captures1(&REQUIRE_RE, content).collect();
    refs.extend(base::captures1(&DOFILE_RE, content));
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = FxHashSet::default();
    for cap in FUNC_DEF_RE.captures_iter(content) {
        let name = &cap[1];
        let leaf = name.split(&['.', ':'][..]).last().unwrap_or(name);
        defs.insert(leaf.to_string());
    }
    defs.extend(
        LOCAL_FUNC_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs
}

fn extract_method_targets(content: &str) -> FxHashSet<String> {
    base::captures1(&METHOD_CALL_RE, content).collect()
}

pub(crate) struct LuaEdgeBuilder;

impl EdgeBuilder for LuaEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_lua_file(Path::new(f.path()))) else {
            return FxHashMap::default();
        };

        let require_w = EDGE_WEIGHTS["lua_require"].forward;
        let fn_w = EDGE_WEIGHTS["lua_fn"].forward;
        let method_w = EDGE_WEIGHTS["lua_method"].forward;
        let reverse_factor = EDGE_WEIGHTS["lua_require"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let defs = base::DefIndex::new(&frags, reverse_factor, extract_defs);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for req in extract_requires(&f.content) {
                base::link_by_name(&f.id, &req, &idx, &mut edges, require_w, reverse_factor);
            }
            let methods = extract_method_targets(&f.content);
            defs.link_except(&mut edges, &f.id, methods, &self_defs, method_w);
            defs.link_except(&mut edges, &f.id, &f.identifiers, &self_defs, fn_w);
        }
        edges
    }

    discover_by_refs!(is_lua_file, extract_requires);
}
