//! What the answer states of a language beyond the cells: the tier table the README
//! reproduces, the relation kinds the case schema names, and the cells made precise.

/// How far an answer about a language is promised (T5). Tier 1 is the
/// declared set whose definitions, callers and test reach are being made
/// precise. Tier 2 is read by a cell or by the ranking graph and answers with
/// candidates. Every other language is Tier 3: not modelled, and no answer about
/// it claims an absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Tier {
    One,
    Two,
}

/// The README's Tier table: a row per language as the README names it, with
/// the language names `languages.rs` gives its files, or the extension of a
/// language it does not name (`.astro`). Tier 3 has no rows.
#[allow(
    dead_code,
    reason = "tests/test_tiers_readme.py reads the README's table off this one"
)]
const TIERS: &[(Tier, &str, &[&str])] = &[
    (Tier::One, "Python", &["python"]),
    (
        Tier::One,
        "TypeScript/JavaScript",
        &["typescript", "tsx", "javascript", "jsx"],
    ),
    (Tier::One, "Rust", &["rust"]),
    (Tier::One, "Kotlin", &["kotlin"]),
    (Tier::Two, "Go", &["go"]),
    (Tier::Two, "Java", &["java"]),
    (Tier::Two, "PHP", &["php"]),
    (Tier::Two, "C", &["c"]),
    (Tier::Two, "C++", &["cpp"]),
    (Tier::Two, "Swift", &["swift"]),
    (Tier::Two, "SQL", &["sql"]),
    (Tier::Two, "shell", &["bash", "zsh"]),
    (Tier::Two, "YAML/k8s/Helm/Argo", &["yaml"]),
    (Tier::Two, "Terraform", &["terraform"]),
    (Tier::Two, "Dockerfile", &["dockerfile"]),
    (Tier::Two, "HTML/Astro", &["html", "astro"]),
];

/// The relations a capability cell is about: the `kind` of a case's
/// `relations` (`tests/cases/impact/SCHEMA.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(
    dead_code,
    reason = "read by the oracle harness once lib.rs re-exports the cells"
)]
pub enum RelationKind {
    Calls,
    Reads,
    Writes,
    Imports,
    Reexports,
    Overrides,
    Instantiates,
    PassesAsValue,
    /// A name written where a type is expected: an annotation, `typeof` in a
    /// type, a type assertion, a type argument, a base class.
    TypeUse,
    Configures,
    Executes,
    ReferencesPath,
    TestsReach,
}

#[allow(
    dead_code,
    reason = "read by the oracle harness once lib.rs re-exports the cells"
)]
impl RelationKind {
    pub const ALL: [Self; 13] = [
        Self::Calls,
        Self::Reads,
        Self::Writes,
        Self::Imports,
        Self::Reexports,
        Self::Overrides,
        Self::Instantiates,
        Self::PassesAsValue,
        Self::TypeUse,
        Self::Configures,
        Self::Executes,
        Self::ReferencesPath,
        Self::TestsReach,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == name)
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::Calls => "calls",
            Self::Reads => "reads",
            Self::Writes => "writes",
            Self::Imports => "imports",
            Self::Reexports => "reexports",
            Self::Overrides => "overrides",
            Self::Instantiates => "instantiates",
            Self::PassesAsValue => "passes_as_value",
            Self::TypeUse => "type_use",
            Self::Configures => "configures",
            Self::Executes => "executes",
            Self::ReferencesPath => "references_path",
            Self::TestsReach => "tests_reach",
        }
    }
}

/// Cells a decision has made `Precise`: the oracle gate passes and the
/// replaced path is deleted. None yet.
const PRECISE: &[(&str, RelationKind)] = &[];

/// Whether the cell is `Precise`: a caller row states its `kind` for exactly
/// these cells.
pub fn is_precise(language: &str, relation: &str) -> bool {
    PRECISE
        .iter()
        .any(|(l, k)| *l == language && k.name() == relation)
}

/// The cell language a file's definitions belong to: the corpus directories.
fn cell_language(path: &str) -> &'static str {
    match crate::languages::get_language_for_file(path) {
        Some("python") => "python",
        Some("typescript" | "tsx" | "javascript" | "jsx") => "typescript",
        Some("rust") => "rust",
        Some("kotlin") => "kotlin",
        Some("java") => "java",
        Some("php") => "php",
        Some("c") => "c",
        Some("cpp") => "cpp",
        Some("swift") => "swift",
        Some("go") => "go",
        Some("terraform") => "terraform",
        Some("bash" | "zsh") => "shell",
        Some("sql") => "sql",
        Some("dockerfile") => "dockerfile",
        Some("yaml" | "json" | "toml" | "ini") => "config",
        Some("html" | "css" | "scss") => "web",
        _ => "other",
    }
}

