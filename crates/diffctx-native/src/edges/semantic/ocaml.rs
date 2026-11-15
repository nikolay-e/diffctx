use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, discover_by_refs};

fn is_ocaml_file(path: &Path) -> bool {
    base::has_ext(path, &[".ml", ".mli"])
}

static OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*open\s+([A-Z]\w*)").unwrap());
static INCLUDE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*include\s+([A-Z]\w*)").unwrap());
static MODULE_DEF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*module\s+([A-Z]\w*)").unwrap());
static LET_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*let\s+(\w+)").unwrap());
static VAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*val\s+(\w+)").unwrap());
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*type\s+(\w+)").unwrap());
static MODULE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\.\w+").unwrap());

fn extract_opens(content: &str) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    refs.extend(base::captures1(&OPEN_RE, content));
    refs.extend(base::captures1(&INCLUDE_RE, content));
    refs
}

fn extract_defs(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    defs.extend(
        MODULE_DEF_RE
            .captures_iter(content)
            .map(|c| c[1].to_string()),
    );
    defs.extend(base::captures1(&LET_RE, content));
    defs.extend(base::captures1(&VAL_RE, content));
    defs.extend(base::captures1(&TYPE_DEF_RE, content));
    defs
}

fn extract_module_refs(content: &str) -> FxHashSet<String> {
    base::captures1(&MODULE_REF_RE, content).collect()
}

pub(crate) struct OCamlEdgeBuilder;

impl EdgeBuilder for OCamlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_ocaml_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };

        let open_w = EDGE_WEIGHTS["ocaml_open"].forward;
        let _type_w = EDGE_WEIGHTS["ocaml_type"].forward;
        let fn_w = EDGE_WEIGHTS["ocaml_fn"].forward;
        let mod_w = EDGE_WEIGHTS["ocaml_module_ref"].forward;
        let reverse_factor = EDGE_WEIGHTS["ocaml_open"].reverse_factor;

        let idx = base::FragmentIndex::new(fragments, repo_root);
        let defs = base::DefIndex::new(&frags, reverse_factor, extract_defs);

        // `open` brings a module into the whole file: the relation leaves from
        // the file's representative, where its seeds are lifted to.
        let reps = base::file_representatives(fragments.iter());
        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_defs = extract_defs(&f.content);
            let from = reps.get(f.path()).unwrap_or(&f.id);
            for open_name in extract_opens(&f.content) {
                // A module is its file's stem: `open Palette` is palette.ml.
                base::link_by_stem(
                    from,
                    &open_name,
                    &[".ml", ".mli"],
                    &idx,
                    &mut edges,
                    open_w,
                    reverse_factor,
                );
                defs.link_one(&mut edges, &f.id, &open_name, open_w);
            }
            let mrefs = extract_module_refs(&f.content);
            defs.link_except(&mut edges, &f.id, mrefs, &self_defs, mod_w);
            defs.link_except(&mut edges, &f.id, &f.identifiers, &self_defs, fn_w);
        }
        edges
    }

    discover_by_refs!(|p| is_ocaml_file(p), |c| extract_opens(c)
        .into_iter()
        .chain(extract_module_refs(c)));
}
