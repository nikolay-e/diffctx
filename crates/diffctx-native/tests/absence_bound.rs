// What an absence claim rests on follows the tier of the language it is about:
// the heuristic model for a modelled language whose cell is not yet precise,
// nothing at all for one no model reads (`unknowns.rs` holds that side). The
// static count stays closed either way.

use std::path::Path;

use diffctx_resolve::vocabulary::{Absence, RelationKind, absence_strength, family::Family};

mod support;
use support::{diffctx_unclocked, repo};

fn impact(repo: &Path, format: &str) -> String {
    let out = diffctx_unclocked()
        .current_dir(repo)
        .args([
            "--diff",
            "HEAD~1..HEAD",
            "--mode",
            "impact",
            "-q",
            "-f",
            format,
        ])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

fn impact_json(repo: &Path) -> serde_json::Value {
    serde_json::from_str(&impact(repo, "json")).expect("json")
}

#[test]
fn an_absence_names_the_bound_of_its_languages_tier() {
    let python = repo(
        &[("app/users.py", "def orphan(i):\n    return i\n")],
        &[("app/users.py", "def orphan(i):\n    return i + 1\n")],
    );
    let doc = impact_json(python.path());
    assert_eq!(doc["summary"]["bound"], "model", "{doc:#}");
    assert_eq!(doc["summary"]["callers"]["exact"], true, "{doc:#}");
    let md = impact(python.path(), "md");
    assert!(
        md.contains("0 caller(s) outside the diff (heuristic model)"),
        "{md}"
    );
    assert!(
        md.contains("Changed, no static caller found among the candidates (heuristic model)"),
        "{md}"
    );
    assert!(!md.contains("nothing calls them"), "{md}");

    let go = repo(
        &[(
            "svc/users.go",
            "package svc\n\nfunc Orphan(id int) int {\n\treturn id\n}\n",
        )],
        &[(
            "svc/users.go",
            "package svc\n\nfunc Orphan(id int) int {\n\treturn id + 1\n}\n",
        )],
    );
    assert_ne!(impact_json(go.path())["summary"]["bound"], "precise");
    assert!(!impact(go.path(), "md").contains("nothing calls them"));

    let ruby = repo(
        &[(
            "lib/users.rb",
            "class Users\n  def orphan(id)\n    id\n  end\nend\n",
        )],
        &[(
            "lib/users.rb",
            "class Users\n  def orphan(id)\n    id + 1\n  end\nend\n",
        )],
    );
    let doc = impact_json(ruby.path());
    assert!(doc["summary"].get("bound").is_none(), "{doc:#}");
}

/// The families the engine reads and the README's tiers are two tables: a
/// family a tier row does not name would answer with a model and be told it
/// has none.
#[test]
fn every_family_the_engine_reads_has_a_tier_row() {
    let sample = |family: Family| match family {
        Family::Python => "a.py",
        Family::TypeScript => "a.ts",
        Family::Rust => "a.rs",
        Family::Kotlin => "a.kt",
        Family::Java => "a.java",
        Family::Go => "a.go",
        Family::Php => "a.php",
        Family::C => "a.c",
        Family::Config => "a.yaml",
        Family::Terraform => "a.tf",
        Family::Dockerfile => "Dockerfile",
        Family::Shell => "a.sh",
        Family::Swift => "a.swift",
        Family::Sql => "a.sql",
    };
    for family in Family::ALL {
        let path = sample(family);
        assert!(
            family == Family::Config || family.has_path(path),
            "{path} is no file of {}",
            family.name()
        );
        assert_ne!(
            absence_strength(path, RelationKind::Calls),
            Absence::Never,
            "{} has no row in TIERS",
            family.name()
        );
    }
}
