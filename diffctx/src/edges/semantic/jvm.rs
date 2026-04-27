use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::SEMANTIC_DISCOVERY;
use crate::config::extensions::{
    JAVA_EXTENSIONS, JVM_EXTENSIONS, KOTLIN_EXTENSIONS, SCALA_EXTENSIONS,
};
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

fn is_jvm_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    JVM_EXTENSIONS.contains(ext.as_str())
}

fn is_java(path: &Path) -> bool {
    let ext = base::file_ext(path);
    JAVA_EXTENSIONS.contains(ext.as_str())
}

fn is_kotlin(path: &Path) -> bool {
    let ext = base::file_ext(path);
    KOTLIN_EXTENSIONS.contains(ext.as_str())
}

fn is_scala(path: &Path) -> bool {
    let ext = base::file_ext(path);
    SCALA_EXTENSIONS.contains(ext.as_str())
}

static KOTLIN_IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*(?:\.[A-Z]\w*|\.\*)?)")
        .unwrap()
});
static KOTLIN_CLASS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:\w+\s+)*(?:class|interface|object|enum)\s+([A-Z]\w*)").unwrap()
});
static JAVA_IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+(?:static\s+)?([a-z][a-z0-9_.]*(?:\.\*)?)\s*;").unwrap()
});
static JAVA_PACKAGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*package\s+([a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)*)").unwrap());
static JAVA_CLASS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:\w+\s+)*(?:class|interface|enum|@interface)\s+([A-Z]\w*)").unwrap()
});
static SCALA_IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*import\s+([a-z][a-z0-9_.]+(?:\.[A-Z]\w*|\._|\.\{[^}]+\})?)").unwrap()
});
static SCALA_CLASS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*(?:\w+\s+)*(?:class|trait|object)\s+([A-Z]\w*)").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static ANNOTATION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"@([A-Z]\w*)").unwrap());
static KOTLIN_EXTENDS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:class|interface|object)\s+\w+(?:<[^>]*>)?(?:\([^)]*\))?\s*:\s*([^{]+)").unwrap()
});
static JAVA_EXTENDS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)\b(?:extends|implements)\s+([A-Z]\w*(?:\s*,\s*[A-Z]\w*)*)").unwrap()
});
static SCALA_EXTENDS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)\b(?:extends|with)\s+([A-Z]\w*)").unwrap());

static JVM_STDLIB_TYPES: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    [
        "String",
        "Integer",
        "Long",
        "Double",
        "Float",
        "Boolean",
        "Byte",
        "Short",
        "Character",
        "Object",
        "Class",
        "System",
        "Math",
        "Collections",
        "Arrays",
        "Optional",
        "HashMap",
        "ArrayList",
        "LinkedList",
        "Iterator",
        "Iterable",
        "Comparable",
        "Runnable",
        "Thread",
        "Exception",
        "RuntimeException",
        "IllegalArgumentException",
        "IllegalStateException",
        "NullPointerException",
        "IndexOutOfBoundsException",
        "IOException",
        "InputStream",
        "OutputStream",
        "StringBuilder",
        "StringBuffer",
        "Number",
        "Enum",
        "Void",
        "Override",
        "Unit",
        "Any",
        "AnyVal",
        "AnyRef",
        "Nothing",
        "Option",
        "Some",
        "Either",
        "Left",
        "Right",
        "Try",
        "Success",
        "Failure",
        "Future",
        "Promise",
        "Seq",
        "Vector",
        "Map",
        "Set",
        "Tuple",
        "Function",
        "Product",
        "Serializable",
        "Pair",
        "Triple",
        "Sequence",
    ]
    .iter()
    .copied()
    .collect()
});

fn extract_imports(content: &str, path: &Path) -> FxHashSet<String> {
    if is_java(path) {
        JAVA_IMPORT_RE
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .collect()
    } else if is_kotlin(path) {
        KOTLIN_IMPORT_RE
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .collect()
    } else if is_scala(path) {
        SCALA_IMPORT_RE
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .collect()
    } else {
        FxHashSet::default()
    }
}

