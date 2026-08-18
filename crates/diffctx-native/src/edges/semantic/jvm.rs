use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::SEMANTIC_DISCOVERY;
use crate::config::extensions::{
    JAVA_EXTENSIONS, JVM_EXTENSIONS, KOTLIN_EXTENSIONS, SCALA_EXTENSIONS,
};
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId, FragmentKind};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`: a name
/// resolving to more files than this names a vocabulary word, not a
/// dependency, and is skipped outright rather than truncated.
const MAX_FILES_PER_NAME: usize = 8;

fn is_jvm_file(path: &Path) -> bool {
    JVM_EXTENSIONS.contains(base::file_ext(path).as_str())
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
static SCALA_IMPORT_LINE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*import\s+(.+)$").unwrap());
static SCALA_PACKAGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*package\s+([A-Za-z_][\w.]*)").unwrap());
static SCALA_CLASS_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?m)^\s*(?:\w+\s+)*(?:class|trait|object|enum)\s+([A-Z]\w*)").unwrap()
});
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\b([A-Z]\w*)\b").unwrap());
static ANNOTATION_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"@([A-Z]\w*)").unwrap());
static MEMBER_USE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\.\s*([a-zA-Z_]\w{2,})").unwrap());
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

struct ScalaImport {
    prefix: String,
    selectors: Vec<String>,
    wildcard: bool,
}

fn split_top_level_commas(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn parse_scala_import_clause(clause: &str) -> Option<ScalaImport> {
    let clause = clause.trim();
    if clause.is_empty() {
        return None;
    }
    if let Some(bpos) = clause.find('{') {
        let prefix = clause[..bpos].trim().trim_end_matches('.').to_string();
        let inner_end = match clause.rfind('}') {
            Some(p) if p > bpos => p,
            Some(_) => return None,
            None => clause.len(),
        };
        let inner = &clause[bpos + 1..inner_end];
        let mut selectors = Vec::new();
        let mut wildcard = false;
        for sel in inner.split(',') {
            // A rename keeps the ORIGINAL name — that is what the target file
            // defines. Scala 2 spells it `C => D`, Scala 3 `C as D`.
            let name = sel
                .split("=>")
                .next()
                .unwrap_or("")
                .split(" as ")
                .next()
                .unwrap_or("")
                .trim()
                .trim_matches('`');
            match name {
                "_" | "*" | "given" => wildcard = true,
                "" => {}
                n if n
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '$') =>
                {
                    selectors.push(n.to_string());
                }
                _ => {}
            }
        }
        return Some(ScalaImport {
            prefix,
            selectors,
            wildcard,
        });
    }
    // Scala 3 top-level rename: `import a.b.c as d` imports `a.b.c`.
    let clause = clause.split(" as ").next().unwrap_or(clause).trim();
    if let Some(p) = clause
        .strip_suffix("._")
        .or_else(|| clause.strip_suffix(".*"))
        .or_else(|| clause.strip_suffix(".given"))
    {
        return Some(ScalaImport {
            prefix: p.to_string(),
            selectors: Vec::new(),
            wildcard: true,
        });
    }
    if !clause
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
    {
        return None;
    }
    if let Some(pos) = clause.rfind('.') {
        let sel = clause[pos + 1..].to_string();
        if sel.is_empty() {
            return None;
        }
        return Some(ScalaImport {
            prefix: clause[..pos].to_string(),
            selectors: vec![sel],
            wildcard: false,
        });
    }
    Some(ScalaImport {
        prefix: String::new(),
        selectors: vec![clause.to_string()],
        wildcard: false,
    })
}

