//! The last pass over every public surface: text that carries a
//! high-confidence credential shape leaves with the credential replaced by
//! `[REDACTED:<category>]`, and the artifact says how many times.
//!
//! This is defence in depth, not a boundary. The withhold policy keeps
//! secret-by-name files (`id_rsa`, `*.pem`, `.env`-style paths the ignore
//! rules cover) out of every surface; this pass catches the token pasted
//! into a source file that policy could not know about. Only shapes that
//! cannot plausibly be anything else are matched — a generic
//! `password = "..."` is not — so a clean pass proves nothing (see
//! SECURITY.md).

use once_cell::sync::Lazy;
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;

struct Shape {
    category: &'static str,
    re: Lazy<Regex>,
}

macro_rules! shape {
    ($category:literal, $re:literal) => {
        Shape {
            category: $category,
            re: Lazy::new(|| Regex::new($re).expect($category)),
        }
    };
}

static SHAPES: [Shape; 8] = [
    shape!(
        "private_key",
        r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)"
    ),
    shape!("aws_access_key", r"\bAKIA[0-9A-Z]{16}\b"),
    shape!(
        "github_token",
        r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})\b"
    ),
    shape!("slack_token", r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b"),
    shape!("stripe_key", r"\b[sr]k_(?:live|test)_[0-9A-Za-z]{16,}\b"),
    shape!("google_api_key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
    shape!(
        "jwt",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"
    ),
    shape!("openai_key", r"\bsk-(?:proj-)?[A-Za-z0-9_-]{32,}\b"),
];

/// What the sanitizer removed from one artifact.
#[derive(Serialize, JsonSchema, Clone, Debug, Default, PartialEq, Eq)]
pub struct Redactions {
    pub count: usize,
    /// Distinct shapes matched, in the order first seen.
    pub categories: Vec<&'static str>,
}

impl Redactions {
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn merge(&mut self, other: Redactions) {
        self.count += other.count;
        for c in other.categories {
            if !self.categories.contains(&c) {
                self.categories.push(c);
            }
        }
    }
}

pub fn sanitize(text: &str) -> (String, Redactions) {
    let mut out = String::from(text);
    let mut redactions = Redactions::default();
    for shape in &SHAPES {
        let hits = shape.re.find_iter(&out).count();
        if hits == 0 {
            continue;
        }
        redactions.count += hits;
        redactions.categories.push(shape.category);
        let replacement = format!("[REDACTED:{}]", shape.category);
        out = shape
            .re
            .replace_all(&out, replacement.as_str())
            .into_owned();
    }
    (out, redactions)
}

/// `sanitize` for an owned string that is usually clean: no allocation
/// unless something matched.
pub fn sanitize_in_place(text: &mut String) -> Redactions {
    if SHAPES.iter().all(|s| !s.re.is_match(text)) {
        return Redactions::default();
    }
    let (clean, redactions) = sanitize(text);
    *text = clean;
    redactions
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures are assembled at runtime so the repository's own secret
    // scanners do not trip on the known-bad input they exist to prove.
    const PARTS: [(&str, &str); 8] = [
        ("key = 'AKIA{}'", "IOSFODNN7EXAMPLE"),
        ("gh = 'ghp_{}'", "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghij1234"), // pragma: allowlist secret
        ("slack = 'xoxb-{}'", "1234567890-abcdefghij"),
        ("stripe = 'sk_live_{}'", "ABCDEFGHIJKLMNOPQRSTUV"),
        (
            "-----BEGIN RSA PRIVATE {}-----\nMIIB\n-----END RSA PRIVATE {}-----",
            "KEY",
        ),
        (
            "jwt = 'eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.{}'", // pragma: allowlist secret
            "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U", // pragma: allowlist secret
        ),
        ("g = 'AIza{}'", "SyA-1234567890abcdefghijklmnopqrstu"), // pragma: allowlist secret
        (
            "oa = 'sk-proj-{}'",
            "abcdefghijklmnopqrstuvwxyz0123456789ABCD",
        ), // pragma: allowlist secret
    ];

    #[test]
    fn every_shape_is_caught_and_named() {
        let planted: String = PARTS
            .iter()
            .map(|(shape, secret)| shape.replace("{}", secret) + "\n")
            .collect();
        let (clean, r) = sanitize(&planted);
        for (i, (_, secret)) in PARTS.iter().enumerate() {
            assert!(!clean.contains(secret), "planted shape {i} survived");
        }
        assert!(!clean.contains("MIIB"));
        assert_eq!(r.count, 8);
        assert_eq!(r.categories.len(), 8);
        assert!(clean.contains("key = '[REDACTED:aws_access_key]'"));
        assert!(clean.contains("[REDACTED:private_key]"));
    }

    #[test]
    fn an_unterminated_private_key_is_cut_to_the_end_of_the_text() {
        let text = format!(
            "head\n-----BEGIN {}-----\nMIIE\nstill secret",
            "PRIVATE KEY"
        );
        let (clean, r) = sanitize(&text);
        assert_eq!(clean, "head\n[REDACTED:private_key]");
        assert_eq!(r.count, 1);
    }

    #[test]
    fn ordinary_source_is_left_alone() {
        let src = "def token_for(user):\n    return sign(user, key=settings.SECRET)\nAKIA = 'not a key'\nsk_live = None\n";
        let (clean, r) = sanitize(src);
        assert_eq!(clean, src);
        assert!(r.is_empty());
        let mut owned = src.to_string();
        assert!(sanitize_in_place(&mut owned).is_empty());
        assert_eq!(owned, src);
    }

    #[test]
    fn merge_counts_and_keeps_categories_distinct() {
        let mut a = Redactions {
            count: 1,
            categories: vec!["aws_access_key"],
        };
        a.merge(Redactions {
            count: 2,
            categories: vec!["aws_access_key", "jwt"],
        });
        assert_eq!(a.count, 3);
        assert_eq!(a.categories, vec!["aws_access_key", "jwt"]);
    }
}