/// The schema v1 kind of a use a resolver found: construction is
/// `instantiates`, never `calls`, and a name in a type position is `type_use`.
pub(crate) const fn kind_of_use(relation: crate::resolve::Relation) -> RelationKind {
    use crate::resolve::Relation as Use;
    match relation {
        Use::Calls => RelationKind::Calls,
        Use::Reads => RelationKind::Reads,
        Use::Writes => RelationKind::Writes,
        Use::Instantiates => RelationKind::Instantiates,
        Use::PassesAsValue => RelationKind::PassesAsValue,
        Use::Imports => RelationKind::Imports,
        Use::Reexports => RelationKind::Reexports,
        Use::Overrides => RelationKind::Overrides,
        Use::TypeUses => RelationKind::TypeUse,
    }
}

/// The kind a caller row of a definition in `path` states: the use's, when
/// the (language, relation) cell is `Precise`; no other cell says which kind
/// of "called from" a row is.
pub(crate) fn stated_kind_of(path: &str, kind: RelationKind) -> Option<&'static str> {
    stated_kind_by(is_precise, path, kind)
}

fn stated_kind_by(
    precise: impl Fn(&str, &str) -> bool,
    path: &str,
    kind: RelationKind,
) -> Option<&'static str> {
    precise(cell_language(path), kind.name()).then_some(kind.name())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_language_sits_in_two_rows() {
        let mut seen = std::collections::BTreeSet::new();
        for (_, _, names) in TIERS {
            for name in *names {
                assert!(seen.insert(*name), "{name} is in two rows of TIERS");
            }
        }
    }

    #[test]
    fn every_name_is_a_language_the_engine_detects_or_an_extension_it_does_not() {
        let detected: std::collections::BTreeSet<&str> = crate::languages::EXTENSION_TO_LANGUAGE
            .values()
            .copied()
            .collect();
        for (_, label, names) in TIERS {
            for name in *names {
                assert!(
                    detected.contains(name) || *name == "astro",
                    "{label}: {name} names no language"
                );
            }
        }
    }

    #[test]
    fn a_row_states_its_kind_exactly_where_the_registry_says_precise() {
        use crate::resolve::Relation as Use;
        let python_calls =
            |language: &str, relation: &str| language == "python" && relation == "calls";
        let stated = |path, relation| stated_kind_by(python_calls, path, kind_of_use(relation));
        assert_eq!(stated("shop/pricing.py", Use::Calls), Some("calls"));
        assert_eq!(stated("shop/pricing.py", Use::Reads), None);
        assert_eq!(stated("shop/pricing.rs", Use::Calls), None);
        assert_eq!(stated("shop/pricing.py", Use::TypeUses), None);
        let python_types = |language: &str, relation: &str| {
            language == "python" && ["type_use", "instantiates"].contains(&relation)
        };
        let types =
            |relation| stated_kind_by(python_types, "shop/pricing.py", kind_of_use(relation));
        assert_eq!(types(Use::TypeUses), Some("type_use"));
        assert_eq!(types(Use::Instantiates), Some("instantiates"));
        assert_eq!(types(Use::Calls), None, "construction is never a call");
        for (path, relation) in [
            ("a.py", Use::Calls),
            ("a.ts", Use::Reads),
            ("a.rs", Use::Overrides),
        ] {
            assert_eq!(
                stated_kind_of(path, kind_of_use(relation)),
                None,
                "{path}: no cell is precise yet"
            );
        }
    }

    #[test]
    fn each_relation_is_spelled_once_and_every_variant_round_trips() {
        for relation in crate::resolve::Relation::ALL {
            let kind = kind_of_use(relation);
            assert_eq!(kind.name(), relation.name(), "{relation:?}");
            assert_eq!(RelationKind::parse(kind.name()), Some(kind));
        }
        for kind in RelationKind::ALL {
            assert_eq!(RelationKind::parse(kind.name()), Some(kind), "{kind:?}");
        }
        let names: std::collections::BTreeSet<&str> =
            RelationKind::ALL.iter().map(|k| k.name()).collect();
        assert_eq!(
            names.len(),
            RelationKind::ALL.len(),
            "a name is spelled twice"
        );
    }

    #[test]
    fn tier_one_is_exactly_the_declared_set() {
        let one: Vec<&str> = TIERS
            .iter()
            .filter(|(tier, _, _)| *tier == Tier::One)
            .map(|(_, label, _)| *label)
            .collect();
        assert_eq!(one, ["Python", "TypeScript/JavaScript", "Rust", "Kotlin"]);
    }
}
