use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::needs::NEEDS;
use crate::stopwords::CODE_STOPWORDS;
use crate::types::{Fragment, FragmentId, extract_identifiers};

static CALL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(\w+)\s*\(").unwrap());
static TYPE_REF_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?::|->)\s*([A-Z]\w+)").unwrap());
static GENERIC_TYPE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[\[<,]\s*([A-Z]\w*)").unwrap());
static INVARIANT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(?:assert|require|ensure|precondition|postcondition|invariant)\s*\(\s*(\w+)")
        .unwrap()
});
static JS_IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]"#).unwrap());
static PY_IMPORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"from\s+(\S+)\s+import\s+(.+)").unwrap());
static JS_LOCAL_IMPORT_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"import\s+(?:\{([^}]+)\}|([A-Z]\w+))\s+from\s+['"]([^'"]+)['"]"#).unwrap()
});
static TF_VAR_NEED_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"var\.(\w+)").unwrap());
static TF_RES_REF_NEED_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:^|[^.\w])([a-zA-Z]\w*)\.(\w+)(?:\[\*?\w*\])?\.[\w\[\]*]+").unwrap()
});

static LANGUAGE_BUILTINS: Lazy<FxHashSet<String>> = Lazy::new(|| {
    [
        "range",
        "enumerate",
        "zip",
        "sorted",
        "reversed",
        "isinstance",
        "issubclass",
        "hasattr",
        "getattr",
        "setattr",
        "delattr",
        "callable",
        "iter",
        "next",
        "any",
        "all",
        "abs",
        "round",
        "pow",
        "divmod",
        "repr",
        "dir",
        "vars",
        "globals",
        "locals",
        "breakpoint",
        "property",
        "classmethod",
        "staticmethod",
        "dataclass",
        "object",
        "exception",
        "baseexception",
        "valueerror",
        "typeerror",
        "keyerror",
        "indexerror",
        "attributeerror",
        "importerror",
        "runtimeerror",
        "stopiteration",
        "generatorexit",
        "oserror",
        "ioerror",
        "filenotfounderror",
        "permissionerror",
        "notimplementederror",
        "zerodivisionerror",
        "overflowerror",
        "memoryerror",
        "recursionerror",
        "unicodeerror",
        "assertionerror",
        "lookuperror",
        "arithmeticerror",
        "array.from",
        "object.keys",
        "object.values",
        "object.entries",
        "array.isarray",
        "number.isnan",
        "number.isfinite",
        "parseint",
        "parsefloat",
        "isnan",
        "isfinite",
        "settimeout",
        "setinterval",
        "clearinterval",
        "cleartimeout",
        "requestanimationframe",
        "cancelanimationframe",
        "typeof",
        "void",
        "make",
        "append",
        "panic",
        "recover",
        "cap",
        "println",
        "printf",
        "sprintf",
        "fprintf",
        "errorf",
        "vec",
        "arc",
        "unwrap",
        "usestate",
        "useeffect",
        "usecontext",
        "usereducer",
        "usecallback",
        "usememo",
        "useref",
        "uselayouteffect",
        "useimperativehandle",
        "usedebugvalue",
        "useid",
        "usetransition",
        "usedeferredvalue",
        "createcontext",
        "forwardref",
        "createref",
        "suspense",
        "strictmode",
        "profiler",
        "usenavigate",
        "useparams",
        "uselocation",
        "usesearchparams",
        "useloaderdata",
        "useactiondata",
        "usefetcher",
        "useoutletcontext",
        "usedispatch",
        "useselector",
        "usestore",
        "usequery",
        "usemutation",
        "usesubscription",
        "describe",
        "beforeeach",
        "aftereach",
        "beforeall",
        "afterall",
        "assert",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
});

static COMMENT_PREFIXES: &[&str] = &["#", "//", "* ", "/*", "--", "\"\"\"", "'''", "<!--"];
static ONE_CLASS_PER_FILE_SUFFIXES: &[&str] = &[".swift", ".java", ".kt"];
static TF_EXTENSIONS: &[&str] = &[".tf", ".tfvars", ".hcl"];
static CONFIG_EXTENSIONS_FOR_DIFF: &[&str] = &[".yaml", ".yml", ".json", ".toml", ".ini"];
static TF_SKIP_REF_TYPES: &[&str] = &[
    "var",
    "local",
    "data",
    "module",
    "path",
    "terraform",
    "count",
    "each",
    "self",
];

#[derive(Debug, Clone)]
pub struct InformationNeed {
    pub need_type: String,
    pub symbol: String,
    pub scope: Option<Arc<str>>,
    pub priority: f64,
}

fn parse_import_names(names_str: &str) -> FxHashSet<String> {
    let mut result = FxHashSet::default();
    for name in names_str.split(',') {
        let name = name.trim().split(" as ").next().unwrap_or("").trim();
        if !name.is_empty() {
            result.insert(name.to_lowercase());
        }
    }
    result
}

fn collect_external_symbols_from_lines(changed_lines: &[&str]) -> FxHashSet<String> {
    let mut symbols = FxHashSet::default();
    for line in changed_lines {
        for m in JS_IMPORT_RE.captures_iter(line) {
            let js_names = &m[1];
            let js_source = &m[2];
            if !js_source.starts_with('.') {
                symbols.extend(parse_import_names(js_names));
            }
        }
        for m in PY_IMPORT_RE.captures_iter(line) {
            let py_module = &m[1];
            let py_names = &m[2];
            if !py_module.starts_with('.') {
                symbols.extend(parse_import_names(py_names));
            }
        }
    }
    symbols
}

fn is_local_import(source: &str) -> bool {
    source.starts_with('.') || source.starts_with("@/") || source.starts_with("~/")
}

fn add_needs_for_syms(
    syms: &FxHashSet<String>,
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    for sym in syms {
        if sym.len() >= NEEDS.min_symbol_length && !CODE_STOPWORDS.contains(sym) {
            let key = ("definition".to_string(), sym.clone());
            needs.entry(key).or_insert_with(|| InformationNeed {
                need_type: "definition".to_string(),
                symbol: sym.clone(),
                scope: None,
                priority: NEEDS.definition_priority,
            });
        }
    }
}

fn collect_js_import_needs(line: &str, needs: &mut FxHashMap<(String, String), InformationNeed>) {
    for m in JS_LOCAL_IMPORT_RE.captures_iter(line) {
        let named = m.get(1).map(|x| x.as_str());
        let default = m.get(2).map(|x| x.as_str());
        let source = &m[3];
        if !is_local_import(source) {
            continue;
        }
        let mut syms = FxHashSet::default();
        if let Some(named) = named {
            syms = parse_import_names(named);
        } else if let Some(default) = default {
            syms.insert(default.to_lowercase());
        }
        add_needs_for_syms(&syms, needs);
    }
}

fn collect_py_import_needs(line: &str, needs: &mut FxHashMap<(String, String), InformationNeed>) {
    for m in PY_IMPORT_RE.captures_iter(line) {
        let module = &m[1];
        let names = &m[2];
        if !module.starts_with('.') {
            continue;
        }
        let syms = parse_import_names(names);
        add_needs_for_syms(&syms, needs);
    }
}

fn collect_import_needs(
    changed_lines: &[&str],
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    for line in changed_lines {
        collect_js_import_needs(line, needs);
        collect_py_import_needs(line, needs);
    }
}

fn is_comment_line(line: &str) -> bool {
    let stripped = line.trim_start();
    COMMENT_PREFIXES.iter().any(|p| stripped.starts_with(p))
}

fn defines_strength(scope_match: bool, has_scope: bool) -> f64 {
    if scope_match {
        NEEDS.defines_scope_match
    } else if !has_scope {
        NEEDS.defines_no_scope
    } else {
        NEEDS.defines_other_scope
    }
}

fn is_test_file(path: &str) -> bool {
    let lower = path.to_lowercase();
    let name = std::path::Path::new(&lower)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".test.js")
        || name.ends_with(".test.jsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.ends_with(".spec.js")
        || name.ends_with(".spec.jsx")
        || name.ends_with("test.java")
        || name.ends_with("test.kt")
        || name.ends_with("test.scala")
        || name.ends_with("test.rs")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
}

fn is_test_fragment(frag: &Fragment) -> bool {
    if is_test_file(frag.path()) {
        return true;
    }
    if let Some(ref sym) = frag.symbol_name {
        return sym.to_lowercase().starts_with("test_");
    }
    false
}

pub fn match_strength_typed(frag: &Fragment, need: &InformationNeed) -> f64 {
    let sym = &need.symbol;
    let frag_sym = frag
        .symbol_name
        .as_ref()
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    let defines = !frag_sym.is_empty() && frag_sym == *sym;
    let mentions = frag.identifiers.contains(sym);
    let scope_match =
        need.scope.is_some() && need.scope.as_ref().map(|s| s.as_ref()) == Some(frag.path());
    let nt = need.need_type.as_str();

    if nt == "impact" && scope_match {
        return NEEDS.impact_scope_match;
    }
    if defines && !frag.kind.is_signature() {
        return defines_strength(scope_match, need.scope.is_some());
    }
    if nt == "impact" && mentions && !defines {
        return NEEDS.impact_mentions;
    }
    if defines && (frag.kind.is_signature() || nt == "signature") {
        return NEEDS.signature_defines;
    }
    if nt == "test" && mentions && is_test_fragment(frag) {
        return NEEDS.test_mentions;
    }
    if mentions {
        NEEDS.mentions_fallback
    } else {
        0.0
    }
}

fn extract_changed_lines(diff_text: &str) -> Vec<String> {
    let mut result = Vec::new();
    for line in diff_text.lines() {
        let is_added = line.starts_with('+') && !line.starts_with("+++");
        let is_removed = line.starts_with('-') && !line.starts_with("---");
        if is_added || is_removed {
            result.push(line[1..].to_string());
        }
    }
    result
}

fn path_suffix(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

fn infer_core_symbol(frag: &Fragment) -> Option<String> {
    if let Some(ref sym) = frag.symbol_name {
        return Some(sym.to_lowercase());
    }
    let suffix = path_suffix(frag.path());
    if ONE_CLASS_PER_FILE_SUFFIXES.contains(&suffix.as_str()) {
        let stem = std::path::Path::new(frag.path())
            .file_stem()
            .map(|s| s.to_string_lossy().to_string());
        if let Some(ref stem) = stem {
            if stem.len() >= NEEDS.min_symbol_length {
                return Some(stem.to_lowercase());
            }
        }
    }
    None
}

fn collect_core_needs(
    all_fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) -> FxHashSet<String> {
    let mut core_symbol_names = FxHashSet::default();
    let mut seen_paths = FxHashSet::default();
    for frag in all_fragments {
        if !core_ids.contains(&frag.id) {
            continue;
        }
        let sym = match infer_core_symbol(frag) {
            Some(s) => s,
            None => continue,
        };
        if seen_paths.contains(frag.path()) && frag.symbol_name.is_none() {
            continue;
        }
        if frag.symbol_name.is_none() {
            seen_paths.insert(frag.path().to_string());
        }
        core_symbol_names.insert(sym.clone());
        let key = ("impact".to_string(), sym.clone());
        needs.entry(key).or_insert_with(|| InformationNeed {
            need_type: "impact".to_string(),
            symbol: sym,
            scope: Some(frag.id.path.clone()),
            priority: NEEDS.impact_priority,
        });
    }
    core_symbol_names
}

fn process_line_for_needs(
    line: &str,
    external_syms: &FxHashSet<String>,
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    for m in CALL_RE.captures_iter(line) {
        let name = &m[1];
        let low = name.to_lowercase();
        if name.len() < NEEDS.min_symbol_length
            || CODE_STOPWORDS.contains(&low)
            || LANGUAGE_BUILTINS.contains(&low)
        {
            continue;
        }
        if external_syms.contains(&low) {
            continue;
        }
        let key = ("definition".to_string(), low.clone());
        needs.entry(key).or_insert_with(|| InformationNeed {
            need_type: "definition".to_string(),
            symbol: low,
            scope: None,
            priority: NEEDS.call_definition_priority,
        });
    }
    for m in TYPE_REF_RE.captures_iter(line) {
        let sym = m[1].to_lowercase();
        let key = ("signature".to_string(), sym.clone());
        needs.entry(key).or_insert_with(|| InformationNeed {
            need_type: "signature".to_string(),
            symbol: sym,
            scope: None,
            priority: NEEDS.signature_priority,
        });
    }
    for m in GENERIC_TYPE_RE.captures_iter(line) {
        let sym = m[1].to_lowercase();
        let key = ("signature".to_string(), sym.clone());
        needs.entry(key).or_insert_with(|| InformationNeed {
            need_type: "signature".to_string(),
            symbol: sym,
            scope: None,
            priority: NEEDS.signature_priority,
        });
    }
}

fn collect_diff_line_needs(
    changed_lines: &[String],
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    let line_refs: Vec<&str> = changed_lines.iter().map(|s| s.as_str()).collect();
    let external_syms = collect_external_symbols_from_lines(&line_refs);
    for line in changed_lines {
        if !is_comment_line(line) {
            process_line_for_needs(line, &external_syms, needs);
        }
    }
}

fn collect_test_needs(
    all_fragments: &[Fragment],
    core_symbol_names: &FxHashSet<String>,
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    for frag in all_fragments {
        if !is_test_fragment(frag) {
            continue;
        }
        let tested = frag.symbol_name.as_ref().map(|s| {
            let lower = s.to_lowercase();
            lower.strip_prefix("test_").unwrap_or(&lower).to_string()
        });
        if let Some(ref tested) = tested {
            if core_symbol_names.contains(tested)
                || needs.contains_key(&("definition".to_string(), tested.clone()))
            {
                let key = ("test".to_string(), tested.clone());
                needs.entry(key).or_insert_with(|| InformationNeed {
                    need_type: "test".to_string(),
                    symbol: tested.clone(),
                    scope: None,
                    priority: NEEDS.test_priority,
                });
            }
        }
    }
}

fn collect_invariant_needs(
    changed_lines: &[String],
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    for line in changed_lines {
        for m in INVARIANT_RE.captures_iter(line) {
            let sym = m[1].to_lowercase();
            if sym.len() >= NEEDS.min_symbol_length && !CODE_STOPWORDS.contains(&sym) {
                let key = ("invariant".to_string(), sym.clone());
                needs.entry(key).or_insert_with(|| InformationNeed {
                    need_type: "invariant".to_string(),
                    symbol: sym,
                    scope: None,
                    priority: NEEDS.invariant_priority,
                });
            }
        }
    }
}

fn is_terraform_diff(all_fragments: &[Fragment], core_ids: &FxHashSet<FragmentId>) -> bool {
    all_fragments.iter().any(|f| {
        core_ids.contains(&f.id) && TF_EXTENSIONS.contains(&path_suffix(f.path()).as_str())
    })
}

fn is_config_only_diff(all_fragments: &[Fragment], core_ids: &FxHashSet<FragmentId>) -> bool {
    let core_frags: Vec<&Fragment> = all_fragments
        .iter()
        .filter(|f| core_ids.contains(&f.id))
        .collect();
    !core_frags.is_empty()
        && core_frags
            .iter()
            .all(|f| CONFIG_EXTENSIONS_FOR_DIFF.contains(&path_suffix(f.path()).as_str()))
}

fn collect_config_context_needs(
    all_fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    let covered: FxHashSet<String> = needs.values().map(|n| n.symbol.clone()).collect();
    for frag in all_fragments {
        if !core_ids.contains(&frag.id) {
            continue;
        }
        for ident in &frag.identifiers {
            if ident.len() >= NEEDS.background_min_ident_length && !covered.contains(ident) {
                let key = ("background".to_string(), ident.clone());
                needs.entry(key).or_insert_with(|| InformationNeed {
                    need_type: "background".to_string(),
                    symbol: ident.clone(),
                    scope: None,
                    priority: NEEDS.background_priority,
                });
            }
        }
    }
}

fn collect_terraform_needs(
    changed_lines: &[String],
    needs: &mut FxHashMap<(String, String), InformationNeed>,
) {
    for line in changed_lines {
        for m in TF_VAR_NEED_RE.captures_iter(line) {
            let sym = m[1].to_lowercase();
            if sym.len() >= NEEDS.min_symbol_length && !CODE_STOPWORDS.contains(&sym) {
                let key = ("definition".to_string(), sym.clone());
                needs.entry(key).or_insert_with(|| InformationNeed {
                    need_type: "definition".to_string(),
                    symbol: sym,
                    scope: None,
                    priority: NEEDS.call_definition_priority,
                });
            }
        }
        for m in TF_RES_REF_NEED_RE.captures_iter(line) {
            let ref_type = m[1].to_lowercase();
            let ref_name = m[2].to_lowercase();
            if TF_SKIP_REF_TYPES.contains(&ref_type.as_str()) {
                continue;
            }
            let full_ref = format!("{}.{}", ref_type, ref_name);
            let key = ("definition".to_string(), full_ref.clone());
            needs.entry(key).or_insert_with(|| InformationNeed {
                need_type: "definition".to_string(),
                symbol: full_ref,
                scope: None,
                priority: NEEDS.definition_priority,
            });
        }
    }
}

pub fn concepts_from_diff_text(
    diff_text: &str,
    changed_lines: Option<&[String]>,
) -> FxHashSet<String> {
    let owned;
    let lines = match changed_lines {
        Some(l) => l,
        None => {
            owned = extract_changed_lines(diff_text);
            &owned
        }
    };
    let text = lines.join("\n");

    let expansion_stopwords: FxHashSet<String> = CODE_STOPWORDS
        .iter()
        .chain(LANGUAGE_BUILTINS.iter())
        .cloned()
        .collect();

    let raw = extract_identifiers(&text, NEEDS.min_symbol_length);
    raw.into_iter()
        .filter(|id| !expansion_stopwords.contains(id))
        .collect()
}

pub fn needs_from_diff(
    all_fragments: &[Fragment],
    core_ids: &FxHashSet<FragmentId>,
    diff_text: &str,
) -> Vec<InformationNeed> {
    let mut needs: FxHashMap<(String, String), InformationNeed> = FxHashMap::default();
    let changed_lines = extract_changed_lines(diff_text);

    let core_symbol_names = collect_core_needs(all_fragments, core_ids, &mut needs);
    collect_diff_line_needs(&changed_lines, &mut needs);
    collect_import_needs(
        &changed_lines.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        &mut needs,
    );
    collect_invariant_needs(&changed_lines, &mut needs);
    collect_test_needs(all_fragments, &core_symbol_names, &mut needs);
    if is_terraform_diff(all_fragments, core_ids) {
        collect_terraform_needs(&changed_lines, &mut needs);
    }
    if is_config_only_diff(all_fragments, core_ids) {
        collect_config_context_needs(all_fragments, core_ids, &mut needs);
    }

    if needs.is_empty() {
        let fallback = concepts_from_diff_text(diff_text, Some(&changed_lines));
        return fallback
            .into_iter()
            .map(|c| InformationNeed {
                need_type: "definition".to_string(),
                symbol: c,
                scope: None,
                priority: NEEDS.fallback_priority,
            })
            .collect();
    }

    let covered_symbols: FxHashSet<String> = needs.values().map(|n| n.symbol.clone()).collect();
    for c in concepts_from_diff_text(diff_text, Some(&changed_lines)) {
        if !covered_symbols.contains(&c) {
            let key = ("background".to_string(), c.clone());
            needs.entry(key).or_insert_with(|| InformationNeed {
                need_type: "background".to_string(),
                symbol: c,
                scope: None,
                priority: NEEDS.concept_background_priority,
            });
        }
    }

    needs.into_values().collect()
}
