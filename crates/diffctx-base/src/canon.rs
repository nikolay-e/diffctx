//! One canonical spelling for names that cross languages and file formats.
//!
//! `DATABASE_URL` in a `.env`, `databaseUrl` in a TypeScript config,
//! `database-url` in a YAML key and `HTTPServer` against `http_server` name the
//! same thing and share no bytes. A name is split at everything that is not
//! alphanumeric and at case boundaries, lowercased, and its tokens joined by
//! `_`: that string is the key every channel that matches names across files
//! compares on (the config-to-code builder, the tags fallback, the typed key
//! table).

use rustc_hash::FxHashMap;

fn is_plain_canon(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('_')
        && !name.ends_with('_')
        && !name.contains("__")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `maxPoolSize` -> `max pool size`; `HTTPServer` -> `http server`;
/// `sha256Hash` -> `sha256 hash`. Digits stay with the letters before them.
pub fn canon_tokens(name: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for segment in name.split(|c: char| !c.is_alphanumeric()) {
        if segment.is_empty() {
            continue;
        }
        if segment
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            tokens.push(segment.to_string());
            continue;
        }
        let chars: Vec<char> = segment.chars().collect();
        let mut start = 0;
        for i in 1..chars.len() {
            let (prev, cur) = (chars[i - 1], chars[i]);
            let boundary = cur.is_uppercase()
                && (prev.is_lowercase()
                    || prev.is_ascii_digit()
                    || (prev.is_uppercase() && chars.get(i + 1).is_some_and(|n| n.is_lowercase())));
            if boundary {
                tokens.push(chars[start..i].iter().collect::<String>().to_lowercase());
                start = i;
            }
        }
        if start < chars.len() {
            tokens.push(chars[start..].iter().collect::<String>().to_lowercase());
        }
    }
    tokens
}

/// The tokens joined by `_`; empty when the name holds no alphanumeric.
pub fn canon_key(name: &str) -> String {
    if is_plain_canon(name) {
        return name.to_string();
    }
    canon_tokens(name).join("_")
}

/// Segment -> canonical key, kept across the fragments of one build: the same
/// few thousand words recur in every file.
#[derive(Default)]
pub struct KeyCache {
    index: FxHashMap<String, usize>,
    keys: Vec<String>,
}

impl KeyCache {
    pub fn key(&mut self, segment: &str) -> &str {
        if let Some(&at) = self.index.get(segment) {
            return &self.keys[at];
        }
        self.index.insert(segment.to_string(), self.keys.len());
        self.keys.push(canon_key(segment));
        &self.keys[self.keys.len() - 1]
    }
}

/// The longest run of dotted or dashed segments (`spring.datasource.url`,
/// `--max-pool-size`) whose sub-runs are also keys: a settings key is spelled
/// whole in one place and as its tail in another.
const MAX_RUN_SEGMENTS: usize = 6;

/// Non-ASCII bytes belong to a word: `sesion` inside `sesionés` is not the
/// word `sesion` (#216).
fn is_segment_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || !b.is_ascii()
}

/// The words of `text` as byte ranges, with the end of each run of words
/// joined by single dots or dashes marked.
fn for_each_run(text: &str, mut run: impl FnMut(&[(usize, usize)])) {
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut segments: Vec<(usize, usize)> = Vec::new();
    while i < bytes.len() {
        if !is_segment_byte(bytes[i]) {
            i += 1;
            continue;
        }
        segments.clear();
        loop {
            let start = i;
            while i < bytes.len() && is_segment_byte(bytes[i]) {
                i += 1;
            }
            segments.push((start, i));
            let joined = i + 1 < bytes.len()
                && matches!(bytes[i], b'.' | b'-')
                && is_segment_byte(bytes[i + 1]);
            if !joined {
                break;
            }
            i += 1;
        }
        run(&segments);
    }
}