fn extract_classes(content: &str, path: &Path) -> FxHashSet<String> {
    if is_java(path) {
        JAVA_CLASS_RE
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .collect()
    } else if is_kotlin(path) {
        KOTLIN_CLASS_RE
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .collect()
    } else if is_scala(path) {
        SCALA_CLASS_RE
            .captures_iter(content)
            .map(|c| c[1].to_string())
            .collect()
    } else {
        FxHashSet::default()
    }
}

fn extract_package(content: &str) -> Option<String> {
    JAVA_PACKAGE_RE.captures(content).map(|c| c[1].to_string())
}

fn extract_inheritance(content: &str, path: &Path) -> FxHashSet<String> {
    let mut refs = FxHashSet::default();
    if is_kotlin(path) {
        for cap in KOTLIN_EXTENDS_RE.captures_iter(content) {
            for type_cap in TYPE_REF_RE.captures_iter(&cap[1]) {
                refs.insert(type_cap[1].to_string());
            }
        }
    } else if is_java(path) {
        for cap in JAVA_EXTENDS_RE.captures_iter(content) {
            for type_cap in TYPE_REF_RE.captures_iter(&cap[1]) {
                refs.insert(type_cap[1].to_string());
            }
        }
    } else if is_scala(path) {
        for cap in SCALA_EXTENDS_RE.captures_iter(content) {
            refs.insert(cap[1].to_string());
        }
    }
    refs
}

