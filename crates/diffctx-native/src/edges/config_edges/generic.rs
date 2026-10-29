use std::path::{Path, PathBuf};

/// Same ambiguity bar as `CFamilySemanticWeights::max_files_per_name`,
/// applied to config keys: document frequency above this is vocabulary.
pub(crate) const MAX_FILES_PER_KEY: usize = 8;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::canon::{KeyCache, canon_tokens, for_each_name_key};
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

/// The canonical keys one config key stands for: itself, whole, and, for a
/// name made of words (`max_pool_size`, `max-pool-size`, `maxPoolSize`), each
/// of its words. A key that is a path or a dotted setting (`spring.datasource.url`)
/// stands only for itself.
fn expand_config_key(raw: &str) -> Vec<(String, bool)> {
    let tokens = canon_tokens(raw);
    let whole = tokens.join("_");
    if whole.len() < 2 {
        return Vec::new();
    }
    let mut keys = vec![(whole, true)];
    let words_only = !raw.contains(['.', '/', ':']);
    if tokens.len() > 1 && words_only && raw.contains(['_', '-']) {
        keys.extend(
            tokens
                .into_iter()
                .filter(|t| t.len() >= 3)
                .map(|t| (t, false)),
        );
    }
    keys
}

/// Every key a config file spells, its sub-words included, each marked
/// whether the file spells it whole.
fn extract_config_keys(suffix: &str, content: &str) -> FxHashMap<String, bool> {
    let patterns = patterns_for_suffix(suffix);
    let mut keys: FxHashMap<String, bool> = FxHashMap::default();
    for pat in patterns {
        for cap in pat.captures_iter(content) {
            if let Some(m) = cap.get(1) {
                for (key, whole) in expand_config_key(m.as_str()) {
                    *keys.entry(key).or_default() |= whole;
                }
            }
        }
    }
    keys
}

pub struct ConfigToCodeEdgeBuilder;

