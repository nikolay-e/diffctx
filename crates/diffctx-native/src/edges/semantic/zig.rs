use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

fn is_zig_file(path: &Path) -> bool {
    base::has_ext(path, &[".zig"])
}

static IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"@import\s*\(\s*['"]([^'"]+)['"]"#).unwrap());
static FN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*(?:pub\s+)?fn\s+(\w+)").unwrap());
static STRUCT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:pub\s+)?const\s+(\w+)\s*=\s*(?:struct|union|enum|packed struct)")
        .unwrap()
});
static TYPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());
static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b(\w+)\s*\(").unwrap());

static ZIG_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    base::kw(concat!(
        "if else while for switch return break continue fn pub const var struct enum union error ",
        "try catch unreachable undefined null true false comptime inline extern export test defer ",
        "errdefer ",
    ))
});
fn extract_imports(content: &str) -> FxHashSet<String> {
    base::captures1(&IMPORT_RE, content).collect()
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs: FxHashSet<String> = base::captures1(&FN_RE, content).collect();
    defs.extend(base::captures1(&STRUCT_RE, content));
    defs
}

pub(crate) struct ZigEdgeBuilder;

impl EdgeBuilder for ZigEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_zig_file(Path::new(f.path()))) else {
            return FxHashMap::default();
        };

        let import_w = EDGE_WEIGHTS["zig_import"].forward;
        let type_w = EDGE_WEIGHTS["zig_type"].forward;
        let fn_w = EDGE_WEIGHTS["zig_fn"].forward;
        let reverse_factor = EDGE_WEIGHTS["zig_import"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let defs = base::DefIndex::new(&frags, reverse_factor, extract_defs);

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            for imp in extract_imports(&f.content) {
                base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, reverse_factor);
            }
            let types = base::captures1(&TYPE_RE, &f.content);
            defs.link_except(&mut edges, &f.id, types, &self_defs, type_w);
            let calls = base::captures1(&CALL_RE, &f.content)
                .filter(|name| !ZIG_KEYWORDS.contains(name.as_str()));
            defs.link_except(&mut edges, &f.id, calls, &self_defs, fn_w);
        }
        edges
    }

    discover_by_refs!(is_zig_file, extract_imports);
}
