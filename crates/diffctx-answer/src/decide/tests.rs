use diffctx_resolve::vocabulary::family::{Family, Group};

use super::gap::{Gap, Reason};

#[test]
fn a_file_belongs_to_the_group_of_its_extension() {
    let groups = [
        ("a.kt", Group::Jvm),
        ("A.java", Group::Jvm),
        ("A.scala", Group::Jvm),
        ("a.groovy", Group::Jvm),
        ("a.c", Group::Native),
        ("a.cpp", Group::Native),
        ("a.m", Group::Native),
        ("a.ts", Group::Web),
        ("a.js", Group::Web),
        ("a.vue", Group::Web),
        ("a.svelte", Group::Web),
        ("a.html", Group::Web),
        ("a.astro", Group::Web),
        ("a.py", Group::Family(Family::Python)),
        ("a.rs", Group::Family(Family::Rust)),
        ("values.yaml", Group::Family(Family::Config)),
        ("main.tf", Group::Family(Family::Terraform)),
        ("Dockerfile", Group::Family(Family::Dockerfile)),
        ("a.sh", Group::Family(Family::Shell)),
        ("a.swift", Group::Family(Family::Swift)),
        ("a.sql", Group::Family(Family::Sql)),
        ("a.rb", Group::Language("ruby")),
    ];
    for (path, group) in groups {
        assert_eq!(Group::of_name(path), Some(group), "{path}");
        assert!(group.has_path(path), "{path}");
    }
    assert_eq!(Group::of_name("LICENSE"), None);
}

#[test]
fn a_gap_of_an_empty_group_counts_no_file() {
    let gap = Gap::Unaccounted {
        reason: Reason::NoGroup,
        files: 0,
    };
    for said in [
        gap.to_string(),
        crate::answer::v2::Gap::of(&gap, "x").to_string(),
    ] {
        assert!(!said.contains("0 file"), "{said}");
        assert!(said.contains("in no group of languages"), "{said}");
    }
}

#[test]
fn a_family_stands_in_the_group_it_names() {
    for family in Family::ALL {
        let group = family.group();
        assert!(
            matches!(group, Group::Family(f) if f == family)
                || matches!(
                    (group, family),
                    (Group::Jvm, Family::Kotlin | Family::Java)
                        | (Group::Native, Family::C)
                        | (Group::Web, Family::TypeScript)
                ),
            "{family:?}"
        );
    }
}