fn parse_scala_imports(content: &str) -> Vec<ScalaImport> {
    let mut out = Vec::new();
    let mut lines = content.lines();
    while let Some(line) = lines.next() {
        let Some(cap) = SCALA_IMPORT_LINE_RE.captures(line) else {
            continue;
        };
        let mut clause_text = cap[1].split("//").next().unwrap_or("").trim().to_string();
        // A brace group may span lines; join until the braces balance so
        // `import scala.collection.{\n mutable,\n immutable\n}` keeps its
        // selectors instead of yielding an empty list.
        let mut open = clause_text.matches('{').count();
        let mut close = clause_text.matches('}').count();
        let mut joined = 0;
        // The join is bounded so an unbalanced brace (a file mid-edit) cannot
        // swallow the rest of the file into one clause.
        while open > close && joined < 32 {
            joined += 1;
            let Some(next) = lines.next() else { break };
            let next = next.split("//").next().unwrap_or("").trim();
            open += next.matches('{').count();
            close += next.matches('}').count();
            clause_text.push(' ');
            clause_text.push_str(next);
        }
        for clause in split_top_level_commas(&clause_text) {
            if let Some(imp) = parse_scala_import_clause(clause) {
                out.push(imp);
            }
        }
    }
    out
}

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
        let mut refs = FxHashSet::default();
        for imp in parse_scala_imports(content) {
            for sel in &imp.selectors {
                if imp.prefix.is_empty() {
                    refs.insert(sel.clone());
                } else {
                    refs.insert(format!("{}.{}", imp.prefix, sel));
                }
            }
            if imp.wildcard && !imp.prefix.is_empty() {
                refs.insert(imp.prefix.clone());
            }
        }
        refs
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

fn extract_package(content: &str, path: &Path) -> Option<String> {
    if is_scala(path) {
        // Scala chains package clauses (`package com.acme` then `package svc`
        // means com.acme.svc); `package object x` is a member of the enclosing
        // package, not a chain segment. A block-form clause (`package util {`)
        // opens a scope whose SIBLINGS are not chain segments — concatenating
        // past it produced nonexistent packages (`com.example.util.data`), so
        // the chain stops at the first block.
        let mut parts: Vec<String> = Vec::new();
        for cap in SCALA_PACKAGE_RE.captures_iter(content) {
            let seg = &cap[1];
            if seg == "object" {
                continue;
            }
            parts.push(seg.to_string());
            let after = content[cap.get(0).map_or(0, |m| m.end())..].trim_start();
            if after.starts_with('{') {
                break;
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("."))
        }
    } else {
        JAVA_PACKAGE_RE.captures(content).map(|c| c[1].to_string())
    }
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
    file_pkg: FxHashMap<&'a str, String>,
    import_files: FxHashMap<&'a str, FxHashSet<&'a str>>,
    import_pkgs: FxHashMap<&'a str, FxHashSet<String>>,
    named_files: FxHashMap<&'a str, FxHashSet<&'a str>>,
    inh_pairs: FxHashSet<(&'a str, &'a str)>,
}

#[allow(clippy::too_many_arguments)]
fn link_class<'a>(
    edges: &mut EdgeDict,
    rel: &mut FileRelations<'a>,
    src: &'a Fragment,
    name_lower: &str,
    weight: f64,
    reverse_factor: f64,
    record_import: bool,
    class_to_frags: &'a FxHashMap<String, Vec<FragmentId>>,
    class_files: &FxHashMap<String, FxHashSet<&'a str>>,
) {
    if class_files
        .get(name_lower)
        .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
    {
        return;
    }
    if let Some(fids) = class_to_frags.get(name_lower) {
        for fid in fids {
            if fid != &src.id {
                add_edge(edges, &src.id, fid, weight, reverse_factor);
                let bucket = if record_import {
                    &mut rel.import_files
                } else {
                    &mut rel.named_files
                };
                bucket
                    .entry(src.path())
                    .or_default()
                    .insert(fid.path.as_ref());
            }
        }
    }
}