fn extract_type_refs(content: &str) -> FxHashSet<String> {
    TYPE_REF_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

fn extract_annotations(content: &str) -> FxHashSet<String> {
    ANNOTATION_RE
        .captures_iter(content)
        .map(|c| c[1].to_string())
        .collect()
}

pub struct JVMEdgeBuilder;

impl EdgeBuilder for JVMEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let jvm_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_jvm_file(Path::new(f.path())))
            .collect();
        if jvm_frags.is_empty() {
            return FxHashMap::default();
        }

        let import_weight = EDGE_WEIGHTS["jvm_import"].forward;
        let inheritance_weight = EDGE_WEIGHTS["jvm_inheritance"].forward;
        let type_weight = EDGE_WEIGHTS["jvm_type"].forward;
        let same_package_weight = EDGE_WEIGHTS["jvm_same_package"].forward;
        let annotation_weight = EDGE_WEIGHTS["jvm_annotation"].forward;
        let reverse_factor = EDGE_WEIGHTS["jvm_import"].reverse_factor;

        let mut package_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut class_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut fqn_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();

        for f in &jvm_frags {
            let path = Path::new(f.path());
            let pkg = extract_package(&f.content);
            if let Some(ref pkg) = pkg {
                package_to_frags
                    .entry(pkg.clone())
                    .or_default()
                    .push(f.id.clone());
            }
            for cls in extract_classes(&f.content, path) {
                class_to_frags
                    .entry(cls.to_lowercase())
                    .or_default()
                    .push(f.id.clone());
                if let Some(ref pkg) = pkg {
                    fqn_to_frags
                        .entry(format!("{}.{}", pkg, cls).to_lowercase())
                        .or_default()
                        .push(f.id.clone());
                }
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for jf in &jvm_frags {
            let path = Path::new(jf.path());

            for imp in extract_imports(&jf.content, path) {
                if imp.ends_with(".*") {
                    let pkg_prefix = &imp[..imp.len() - 2];
                    for fid in package_to_frags.get(pkg_prefix).unwrap_or(&vec![]) {
                        if fid != &jf.id {
                            add_edge(&mut edges, &jf.id, fid, import_weight, reverse_factor);
                        }
                    }
                } else {
                    for fid in fqn_to_frags.get(&imp.to_lowercase()).unwrap_or(&vec![]) {
                        if fid != &jf.id {
                            add_edge(&mut edges, &jf.id, fid, import_weight, reverse_factor);
                        }
                    }
                    if let Some(last) = imp.split('.').next_back() {
                        for fid in class_to_frags.get(&last.to_lowercase()).unwrap_or(&vec![]) {
                            if fid != &jf.id {
                                add_edge(&mut edges, &jf.id, fid, import_weight, reverse_factor);
                            }
                        }
                    }
                }
            }

            for inh_ref in extract_inheritance(&jf.content, path) {
                for fid in class_to_frags
                    .get(&inh_ref.to_lowercase())
                    .unwrap_or(&vec![])
                {
                    if fid != &jf.id {
                        add_edge(&mut edges, &jf.id, fid, inheritance_weight, reverse_factor);
                    }
                }
            }

            for type_ref in extract_type_refs(&jf.content) {
                if !JVM_STDLIB_TYPES.contains(type_ref.as_str()) {
                    for fid in class_to_frags
                        .get(&type_ref.to_lowercase())
                        .unwrap_or(&vec![])
                    {
                        if fid != &jf.id {
                            add_edge(&mut edges, &jf.id, fid, type_weight, reverse_factor);
                        }
                    }
                }
            }

            for ann_ref in extract_annotations(&jf.content) {
                for fid in class_to_frags
                    .get(&ann_ref.to_lowercase())
                    .unwrap_or(&vec![])
                {
                    if fid != &jf.id {
                        add_edge(&mut edges, &jf.id, fid, annotation_weight, reverse_factor);
                    }
                }
            }

            if let Some(current_pkg) = extract_package(&jf.content) {
                for fid in package_to_frags.get(&current_pkg).unwrap_or(&vec![]) {
                    if fid != &jf.id {
                        add_edge(&mut edges, &jf.id, fid, same_package_weight, reverse_factor);
                    }
                }
            }
        }

        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        _repo_root: Option<&Path>,
        _file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let jvm_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_jvm_file(f)).collect();
        if jvm_changed.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<PathBuf> = changed.iter().cloned().collect();
        let jvm_candidates: Vec<PathBuf> = candidates
            .iter()
            .filter(|c| is_jvm_file(c) && !changed_set.contains(*c))
            .cloned()
            .collect();

        let mut discovered: FxHashSet<PathBuf> = FxHashSet::default();
        let mut frontier: Vec<PathBuf> = jvm_changed.iter().map(|f| (*f).clone()).collect();

        for _ in 0..SEMANTIC_DISCOVERY.max_depth {
            let mut type_refs: FxHashSet<String> = FxHashSet::default();
            let mut frontier_classes: FxHashSet<String> = FxHashSet::default();

            for f in &frontier {
                if let Ok(content) = std::fs::read_to_string(f) {
                    type_refs.extend(extract_type_refs(&content));
                    frontier_classes.extend(extract_classes(&content, f));
                }
            }

            let mut hop_found: Vec<PathBuf> = Vec::new();
            for c in &jvm_candidates {
                if discovered.contains(c) {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(c) {
                    let cand_classes = extract_classes(&content, c);
                    let cand_type_refs = extract_type_refs(&content);

                    if !cand_classes.is_disjoint(&type_refs)
                        || !cand_type_refs.is_disjoint(&frontier_classes)
                    {
                        hop_found.push(c.clone());
                        continue;
                    }
                    let cand_imports = extract_imports(&content, c);
                    for imp in &cand_imports {
                        if let Some(last) = imp.rsplit('.').next() {
                            if frontier_classes.contains(last) {
                                hop_found.push(c.clone());
                                break;
                            }
                        }
                    }
                }
            }

            let new_files: Vec<PathBuf> = hop_found
                .into_iter()
                .filter(|f| !discovered.contains(f))
                .collect();
            if new_files.is_empty() {
                break;
            }
            discovered.extend(new_files.iter().cloned());
            frontier = new_files;
        }

        let mut result: Vec<PathBuf> = discovered.into_iter().collect();
        result.sort();
        result
    }
}
