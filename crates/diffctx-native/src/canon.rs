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

/// `maxPoolSize` -> `max pool size`; `HTTPServer` -> `http server`;
/// `sha256Hash` -> `sha256 hash`. Digits stay with the letters before them.
pub(crate) fn canon_tokens(name: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for segment in name.split(|c: char| !c.is_alphanumeric()) {
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
pub(crate) fn canon_key(name: &str) -> String {
    canon_tokens(name).join("_")
}

/// Segment -> canonical key, kept across the fragments of one build: the same
/// few thousand words recur in every file.
#[derive(Default)]
pub(crate) struct KeyCache(FxHashMap<String, String>);

impl KeyCache {
    pub(crate) fn key(&mut self, segment: &str) -> &str {
        if !self.0.contains_key(segment) {
            self.0.insert(segment.to_string(), canon_key(segment));
        }
        &self.0[segment]
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
pub(crate) fn for_each_name_key(text: &str, cache: &mut KeyCache, mut sink: impl FnMut(&str)) {
    for_each_run(text, |segments| {
        let keys: Vec<String> = segments
            .iter()
            .map(|&(s, e)| cache.key(&text[s..e]).to_string())
            .collect();
        for from in 0..keys.len() {
            let mut joined = String::new();
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
pub(crate) fn for_each_word_key(text: &str, cache: &mut KeyCache, mut sink: impl FnMut(&str)) {
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
