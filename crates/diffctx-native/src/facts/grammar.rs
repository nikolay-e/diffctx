//! Whether a newer release of a pinned grammar reads a blob the pinned one
//! cannot. The answer is witnessed, not computed: the shipped build holds one
//! grammar per language, so the bump job (`scripts/bump-grammars.sh`) parses the
//! files it can see with the pinned and with the newest releases and records the
//! blobs that moved from errors to clean in `grammar_witnesses.txt`. A blob that
//! file does not name is a plain parse error: the build has no witness that a
//! newer grammar would read it.

use std::collections::HashSet;
use std::sync::OnceLock;

use crate::facts::language::Language;
use crate::snapshot::Oid;

const WITNESSES: &str = include_str!("grammar_witnesses.txt");

fn witnessed() -> &'static HashSet<&'static str> {
    static SET: OnceLock<HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        WITNESSES
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect()
    })
}

/// Whether the bump job saw a newer release of `language`'s grammar read `blob`.
pub(super) fn newer_grammar_reads(language: Language, blob: &Oid) -> bool {
    witnessed().contains(format!("{language:?} {blob}").as_str())
}
