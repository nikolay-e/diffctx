use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::extensions::{CODE_EXTENSIONS, CONFIG_EXTENSIONS};
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge};

static CONFIG_EXTENSIONS_WITH_ENV: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    let mut s = CONFIG_EXTENSIONS.clone();
    s.insert(".env");
    s
});

static STOPWORDS: Lazy<FxHashSet<&'static str>> = Lazy::new(|| {
    [
        "action",
        "actions",
        "assert",
        "author",
        "before",
        "branch",
        "change",
        "client",
        "config",
        "create",
        "default",
        "delete",
        "deploy",
        "description",
        "enable",
        "engine",
        "engines",
        "export",
        "exports",
        "format",
        "health",
        "ignore",
        "import",
        "inputs",
        "keywords",
        "module",
        "modules",
        "number",
        "object",
        "openapi",
        "option",
        "options",
        "output",
        "outputs",
        "params",
        "plugin",
        "plugins",
        "private",
        "public",
        "remove",
        "render",
        "report",
        "require",
        "result",
        "return",
        "script",
        "scripts",
        "server",
        "source",
        "status",
        "string",
        "target",
        "update",
        "verbose",
        "version",
    ]
    .iter()
    .copied()
    .collect()
});

static YAML_KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*:").unwrap());

static JSON_KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#""([a-zA-Z_][a-zA-Z0-9_-]*)"\s*:"#).unwrap());

static TOML_INI_KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*=").unwrap());

static ENV_KEY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^([A-Za-z_]\w*)\s*=").unwrap());

static PROPERTIES_KEY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?m)^\s*([a-zA-Z_][a-zA-Z0-9_./-]*)\s*[=:]").unwrap());

static XML_ELEMENT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"<([a-zA-Z_][\w.-]*)[>\s/]").unwrap());

static SEPARATOR_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[_-]").unwrap());

fn is_config_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    if ext.is_empty() {
        return false;
    }
    CONFIG_EXTENSIONS_WITH_ENV.contains(ext.as_str())
}

fn is_code_file(path: &Path) -> bool {
    let ext = base::file_ext(path);
    if ext.is_empty() {
        return false;
    }
    CODE_EXTENSIONS.contains(ext.as_str())
}

fn patterns_for_suffix(suffix: &str) -> Vec<&'static Regex> {
    match suffix {
        ".yaml" | ".yml" => vec![&*YAML_KEY_RE],
        ".json" => vec![&*JSON_KEY_RE],
        ".toml" => vec![&*TOML_INI_KEY_RE],
        ".ini" | ".cfg" | ".conf" => vec![&*TOML_INI_KEY_RE],
        ".env" => vec![&*ENV_KEY_RE],
        ".properties" => vec![&*PROPERTIES_KEY_RE],
        ".xml" => vec![&*XML_ELEMENT_RE],
        _ => vec![],
    }
}

fn expand_config_key(key: &str) -> FxHashSet<String> {
    let mut result = FxHashSet::default();
    if key.len() < 2 {
        return result;
    }
    result.insert(key.to_string());
    if key.contains('_') || key.contains('-') {
        let parts: Vec<&str> = SEPARATOR_RE.split(key).collect();
        for p in &parts {
            if p.len() >= 3 {
                result.insert(p.to_string());
            }
        }
        let joined: String = key.chars().filter(|c| *c != '_' && *c != '-').collect();
        if joined.len() >= 4 {
            result.insert(joined);
        }
    }
    result
}

fn extract_config_keys(suffix: &str, content: &str) -> FxHashSet<String> {
    let patterns = patterns_for_suffix(suffix);
    let mut keys = FxHashSet::default();
    for pat in patterns {
        for cap in pat.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                let raw_key = m.as_str().to_lowercase();
                for expanded in expand_config_key(&raw_key) {
                    keys.insert(expanded);
                }
            }
        }
    }
    keys
}

fn build_key_patterns(keys: &FxHashSet<String>) -> Vec<Regex> {
    let mut patterns = Vec::new();
    for key in keys {
        if key.len() >= 4 && !STOPWORDS.contains(key.as_str()) {
            let escaped = regex::escape(key);
            if let Ok(re) = Regex::new(&format!("(?i)\\b{}\\b", escaped)) {
                patterns.push(re);
            }
        }
    }
    patterns
}

fn content_matches_any_pattern(content: &str, patterns: &[Regex]) -> bool {
    for pat in patterns {
        if pat.is_match(content) {
            return true;
        }
    }
    false
}

pub struct ConfigToCodeEdgeBuilder;

impl EdgeBuilder for ConfigToCodeEdgeBuilder {
    fn category_label(&self) -> Option<&str> {
        Some("config_generic")
    }

    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let w = &EDGE_WEIGHTS["config_code"];
        let weight = w.forward;
        let reverse_factor = w.reverse_factor;

        let config_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_config_file(Path::new(f.path())))
            .collect();

        let code_frags: Vec<&Fragment> = fragments
            .iter()
            .filter(|f| is_code_file(Path::new(f.path())))
            .collect();

        if config_frags.is_empty() || code_frags.is_empty() {
            return FxHashMap::default();
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for cfg in &config_frags {
            let suffix = base::file_ext(Path::new(cfg.path()));
            let keys = extract_config_keys(&suffix, &cfg.content);
            if keys.is_empty() {
                continue;
            }
            let patterns = build_key_patterns(&keys);
            if patterns.is_empty() {
                continue;
            }
            for code_frag in &code_frags {
                if content_matches_any_pattern(&code_frag.content, &patterns) {
                    add_edge(&mut edges, &cfg.id, &code_frag.id, weight, reverse_factor);
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
        let config_changed: Vec<&PathBuf> = changed.iter().filter(|f| is_config_file(f)).collect();

        if config_changed.is_empty() {
            return vec![];
        }

        let mut all_keys = FxHashSet::default();
        for cfg_path in &config_changed {
            let content = match base::read_file_cached(cfg_path, file_cache) {
                Some(c) => c,
                None => continue,
            };
            let suffix = base::file_ext(cfg_path);
            for key in extract_config_keys(&suffix, &content) {
                all_keys.insert(key);
            }
        }

        if all_keys.is_empty() {
            return vec![];
        }

        let patterns = build_key_patterns(&all_keys);
        if patterns.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let mut discovered = Vec::new();

        for candidate in candidates {
            if changed_set.contains(candidate) || !is_code_file(candidate) {
                continue;
            }
            let content = match base::read_file_cached(candidate, file_cache) {
                Some(c) => c,
                None => continue,
            };
            if content_matches_any_pattern(&content, &patterns) {
                discovered.push(candidate.clone());
            }
        }

        discovered
    }
}