impl EdgeBuilder for ConfigToCodeEdgeBuilder {
    fn category_label(&self) -> Option<&str> {
        Some("key_match")
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

        let mut key_to_cfgs: FxHashMap<String, Vec<usize>> = FxHashMap::default();
        let mut spelled_whole: FxHashSet<String> = FxHashSet::default();
        for (ci, cfg) in config_frags.iter().enumerate() {
            let suffix = base::file_ext(Path::new(cfg.path()));
            for (key, whole) in extract_config_keys(&suffix, &cfg.content) {
                if key.len() < 4 || STOPWORDS.contains(key.as_str()) {
                    continue;
                }
                if whole {
                    spelled_whole.insert(key.clone());
                }
                key_to_cfgs.entry(key).or_default().push(ci);
            }
        }
        // The ambiguity bar holds on the config side too: a sub-word most
        // config files carry (`workflow_dispatch` -> `workflow` in every CI
        // workflow) names no one file, and every source file saying the word
        // was linked to all of them (#297). A key spelled whole is the
        // setting itself: `database_url` in two of four environment files
        // still names what the code reads.
        let config_files: FxHashSet<&str> = config_frags.iter().map(|f| f.path()).collect();
        let per_key_files = |cis: &[usize]| {
            cis.iter()
                .map(|&ci| config_frags[ci].path())
                .collect::<FxHashSet<&str>>()
                .len()
        };
        let vocabulary = |n: usize, sub_word: bool| {
            n > MAX_FILES_PER_KEY
                || (sub_word && config_files.len() >= 4 && n * 4 > config_files.len())
        };
        key_to_cfgs
            .retain(|key, cis| !vocabulary(per_key_files(cis), !spelled_whole.contains(key)));
        if key_to_cfgs.is_empty() {
            return FxHashMap::default();
        }

        let keys: Vec<&String> = key_to_cfgs.keys().collect();
        let key_index: FxHashMap<&str, usize> = keys
            .iter()
            .enumerate()
            .map(|(i, k)| (k.as_str(), i))
            .collect();

        // Which keys each code fragment spells, read once: the document
        // frequency below and the emission after it share the scan.
        let mut cache = KeyCache::default();
        let mut hits: Vec<(usize, Vec<usize>)> = Vec::new();
        for (i, code_frag) in code_frags.iter().enumerate() {
            // This scan alone outran the whole pipeline timeout on a
            // sentry-scale commit (#116); the between-builders deadline check
            // cannot interrupt a single builder, so poll inside the loop.
            if !crate::resource::poll_current_every(i, 64) {
                break;
            }
            let mut spelled: FxHashSet<usize> = FxHashSet::default();
            for_each_name_key(code_frag.content.as_ref(), &mut cache, |k| {
                if let Some(&ki) = key_index.get(k) {
                    spelled.insert(ki);
                }
            });
            if !spelled.is_empty() {
                hits.push((i, spelled.into_iter().collect()));
            }
        }

        // Document-frequency gate, same ambiguity bar as
        // `CFamilySemanticWeights::max_files_per_name`: a key matched in more
        // distinct code files than this is vocabulary, not a config-to-code
        // dependency. Without it a lockfile's package names ("react",
        // "lodash") match nearly every source fragment and this builder alone
        // ran past the pipeline deadline on a sentry-scale yarn->pnpm commit
        // (#116) — the emission map, not the scan, was the cost.
        let mut key_files: Vec<FxHashSet<&str>> = vec![FxHashSet::default(); keys.len()];
        for (i, spelled) in &hits {
            for &ki in spelled {
                if key_files[ki].len() <= MAX_FILES_PER_KEY {
                    key_files[ki].insert(code_frags[*i].path());
                }
            }
        }
        let key_alive: Vec<bool> = key_files
            .iter()
            .map(|s| s.len() <= MAX_FILES_PER_KEY)
            .collect();

        let mut edges: EdgeDict = FxHashMap::default();
        let mut reported = 0u64;
        for (n, (i, spelled)) in hits.iter().enumerate() {
            if !crate::resource::poll_emissions(n, 64, edges.len() as u64, &mut reported) {
                break;
            }
            let code_frag = code_frags[*i];
            let mut matched_cfgs: FxHashSet<usize> = FxHashSet::default();
            for &ki in spelled {
                if key_alive[ki] {
                    matched_cfgs.extend(key_to_cfgs[keys[ki]].iter());
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

        let mut all_keys: FxHashSet<String> = FxHashSet::default();
        for cfg_path in &config_changed {
            let content = match base::read_file_cached(cfg_path, file_cache) {
                Some(c) => c,
                None => continue,
            };
            let suffix = base::file_ext(cfg_path);
            all_keys.extend(
                extract_config_keys(&suffix, &content)
                    .into_keys()
                    .filter(|k| k.len() >= 4 && !STOPWORDS.contains(k.as_str())),
            );
        }

        if all_keys.is_empty() {
            return vec![];
        }

        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let mut discovered = Vec::new();
        let mut cache = KeyCache::default();

        for candidate in candidates {
            if changed_set.contains(candidate) || !is_code_file(candidate) {
                continue;
            }
            let content = match base::read_file_cached(candidate, file_cache) {
                Some(c) => c,
                None => continue,
            };
            let mut spelled = false;
            for_each_name_key(&content, &mut cache, |k| spelled |= all_keys.contains(k));
            if spelled {
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

    /// One setting is spelled three ways across a YAML key, a TypeScript
    /// property and a Python constant; a name that merely shares two of its
    /// words is another setting.
    #[test]
    fn one_setting_in_every_spelling_links_config_to_code() {
        let cfg = frag("app.yaml", "max-pool-size: 10\n");
        let camel = frag("a.ts", "const n = cfg.maxPoolSize;\n");
        let snake = frag("b.py", "MAX_POOL_SIZE = 1\n");
        let other = frag("c.py", "pool_size = 2\n");
        let frags = vec![cfg.clone(), camel.clone(), snake.clone(), other.clone()];
        let edges = ConfigToCodeEdgeBuilder.build(&frags, None);
        assert!(edges.contains_key(&(cfg.id.clone(), camel.id.clone())));
        assert!(edges.contains_key(&(cfg.id.clone(), snake.id.clone())));
        assert!(!edges.contains_key(&(cfg.id.clone(), other.id.clone())));
    }
}