impl<'a> FileRelations<'a> {
    /// A member-name match alone is tags-grade evidence; it only becomes an
    /// edge when the using file already relates to the defining file through
    /// something it wrote down: an import, a named type, or inheritance.
    /// Same-package proximity is deliberately not enough — it links call
    /// sites to every sibling implementation body (measured on
    /// bal_hard_008: `sink.consume` endorsing ConsoleSink's body).
    fn confirmed(&self, user: &str, definer: &str) -> bool {
        self.import_files
            .get(user)
            .is_some_and(|s| s.contains(definer))
            || self
                .named_files
                .get(user)
                .is_some_and(|s| s.contains(definer))
            || self
                .import_pkgs
                .get(user)
                .zip(self.file_pkg.get(definer))
                .is_some_and(|(pkgs, pkg)| pkgs.contains(pkg))
            || self.inh_pairs.contains(&(user, definer))
    }
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
        let member_weight = EDGE_WEIGHTS["jvm_member"].forward;
        let same_package_weight = EDGE_WEIGHTS["jvm_same_package"].forward;
        let annotation_weight = EDGE_WEIGHTS["jvm_annotation"].forward;
        let reverse_factor = EDGE_WEIGHTS["jvm_import"].reverse_factor;

        let mut file_pkg: FxHashMap<&str, String> = FxHashMap::default();
        for f in &jvm_frags {
            if let Some(pkg) = extract_package(&f.content, Path::new(f.path())) {
                file_pkg.entry(f.path()).or_insert(pkg);
            }
        }

        let mut package_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut class_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut class_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();
        let mut fqn_to_frags: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut member_defs: FxHashMap<String, Vec<FragmentId>> = FxHashMap::default();
        let mut member_def_files: FxHashMap<String, FxHashSet<&str>> = FxHashMap::default();

