use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use diffctx_base::types::{Fragment, FragmentId};
use diffctx_config::config::weights::EDGE_WEIGHTS;
use diffctx_facts::facts::lang::swift::SwiftExtractor;
use diffctx_facts::facts::{
    BlobFacts, Def, DefKind, Disambig, Extractor, Fact, Namespace, UseKind,
};
use diffctx_snapshot::snapshot::{Loss, RunBudget};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids, discover_by_refs};

fn is_swift_file(path: &Path) -> bool {
    base::has_ext(path, &[".swift"])
}

static IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+(?:class|struct|enum|protocol|func\s+)?(\w+)").unwrap()
});

fn extract_imports(content: &str) -> FxHashSet<String> {
    base::captures1(&IMPORT_RE, content).collect()
}

fn reconstruct_file(frags: &[&Fragment]) -> String {
    let mut lines = vec![""; frags.iter().map(|f| f.end_line()).max().unwrap_or(0) as usize];
    for f in frags {
        let start = f.start_line().saturating_sub(1) as usize;
        for (i, line) in f.content.lines().enumerate() {
            if let Some(slot) = lines.get_mut(start + i) {
                *slot = line;
            }
        }
    }
    lines.join("\n")
}

fn at_line<'a>(frags: &[&'a Fragment], line: u32, tight: bool) -> Vec<&'a Fragment> {
    let hits: Vec<_> = frags
        .iter()
        .copied()
        .filter(|f| (f.start_line()..=f.end_line()).contains(&line))
        .collect();
    if !tight {
        return hits;
    }
    let Some(min_w) = hits.iter().map(|f| f.end_line() - f.start_line()).min() else {
        return hits;
    };
    hits.into_iter()
        .filter(|f| f.end_line() - f.start_line() == min_w)
        .collect()
}

fn is_ext(d: &Def) -> bool {
    matches!(d.local.disambig, Some(Disambig::ImplOf(_))) && d.local.container.is_empty()
}

#[derive(Default)]
struct SwiftIndex<'a> {
    blobs: FxHashMap<&'a str, BlobFacts>,
    defs: FxHashMap<String, Vec<FragmentId>>,
    types: FxHashMap<&'a str, FxHashSet<String>>,
    used: FxHashMap<&'a str, FxHashSet<String>>,
    own: FxHashMap<FragmentId, FxHashSet<String>>,
}

impl<'a> SwiftIndex<'a> {
    fn add(&mut self, path: &'a str, fset: &[&Fragment], budget: &RunBudget) {
        let blob = SwiftExtractor.extract(reconstruct_file(fset).as_bytes(), budget);
        if blob
            .losses
            .iter()
            .any(|l| matches!(l, Loss::Unsupported { .. }))
        {
            return;
        }
        for fact in &blob.facts {
            if let Fact::Definition(d) = fact {
                let name = d.local.name.as_str();
                if is_ext(d) {
                    self.types.entry(path).or_default().insert(name.to_string());
                    continue;
                }
                let is_t = d.local.ns == Namespace::Type;
                if !is_t && !matches!(d.kind, DefKind::Function | DefKind::Method) {
                    continue;
                }
                if is_t {
                    self.types.entry(path).or_default().insert(name.to_string());
                }
                let list = self.defs.entry(name.to_ascii_lowercase()).or_default();
                for f in at_line(fset, d.span.lines.0, !is_t) {
                    self.own
                        .entry(f.id.clone())
                        .or_default()
                        .insert(name.to_string());
                    if !list.contains(&f.id) {
                        list.push(f.id.clone());
                    }
                }
            } else if let Fact::Reference(r) = fact
                && (matches!(
                    r.use_kind,
                    UseKind::TypeUse | UseKind::Override | UseKind::Instantiate
                ) || r.name.as_str().starts_with(char::is_uppercase))
            {
                self.used
                    .entry(path)
                    .or_default()
                    .insert(r.name.as_str().to_string());
            }
        }
        self.blobs.insert(path, blob);
    }
}

pub(crate) struct SwiftEdgeBuilder;

impl EdgeBuilder for SwiftEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_swift_file(Path::new(f.path())))
        else {
            return FxHashMap::default();
        };
        let (import_w, rev) = (
            EDGE_WEIGHTS["swift_import"].forward,
            EDGE_WEIGHTS["swift_import"].reverse_factor,
        );
        let (conform_w, ext_w, type_w) = (
            EDGE_WEIGHTS["swift_conformance"].forward,
            EDGE_WEIGHTS["swift_extension"].forward,
            EDGE_WEIGHTS["swift_type"].forward,
        );
        let idx = base::FragmentIndex::new(fragments, repo_root);
        let budget = RunBudget::default();
        let mut by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        for f in &frags {
            by_path.entry(f.path()).or_default().push(*f);
        }
        let mut sx = SwiftIndex::default();
        for (&path, fset) in &by_path {
            sx.add(path, fset, &budget);
        }
        let mut edges: EdgeDict = FxHashMap::default();
        for (&path, fset) in &by_path {
            let Some(blob) = sx.blobs.get(path) else {
                for f in fset {
                    for imp in extract_imports(&f.content) {
                        base::link_by_name(&f.id, &imp, &idx, &mut edges, import_w, rev);
                    }
                }
                continue;
            };
            let used = sx.used.get(path);
            for fact in &blob.facts {
                match fact {
                    Fact::Import(imp) => {
                        for f in at_line(fset, imp.site.lines.0, false) {
                            base::link_by_name(
                                &f.id,
                                imp.module.as_str(),
                                &idx,
                                &mut edges,
                                import_w,
                                rev,
                            );
                        }
                    }
                    Fact::Definition(d) if is_ext(d) => {
                        if let Some(t) = sx.defs.get(&d.local.name.as_str().to_ascii_lowercase()) {
                            for f in at_line(fset, d.span.lines.0, false) {
                                add_edges_from_ids(&mut edges, &f.id, t, ext_w, rev);
                            }
                        }
                    }
                    Fact::Reference(r)
                        if matches!(
                            r.use_kind,
                            UseKind::TypeUse
                                | UseKind::Override
                                | UseKind::Call
                                | UseKind::Instantiate
                        ) =>
                    {
                        let Some(t) = sx.defs.get(&r.name.as_str().to_ascii_lowercase()) else {
                            continue;
                        };
                        let (w, direct) = (
                            if r.use_kind == UseKind::Override {
                                conform_w
                            } else {
                                type_w
                            },
                            matches!(r.use_kind, UseKind::TypeUse | UseKind::Override),
                        );
                        let scoped: Vec<_> = if direct {
                            Vec::new()
                        } else {
                            t.iter()
                                .filter(|id| {
                                    id.path.as_ref() == path
                                        || sx.types.get(id.path.as_ref()).is_some_and(|dt| {
                                            used.is_some_and(|u| !dt.is_disjoint(u))
                                        })
                                })
                                .cloned()
                                .collect()
                        };
                        let chosen = if direct || scoped.is_empty() {
                            t.as_slice()
                        } else {
                            &scoped
                        };
                        for f in at_line(fset, r.site.lines.0, true) {
                            if !sx
                                .own
                                .get(&f.id)
                                .is_some_and(|o| o.contains(r.name.as_str()))
                            {
                                add_edges_from_ids(&mut edges, &f.id, chosen, w, rev);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        edges
    }

    discover_by_refs!(is_swift_file, extract_imports);
}
