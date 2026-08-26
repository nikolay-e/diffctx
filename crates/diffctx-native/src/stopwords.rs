use once_cell::sync::Lazy;
use rustc_hash::FxHashSet;

use crate::config::extensions::{CODE_EXTENSIONS, DOC_EXTENSIONS};

fn set_from_list(list: &str) -> FxHashSet<String> {
    list.split_ascii_whitespace().map(str::to_string).collect()
}

// The word lists live in .txt next door rather than as array literals: rustfmt
// packs a short array and gives up on a long one, so 928 literals occupied 865
// lines here. They are unioned into a set, so their order and any duplicate
// across the old three-way code split are unobservable — which is what makes
// the move to a flat file bit-equivalent.
pub static CODE_STOPWORDS: Lazy<FxHashSet<String>> =
    Lazy::new(|| set_from_list(include_str!("stopwords/code.txt")));

static DOCS_STOPWORDS: Lazy<FxHashSet<String>> =
    Lazy::new(|| set_from_list(include_str!("stopwords/docs.txt")));

pub const PROFILE_CODE: &str = "code";
pub const PROFILE_DOCS: &str = "docs";
pub const PROFILE_LEGAL: &str = "legal";
pub const PROFILE_DATA: &str = "data";
pub const PROFILE_GENERIC: &str = "generic";

static EMPTY_STOPWORDS: Lazy<FxHashSet<String>> = Lazy::new(FxHashSet::default);

pub fn get_stopwords(profile: &str) -> &FxHashSet<String> {
    match profile {
        PROFILE_CODE | PROFILE_GENERIC => &CODE_STOPWORDS,
        PROFILE_DOCS | PROFILE_LEGAL => &DOCS_STOPWORDS,
        PROFILE_DATA => &EMPTY_STOPWORDS,
        _ => &CODE_STOPWORDS,
    }
}

pub fn profile_from_path(path: &str) -> &'static str {
    let p = std::path::Path::new(path);
    let suffix = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    let name_lower = p
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    if CODE_EXTENSIONS.contains(suffix.as_str()) {
        return PROFILE_CODE;
    }

    if DOC_EXTENSIONS.contains(suffix.as_str()) || suffix == ".markdown" || suffix == ".tex" {
        return PROFILE_DOCS;
    }

    let data_exts = [
        ".csv", ".json", ".jsonl", ".yaml", ".yml", ".toml", ".xml", ".ini", ".env",
    ];
    if data_exts.contains(&suffix.as_str()) {
        return PROFILE_DATA;
    }

    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let legal_names = [
        "license",
        "licence",
        "legal",
        "terms",
        "agreement",
        "contract",
        "policy",
        "privacy",
        "tos",
        "eula",
    ];
    if legal_names.contains(&stem.as_str())
        || name_lower.contains("license")
        || name_lower.contains("legal")
        || name_lower.contains("terms")
    {
        return PROFILE_LEGAL;
    }

    PROFILE_GENERIC
}

pub fn is_reasonable_ident(ident: &str, min_len: usize, profile: &str) -> bool {
    if ident.is_empty() || ident.len() < min_len {
        return false;
    }
    let low = ident.to_lowercase();
    let stopwords = get_stopwords(profile);
    if stopwords.contains(&low) {
        return false;
    }
    if low.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    true
}

pub fn filter_idents(idents: &[String], min_len: usize, profile: &str) -> Vec<String> {
    idents
        .iter()
        .filter(|s| is_reasonable_ident(s, min_len, profile))
        .cloned()
        .collect()
}