/// Every canonical key `text` spells as a setting name: each word, and each
/// run of words joined by single dots or dashes with its contiguous sub-runs.
/// The same key may be offered more than once.
pub fn for_each_name_key(text: &str, cache: &mut KeyCache, mut sink: impl FnMut(&str)) {
    let mut keys: Vec<String> = Vec::new();
    let mut joined = String::new();
    for_each_run(text, |segments| {
        if let [(s, e)] = segments {
            let key = cache.key(&text[*s..*e]);
            if !key.is_empty() {
                sink(key);
            }
            return;
        }
        keys.clear();
        keys.extend(
            segments
                .iter()
                .map(|&(s, e)| cache.key(&text[s..e]).to_string()),
        );
        for from in 0..keys.len() {
            joined.clear();
            for key in keys.iter().skip(from).take(MAX_RUN_SEGMENTS) {
                if key.is_empty() {
                    break;
                }
                if !joined.is_empty() {
                    joined.push('_');
                }
                joined.push_str(key);
                sink(&joined);
            }
        }
    });
}

/// The canonical key of every identifier `text` spells (a word that starts
/// with a letter or an underscore), each possibly more than once.
pub fn for_each_word_key(text: &str, cache: &mut KeyCache, mut sink: impl FnMut(&str)) {
    for_each_run(text, |segments| {
        for &(s, e) in segments {
            let word = &text[s..e];
            if word.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            let key = cache.key(word);
            if !key.is_empty() {
                sink(key);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_name_in_every_spelling_has_one_key() {
        for spelling in [
            "DATABASE_URL",
            "databaseUrl",
            "DatabaseURL",
            "database-url",
            "database.url",
            "database_url",
        ] {
            assert_eq!(canon_key(spelling), "database_url", "{spelling}");
        }
    }

    #[test]
    fn case_boundaries_split_acronyms_and_digits() {
        assert_eq!(canon_key("HTTPServer"), "http_server");
        assert_eq!(canon_key("maxPoolSize"), "max_pool_size");
        assert_eq!(canon_key("sha256Hash"), "sha256_hash");
        assert_eq!(canon_key("V2Api"), "v2_api");
        assert_eq!(canon_key("__init__"), "init");
        assert_eq!(canon_key("___"), "");
    }

    #[test]
    fn only_a_canonical_name_is_its_own_key() {
        assert_eq!(canon_key("max_pool_size"), "max_pool_size");
        assert_eq!(canon_key("sha256"), "sha256");
        assert_eq!(canon_key("_max__pool_"), "max_pool");
        assert_eq!(canon_key("max_Pool"), "max_pool");
        assert_eq!(canon_key("naïve_name"), "naïve_name");
        assert_eq!(canon_key(""), "");
    }

    #[test]
    fn a_lone_word_offers_its_key_unless_it_has_none() {
        let mut cache = KeyCache::default();
        let mut keys = Vec::new();
        for_each_name_key("x y1 _ Z", &mut cache, |k| keys.push(k.to_string()));
        assert_eq!(
            keys,
            vec!["x".to_string(), "y1".to_string(), "z".to_string()]
        );
    }

    #[test]
    fn a_dotted_run_offers_its_sub_runs() {
        let mut cache = KeyCache::default();
        let mut keys = Vec::new();
        for_each_name_key("x = spring.datasource.url + 1", &mut cache, |k| {
            keys.push(k.to_string())
        });
        for expected in [
            "spring",
            "spring_datasource",
            "spring_datasource_url",
            "datasource",
            "datasource_url",
            "url",
        ] {
            assert!(keys.iter().any(|k| k == expected), "{expected} in {keys:?}");
        }
        assert!(!keys.iter().any(|k| k == "spring_url"));
    }

    #[test]
    fn a_dash_between_spaces_joins_nothing() {
        let mut cache = KeyCache::default();
        let mut keys = Vec::new();
        for_each_name_key("a - b", &mut cache, |k| keys.push(k.to_string()));
        assert_eq!(keys, vec!["a".to_string(), "b".to_string()]);
    }
}