        for f in &jvm_frags {
            let path = Path::new(f.path());
            if let Some(pkg) = extract_package(&f.content, path) {
                package_to_frags.entry(pkg).or_default().push(f.id.clone());
            }
            for cls in extract_classes(&f.content, path) {
                let lower = cls.to_lowercase();
                class_files
                    .entry(lower.clone())
                    .or_default()
                    .insert(f.path());
                class_to_frags.entry(lower).or_default().push(f.id.clone());
                if let Some(pkg) = file_pkg.get(f.path()) {
                    fqn_to_frags
                        .entry(format!("{}.{}", pkg, cls).to_lowercase())
                        .or_default()
                        .push(f.id.clone());
                }
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
            file_pkg,
            import_files: FxHashMap::default(),
            import_pkgs: FxHashMap::default(),
            named_files: FxHashMap::default(),
            inh_pairs: FxHashSet::default(),
        };

        for jf in &jvm_frags {
            let path = Path::new(jf.path());

            if is_scala(path) {
                for imp in parse_scala_imports(&jf.content) {
                    for sel in &imp.selectors {
                        let sel_lower = sel.to_lowercase();
                        let mut hit_fqn = false;
                        if !imp.prefix.is_empty() {
                            let fqn = format!("{}.{}", imp.prefix, sel).to_lowercase();
                            if let Some(fids) = fqn_to_frags.get(&fqn) {
                                hit_fqn = true;
                                for fid in fids {
                                    if fid != &jf.id {
                                        add_edge(
                                            &mut edges,
                                            &jf.id,
                                            fid,
                                            import_weight,
                                            reverse_factor,
                                        );
                                        rel.import_files
                                            .entry(jf.path())
                                            .or_default()
                                            .insert(fid.path.as_ref());
                                    }
                                }
                            }
                        }
                        if !hit_fqn {
                            link_class(
                                &mut edges,
                                &mut rel,
                                jf,
                                &sel_lower,
                                import_weight,
                                reverse_factor,
                                true,
                                &class_to_frags,
                                &class_files,
                            );
                        }
                    }
                    if imp.wildcard && !imp.prefix.is_empty() {
                        rel.import_pkgs
                            .entry(jf.path())
                            .or_default()
                            .insert(imp.prefix.clone());
                        for fid in package_to_frags.get(imp.prefix.as_str()).unwrap_or(&vec![]) {
                            if fid != &jf.id {
                                add_edge(&mut edges, &jf.id, fid, import_weight, reverse_factor);
                            }
                        }
                        // `import Tables._` / `import pkg.Obj._` endorses an
                        // object's members, not a package: the prefix leaf is
                        // the object name.
                        if let Some(leaf) = imp.prefix.rsplit('.').next() {
                            if leaf.chars().next().is_some_and(|c| c.is_uppercase()) {
                                link_class(
                                    &mut edges,
                                    &mut rel,
                                    jf,
                                    &leaf.to_lowercase(),
                                    import_weight,
                                    reverse_factor,
                                    true,
                                    &class_to_frags,
                                    &class_files,
                                );
                            }
                        }
                    }
                }
            } else {
                for imp in extract_imports(&jf.content, path) {
                    if imp.ends_with(".*") {
                        let pkg_prefix = &imp[..imp.len() - 2];
                        rel.import_pkgs
                            .entry(jf.path())
                            .or_default()
                            .insert(pkg_prefix.to_string());
                        for fid in package_to_frags.get(pkg_prefix).unwrap_or(&vec![]) {
                            if fid != &jf.id {
                                add_edge(&mut edges, &jf.id, fid, import_weight, reverse_factor);
                            }
                        }
                    } else {
                        let mut hit_fqn = false;
                        if let Some(fids) = fqn_to_frags.get(&imp.to_lowercase()) {
                            hit_fqn = true;
                            for fid in fids {
                                if fid != &jf.id {
                                    add_edge(
                                        &mut edges,
                                        &jf.id,
                                        fid,
                                        import_weight,
                                        reverse_factor,
                                    );
                                    rel.import_files
                                        .entry(jf.path())
                                        .or_default()
                                        .insert(fid.path.as_ref());
                                }
                            }
                        }
                        if !hit_fqn {
                            if let Some(last) = imp.split('.').next_back() {
                                link_class(
                                    &mut edges,
                                    &mut rel,
                                    jf,
                                    &last.to_lowercase(),
                                    import_weight,
                                    reverse_factor,
                                    true,
                                    &class_to_frags,
                                    &class_files,
                                );
                            }
                        }
                    }
                }
            }

            for inh_ref in extract_inheritance(&jf.content, path) {
                let lower = inh_ref.to_lowercase();
                if class_files
                    .get(&lower)
                    .is_some_and(|s| s.len() > MAX_FILES_PER_NAME)
                {
                    continue;
                }
                if let Some(fids) = class_to_frags.get(&lower) {
                    for fid in fids {
                        if fid != &jf.id {
                            add_edge(&mut edges, &jf.id, fid, inheritance_weight, reverse_factor);
                            let a = jf.path();
                            let b: &str = fid.path.as_ref();
                            if a != b {
                                rel.inh_pairs.insert((a, b));
                                rel.inh_pairs.insert((b, a));
                            }
                        }
                    }
                }
            }

            for type_ref in extract_type_refs(&jf.content) {
                if !JVM_STDLIB_TYPES.contains(type_ref.as_str()) {
                    link_class(
                        &mut edges,
                        &mut rel,
                        jf,
                        &type_ref.to_lowercase(),
                        type_weight,
                        reverse_factor,
                        false,
                        &class_to_frags,
                        &class_files,
                    );
                }
            }

            for ann_ref in extract_annotations(&jf.content) {
                link_class(
                    &mut edges,
                    &mut rel,
                    jf,
                    &ann_ref.to_lowercase(),
                    annotation_weight,
                    reverse_factor,
                    false,
                    &class_to_frags,
                    &class_files,
                );
            }

            if let Some(current_pkg) = extract_package(&jf.content, path) {
                for fid in package_to_frags.get(&current_pkg).unwrap_or(&vec![]) {
                    if fid != &jf.id {
                        add_edge(&mut edges, &jf.id, fid, same_package_weight, reverse_factor);
                    }
                }
            }
        }

        for jf in &jvm_frags {
            let own = jf.symbol_name.as_deref().map(|s| s.to_lowercase());
            for m in extract_member_uses(&jf.content) {
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
                    if dst_path == jf.path() || d == &jf.id {
                        continue;
                    }
                    if rel.confirmed(jf.path(), dst_path) {
                        add_edge(&mut edges, &jf.id, d, member_weight, reverse_factor);
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
        file_cache: Option<&FxHashMap<PathBuf, String>>,
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
                if let Some(content) = base::read_file_cached(f, file_cache) {
                    type_refs.extend(extract_type_refs(&content));
                    frontier_classes.extend(extract_classes(&content, f));
                }
            }

            let mut hop_found: Vec<PathBuf> = Vec::new();
            for c in &jvm_candidates {
                if discovered.contains(c) {
                    continue;
                }
                if let Some(content) = base::read_file_cached(c, file_cache) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(s: &str) -> ScalaImport {
        let mut imports = parse_scala_imports(&format!("import {s}\n"));
        assert_eq!(imports.len(), 1, "expected one import from {s:?}");
        imports.remove(0)
    }

    #[test]
    fn scala_import_with_brace_before_open_brace_is_rejected_not_a_panic() {
        assert!(parse_scala_import_clause("a}.{B").is_none());
        assert!(parse_scala_import_clause("}x{").is_none());
        let open_only = parse_scala_import_clause("http.{Request").expect("still parses");
        assert_eq!(open_only.prefix, "http");
        assert_eq!(open_only.selectors, vec!["Request".to_string()]);
    }

    #[test]
    fn scala_import_captures_the_class_not_a_truncated_prefix() {
        let imp = parse_one("repo.UserRepository");
        assert_eq!(imp.prefix, "repo");
        assert_eq!(imp.selectors, vec!["UserRepository".to_string()]);
        assert!(!imp.wildcard);
    }

    #[test]
    fn scala_wildcard_and_brace_imports_resolve_selectors() {
        let w = parse_one("com.foo._");
        assert_eq!(w.prefix, "com.foo");
        assert!(w.wildcard && w.selectors.is_empty());

        let s3 = parse_one("com.foo.*");
        assert!(s3.wildcard);

        let braces = parse_one("http.{Request, Response}");
        assert_eq!(braces.prefix, "http");
        assert_eq!(
            braces.selectors,
            vec!["Request".to_string(), "Response".to_string()]
        );

        let rename = parse_one("a.b.{C => D, _}");
        assert_eq!(rename.prefix, "a.b");
        assert_eq!(rename.selectors, vec!["C".to_string()]);
        assert!(rename.wildcard);

        let object_rooted = parse_one("Tables._");
        assert_eq!(object_rooted.prefix, "Tables");
        assert!(object_rooted.wildcard);
    }

    #[test]
    fn scala3_renames_keep_the_original_name() {
        let top = parse_one("a.b.Conf as Config");
        assert_eq!(top.prefix, "a.b");
        assert_eq!(top.selectors, vec!["Conf".to_string()]);

        let braced = parse_one("a.{B as C, D}");
        assert_eq!(braced.prefix, "a");
        assert_eq!(braced.selectors, vec!["B".to_string(), "D".to_string()]);
    }

    #[test]
    fn scala_multiline_brace_import_keeps_its_selectors() {
        let content = "import scala.collection.{\n  mutable,\n  immutable\n}\nimport a.B\n";
        let imports = parse_scala_imports(content);
        assert_eq!(imports.len(), 2);
        assert_eq!(imports[0].prefix, "scala.collection");
        assert_eq!(
            imports[0].selectors,
            vec!["mutable".to_string(), "immutable".to_string()]
        );
        assert_eq!(imports[1].selectors, vec!["B".to_string()]);
    }

    #[test]
    fn scala_nested_package_blocks_do_not_concatenate_siblings() {
        let content =
            "package com.example\npackage util {\n  class A\n}\npackage data {\n  class B\n}\n";
        assert_eq!(
            extract_package(content, Path::new("a.scala")),
            Some("com.example.util".to_string())
        );
    }

    #[test]
    fn scala_chained_packages_join_and_package_object_is_skipped() {
        let content = "package com.acme\npackage service\n\nclass X {}\n";
        assert_eq!(
            extract_package(content, Path::new("a.scala")),
            Some("com.acme.service".to_string())
        );
        let pkg_obj = "package util\npackage object strings {\n  def slug = 1\n}\n";
        assert_eq!(
            extract_package(pkg_obj, Path::new("a.scala")),
            Some("util".to_string())
        );
    }
}
