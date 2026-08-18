use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::DOTNET_EXTENSIONS;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId, FragmentKind};

use super::super::EdgeDict;
use super::super::base::{
    self, EdgeBuilder, FragmentIndex, add_edge, discover_files_by_refs, link_by_name,
};

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`: a name
/// defined in more files than this is vocabulary, not a dependency, and is
/// skipped outright rather than truncated.
const MAX_FILES_PER_NAME: usize = 8;

static EXTENDED_DOTNET_EXTENSIONS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    DOTNET_EXTENSIONS
        .iter()
        .copied()
        .chain([".vb", ".csproj", ".fsproj", ".sln"])
        .collect()
});

fn is_dotnet_file(path: &Path) -> bool {
    EXTENDED_DOTNET_EXTENSIONS.contains(base::file_ext(path).as_str())
}

fn is_cs_file(path: &Path) -> bool {
    base::has_ext(path, &[".cs"])
}

fn is_fs_file(path: &Path) -> bool {
    base::has_ext(path, &[".fs", ".fsi", ".fsx"])
}

static CS_USING_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:global\s+)?using\s+(?:static\s+)?(?:\w+\s*=\s*)?([A-Z][\w.]+)").unwrap()
});
static FS_OPEN_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^\s*open\s+([A-Z][\w.]+)").unwrap());
static NAMESPACE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*namespace\s+([A-Z][\w.]+)").unwrap());
static TYPE_DEF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?m)^\s*(?:public|internal|private|protected)?\s*(?:static|abstract|sealed|partial)?\s*(?:class|struct|interface|enum|record)\s+(\w+)",
    )
    .unwrap()
});
static INHERITANCE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:class|struct|interface|record)\s+\w+\s*(?:<[^>]*>)?\s*:\s*(.+)").unwrap()
});
static ATTRIBUTE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\[(\w+)(?:\(|])").unwrap());
static PARTIAL_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:public|internal|private|protected)?\s*partial\s+(?:class|struct|interface|record)\s+(\w+)").unwrap()
});
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w+)\b").unwrap());
static MEMBER_USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\.\s*([a-zA-Z_]\w{2,})").unwrap());

static DOTNET_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "String",
        "Int32",
        "Boolean",
        "Object",
        "Void",
        "Task",
        "Action",
        "Func",
        "List",
        "Dictionary",
        "IEnumerable",
        "IList",
        "ICollection",
        "Exception",
        "Console",
        "Math",
        "Convert",
        "Type",
        "Attribute",
        "Nullable",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "break",
        "continue",
        "return",
        "new",
        "this",
        "base",
        "null",
        "true",
        "false",
        "var",
        "dynamic",
        "async",
        "await",
        "try",
        "catch",
        "finally",
        "throw",
        "using",
        "namespace",
        "class",
        "struct",
        "interface",
        "enum",
        "record",
        "delegate",
        "event",
        "public",
        "private",
        "protected",
        "internal",
        "static",
        "abstract",
        "sealed",
        "virtual",
        "override",
        "partial",
        "readonly",
        "const",
        "ref",
        "out",
        "in",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_usings(content: &str, path: &Path) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    if is_cs_file(path) {
        for cap in CS_USING_RE.captures_iter(content) {
            refs.insert(cap[1].to_string());
        }
    }
    if is_fs_file(path) {
        for cap in FS_OPEN_RE.captures_iter(content) {
            refs.insert(cap[1].to_string());
        }
    }
    refs
}

fn extract_namespaces(content: &str) -> FxHashSet<String> {
    NAMESPACE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_defines(content: &str) -> FxHashSet<String> {
    let mut defs = FxHashSet::default();
    for cap in TYPE_DEF_RE.captures_iter(content) {
        defs.insert(cap[1].to_string());
    }
    defs
}

fn extract_partials(content: &str) -> FxHashSet<String> {
    PARTIAL_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_base_types(content: &str) -> FxHashSet<String> {
    let mut bases = FxHashSet::default();
    for cap in INHERITANCE_RE.captures_iter(content) {
        for part in cap[1].split(',') {
            let trimmed = part.trim().split('<').next().unwrap_or("").trim();
            if !trimmed.is_empty() && trimmed.chars().next().map_or(false, |c| c.is_uppercase()) {
                bases.insert(trimmed.to_string());
            }
        }
    }
    bases
}

fn extract_attributes(content: &str) -> FxHashSet<String> {
    ATTRIBUTE_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !DOTNET_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .filter(|n| !DOTNET_KEYWORDS.contains(n.as_str()))
        .collect()
}

fn extract_member_uses(content: &str) -> FxHashSet<String> {
    MEMBER_USE_RE
        .captures_iter(content)
        .map(|c| c[1].to_lowercase())
        .collect()
}

fn is_member_def(f: &Fragment) -> bool {
    matches!(f.kind, FragmentKind::Function | FragmentKind::Property)
}

struct FileRelations<'a> {
    file_ns: FxHashMap<&'a str, FxHashSet<String>>,
    file_usings: FxHashMap<&'a str, FxHashSet<String>>,
    named_files: FxHashMap<&'a str, FxHashSet<&'a str>>,
    inh_pairs: FxHashSet<(&'a str, &'a str)>,
}

impl<'a> FileRelations<'a> {
    /// A member-name match alone is tags-grade evidence; it only becomes an
    /// edge when the using file already relates to the defining file through
    /// something it wrote down: a `using` naming the definer's namespace
    /// (extension methods never name their static class), a named type, or
    /// inheritance. Same-namespace proximity is deliberately not enough.
    fn confirmed(&self, user: &str, definer: &str) -> bool {
        if self
            .named_files
            .get(user)
            .is_some_and(|s| s.contains(definer))
            || self.inh_pairs.contains(&(user, definer))
        {
            return true;
        }
        match (self.file_usings.get(user), self.file_ns.get(definer)) {
            (Some(usings), Some(nss)) => nss.iter().any(|ns| usings.contains(ns)),
            _ => false,
        }
    }
}

fn link_defs<'a>(
    edges: &mut EdgeDict,
    rel: &mut FileRelations<'a>,
    src: &'a Fragment,
    name: &str,
    weight: f64,
    reverse_factor: f64,
    name_to_defs: &'a FxHashMap<String, Vec<FragmentId>>,
    name_def_files: &FxHashMap<String, FxHashSet<&'a str>>,
) {
    if name_def_files
        .get(name)
        .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
    {
        return;
    }
    if let Some(dst_ids) = name_to_defs.get(name) {
        for dst_id in dst_ids {
            if dst_id != &src.id {
                add_edge(edges, &src.id, dst_id, weight, reverse_factor);
                rel.named_files
                    .entry(src.path())
                    .or_default()
                    .insert(dst_id.path.as_ref());
            }
        }
    }
}

pub struct DotNetEdgeBuilder;

impl EdgeBuilder for DotNetEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let dn_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_dotnet_file(Path::new(f.path())))
            .collect();
        if dn_frags.is_empty() {
            return FxHashMap::default();
        }

        let using_weight = EDGE_WEIGHTS["dotnet_using"].forward;
        let inheritance_weight = EDGE_WEIGHTS["dotnet_inheritance"].forward;
        let type_weight = EDGE_WEIGHTS["dotnet_type"].forward;
        let member_weight = EDGE_WEIGHTS["dotnet_member"].forward;
        let same_ns_weight = EDGE_WEIGHTS["dotnet_same_namespace"].forward;
        let attribute_weight = EDGE_WEIGHTS["dotnet_attribute"].forward;
        let partial_weight = EDGE_WEIGHTS["dotnet_partial"].forward;
        let reverse_factor = EDGE_WEIGHTS["dotnet_using"].reverse_factor;

        let idx = FragmentIndex::new(fragments, repo_root);

        let mut name_to_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut name_def_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
        let mut frag_defines: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();
        let mut ns_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut frag_namespaces: FxHashMap<FragmentId, FxHashSet<String>> = FxHashMap::default();
        let mut partial_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut member_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut member_def_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
        let mut file_ns: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();
        let mut file_usings: FxHashMap<&str, FxHashSet<String>> = FxHashMap::default();

        for f in &dn_frags {
            let defs = extract_defines(&f.content);
            for name in &defs {
                name_to_defs
                    .entry(name.clone())
                    .or_default()
                    .push(f.id.clone());
                name_def_files
                    .entry(name.clone())
                    .or_default()
                    .insert(f.path());
            }
            frag_defines.insert(f.id.clone(), defs);

            let namespaces = extract_namespaces(&f.content);
            for ns in &namespaces {
                ns_to_frags
                    .entry(ns.clone())
                    .or_default()
                    .push(f.id.clone());
                file_ns.entry(f.path()).or_default().insert(ns.clone());
            }
            frag_namespaces.insert(f.id.clone(), namespaces);

            let usings = extract_usings(&f.content, Path::new(f.path()));
            if !usings.is_empty() {
                file_usings
                    .entry(f.path())
                    .or_default()
                    .extend(usings.iter().cloned());
            }

            let partials = extract_partials(&f.content);
            for p in &partials {
                partial_to_frags
                    .entry(p.clone())
                    .or_default()
                    .push(f.id.clone());
            }

            if is_member_def(f) {
                if let Some(name) = f.symbol_name.as_deref() {
                    if name.len() >= 3 {
                        let lower = name.to_lowercase();
                        member_defs
                            .entry(lower.clone())
                            .or_default()
                            .push(f.id.clone());
                        member_def_files
                            .entry(lower.clone())
                            .or_default()
                            .insert(f.path());
                    }
                }
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();
        let mut rel = FileRelations {
            file_ns,
            file_usings,
            named_files: FxHashMap::default(),
            inh_pairs: FxHashSet::default(),
        };

        for f in &dn_frags {
            let self_defs = frag_defines.get(&f.id).cloned().unwrap_or_default();
            let self_ns = frag_namespaces.get(&f.id).cloned().unwrap_or_default();

            let usings = extract_usings(&f.content, Path::new(f.path()));
            for u in &usings {
                if let Some(targets) = ns_to_frags.get(u) {
                    for tgt in targets {
                        if tgt != &f.id {
                            add_edge(&mut edges, &f.id, tgt, using_weight, reverse_factor);
                        }
                    }
                }
                link_by_name(&f.id, u, &idx, &mut edges, using_weight, reverse_factor);
            }

            let base_types = extract_base_types(&f.content);
            for bt in &base_types {
                if name_def_files
                    .get(bt)
                    .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
                {
                    continue;
                }
                if let Some(dst_ids) = name_to_defs.get(bt) {
                    for dst_id in dst_ids {
                        if dst_id != &f.id {
                            add_edge(
                                &mut edges,
                                &f.id,
                                dst_id,
                                inheritance_weight,
                                reverse_factor,
                            );
                            let a = f.path();
                            let b: &str = dst_id.path.as_ref();
                            if a != b {
                                rel.inh_pairs.insert((a, b));
                                rel.inh_pairs.insert((b, a));
                            }
                        }
                    }
                }
            }

            let type_refs = extract_type_refs(&f.content);
            for name in &type_refs {
                if self_defs.contains(name) {
                    continue;
                }
                link_defs(
                    &mut edges,
                    &mut rel,
                    f,
                    name,
                    type_weight,
                    reverse_factor,
                    &name_to_defs,
                    &name_def_files,
                );
            }

            let attrs = extract_attributes(&f.content);
            for attr in &attrs {
                link_defs(
                    &mut edges,
                    &mut rel,
                    f,
                    attr,
                    attribute_weight,
                    reverse_factor,
                    &name_to_defs,
                    &name_def_files,
                );
            }

            for ns in &self_ns {
                if let Some(targets) = ns_to_frags.get(ns) {
                    for tgt in targets {
                        if tgt != &f.id {
                            add_edge(&mut edges, &f.id, tgt, same_ns_weight, reverse_factor);
                        }
                    }
                }
            }
        }

        for f in &dn_frags {
            let own = f.symbol_name.as_deref().map(|s| s.to_lowercase());
            for m in extract_member_uses(&f.content) {
                if own.as_deref() == Some(m.as_str()) {
                    continue;
                }
                let Some(def_files) = member_def_files.get(&m) else {
                    continue;
                };
                if def_files.len() > MAX_FILES_PER_NAME {
                    continue;
                }
                let Some(defs) = member_defs.get(&m) else {
                    continue;
                };
                for d in defs {
                    let dst_path: &str = d.path.as_ref();
                    if dst_path == f.path() || d == &f.id {
                        continue;
                    }
                    if rel.confirmed(f.path(), dst_path) {
                        add_edge(&mut edges, &f.id, d, member_weight, reverse_factor);
                    }
                }
            }
        }

        for (_, frag_ids) in &partial_to_frags {
            if frag_ids.len() < 2 {
                continue;
            }
            for i in 0..frag_ids.len() {
                for j in (i + 1)..frag_ids.len() {
                    add_edge(
                        &mut edges,
                        &frag_ids[i],
                        &frag_ids[j],
                        partial_weight,
                        reverse_factor,
                    );
                    add_edge(
                        &mut edges,
                        &frag_ids[j],
                        &frag_ids[i],
                        partial_weight,
                        reverse_factor,
                    );
                }
            }
        }

        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let dn_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_dotnet_file(f)).collect();
        if dn_changed.is_empty() {
            return vec![];
        }

        let mut all_refs = FxHashSet::default();
        for f in &dn_changed {
            let content = base::read_file_cached(f, file_cache);
            if let Some(c) = content {
                all_refs.extend(extract_usings(&c, f));
                all_refs.extend(extract_namespaces(&c));
                for bt in extract_base_types(&c) {
                    all_refs.insert(bt);
                }
            }
        }

        discover_files_by_refs(&all_refs, changed, candidates, repo_root)
    }
}
