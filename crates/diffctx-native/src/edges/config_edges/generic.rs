use std::path::{Path, PathBuf};

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`,
/// applied to config keys: document frequency above this is vocabulary.
pub(crate) const MAX_FILES_PER_KEY: usize = 8;

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

fn is_word_byte(b: u8) -> bool {
    // Non-ASCII bytes count as word bytes: the regex `\b` this check replaced
    // treats a Unicode letter as a word character, so `token` inside
    // `tokenización` was NOT a match (#216). Treating every non-ASCII byte as
    // a word byte is the conservative direction — it can only reject matches
    // adjacent to non-ASCII punctuation that `\b` would have accepted, never
    // admit ones it rejected.
    b.is_ascii_alphanumeric() || b == b'_' || !b.is_ascii()
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

        // One automaton over every distinct key instead of per-config regex
        // sweeps of every code fragment: the per-pair form is
        // O(configs x code x patterns) and stood at 42s of a 94s envoy run.
        // Keys whose edge characters are not word characters (or not ASCII)
        // keep the regex path — `\b` anchors relative to those differently
        // than a manual boundary check, and exactness is what makes this a
        // pure speedup.
        let mut key_to_cfgs: FxHashMap<String, Vec<usize>> = FxHashMap::default();
        let mut fallback_patterns: Vec<(Regex, usize)> = Vec::new();
        for (ci, cfg) in config_frags.iter().enumerate() {
            let suffix = base::file_ext(Path::new(cfg.path()));
            for key in extract_config_keys(&suffix, &cfg.content) {
                if key.len() < 4 || STOPWORDS.contains(key.as_str()) {
                    continue;
                }
                let bytes = key.as_bytes();
                let word_edges = key.is_ascii()
                    && is_word_byte(bytes[0])
                    && is_word_byte(bytes[bytes.len() - 1]);
                if word_edges {
                    key_to_cfgs.entry(key.to_lowercase()).or_default().push(ci);
                } else if let Ok(re) = Regex::new(&format!("(?i)\\b{}\\b", regex::escape(&key))) {
                    fallback_patterns.push((re, ci));
                }
            }
        }
        if key_to_cfgs.is_empty() && fallback_patterns.is_empty() {
            return FxHashMap::default();
        }

        let keys: Vec<&String> = key_to_cfgs.keys().collect();
        let automaton = match aho_corasick::AhoCorasick::builder()
            .ascii_case_insensitive(true)
            .build(&keys)
        {
            Ok(ac) => Some(ac),
            Err(e) => {
                // A failed build silences the entire keyed channel (only the
                // fallback regexes still run) — that must never be silent.
                tracing::warn!(
                    "config-edge AhoCorasick build failed over {} keys — keyed channel disabled: {e}",
                    keys.len()
                );
                None
            }
        };

        // Document-frequency gate, same ambiguity bar as
        // `CFamilySemanticWeights::max_files_per_name`: a key matched in more
        // distinct code files than this is vocabulary, not a config-to-code
        // dependency. Without it a lockfile's package names ("react",
        // "lodash") match nearly every source fragment and this builder alone
        // ran past the pipeline deadline on a sentry-scale yarn->pnpm commit
        // (#116) — the emission map, not the automaton, was the cost.
        let mut key_files: Vec<FxHashSet<&str>> = vec![FxHashSet::default(); keys.len()];
        let mut fallback_files: Vec<FxHashSet<&str>> =
            vec![FxHashSet::default(); fallback_patterns.len()];
        for (i, code_frag) in code_frags.iter().enumerate() {
            // This scan alone outran the whole pipeline timeout on a
            // sentry-scale commit (#116); the between-builders deadline check
            // cannot interrupt a single builder, so poll inside the loop.
            crate::deadline::check_current_every(i, 64, "edge construction (config key scan)");
            let content = code_frag.content.as_ref();
            if let Some(ac) = &automaton {
                for m in ac.find_overlapping_iter(content) {
                    let b = content.as_bytes();
                    let before_ok = m.start() == 0 || !is_word_byte(b[m.start() - 1]);
                    let after_ok = m.end() == b.len() || !is_word_byte(b[m.end()]);
                    if before_ok && after_ok {
                        let ki = m.pattern().as_usize();
                        if key_files[ki].len() <= MAX_FILES_PER_KEY {
                            key_files[ki].insert(code_frag.path());
                        }
                    }
                }
            }
            for (fi, (re, _)) in fallback_patterns.iter().enumerate() {
                if fallback_files[fi].len() <= MAX_FILES_PER_KEY && re.is_match(content) {
                    fallback_files[fi].insert(code_frag.path());
                }
            }
        }
        let key_alive: Vec<bool> = key_files
            .iter()
            .map(|s| s.len() <= MAX_FILES_PER_KEY)
            .collect();
        let fallback_alive: Vec<bool> = fallback_files
            .iter()
            .map(|s| s.len() <= MAX_FILES_PER_KEY)
            .collect();

        let mut edges: EdgeDict = FxHashMap::default();
        for (i, code_frag) in code_frags.iter().enumerate() {
            crate::deadline::check_current_every(i, 64, "edge construction (config emission)");
            let content = code_frag.content.as_ref();
            let mut matched_cfgs: FxHashSet<usize> = FxHashSet::default();
            if let Some(ac) = &automaton {
                for m in ac.find_overlapping_iter(content) {
                    let ki = m.pattern().as_usize();
                    if !key_alive[ki] {
                        continue;
                    }
                    let b = content.as_bytes();
                    let before_ok = m.start() == 0 || !is_word_byte(b[m.start() - 1]);
                    let after_ok = m.end() == b.len() || !is_word_byte(b[m.end()]);
                    if before_ok && after_ok {
                        matched_cfgs.extend(key_to_cfgs[keys[ki]].iter());
                    }
                }
            }
            for (fi, (re, ci)) in fallback_patterns.iter().enumerate() {
                if fallback_alive[fi] && !matched_cfgs.contains(ci) && re.is_match(content) {
                    matched_cfgs.insert(*ci);
                }
            }
            for ci in matched_cfgs {
                add_edge(
                    &mut edges,
                    &config_frags[ci].id,
                    &code_frag.id,
                    weight,
                    reverse_factor,
                );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FragmentId, FragmentKind};
    use std::sync::Arc;

    fn frag(path: &str, content: &str) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), 1, 5),
            kind: FragmentKind::Chunk,
            content: Arc::from(content),
            identifiers: FxHashSet::default(),
            token_count: 10,
            symbol_name: None,
        }
    }

    /// #216: the manual boundary check replaced the regex crate's Unicode
    /// `\b`, which treats a non-ASCII letter as a word character. A key ending
    /// exactly where a non-ASCII letter begins is NOT a whole-word match.
    #[test]
    fn a_key_inside_a_non_ascii_word_is_not_a_match() {
        let cfg = frag("app.yaml", "sesion_timeout: 30\n");
        let hit = frag("b.py", "sesion = abrir()\n");
        let miss = frag("a.py", "valor = sesion\u{00e9}s\n");
        let frags = vec![cfg.clone(), hit.clone(), miss.clone()];
        let edges = ConfigToCodeEdgeBuilder.build(&frags, None);
        assert!(
            edges.contains_key(&(cfg.id.clone(), hit.id.clone())),
            "a standalone key occurrence must still link"
        );
        assert!(
            !edges.contains_key(&(cfg.id.clone(), miss.id.clone())),
            "`sesion` inside `sesion\u{00e9}s` is not a word match"
        );
    }
}
