//! The SQL cell: routines and views bound by schema and name across migrations
//! (#459). The answers are read off the fixtures; nothing here runs diffctx to
//! learn one.

mod snapshot_fixture;

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name};
use _diffctx::resolve::{Binding, Relation, Resolution, SqlResolver};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag};
use snapshot_fixture::Repo;

struct Fixture {
    _repo: Repo,
    head: SqlResolver,
}

fn snapshot(files: &[(&str, &str)]) -> Fixture {
    let repo = Repo::init();
    for (path, text) in files {
        repo.write(path, text);
    }
    let head = repo.commit("snapshot");
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        RangeSpec::At(Side::Tree(head)),
        RunBudget::default(),
    )
    .expect("an analysis context");
    let resolver =
        SqlResolver::open(&ctx, SideTag::Head, &FactsCache::disabled()).expect("the head resolves");
    Fixture {
        _repo: repo,
        head: resolver,
    }
}

fn dotted(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(Name::as_str).collect();
    parts.push(local.name.as_str());
    parts.join(".")
}

impl Fixture {
    fn where_is(&self, key: &DefKey) -> String {
        let found = self
            .head
            .definitions()
            .iter()
            .find(|d| d.key == *key)
            .expect("a resolved target is a definition of the snapshot");
        format!("{}::{}", found.file, dotted(&key.local))
    }

    /// Every binding of `name` on `line` of `file`, as one line each.
    fn lands(&self, file: &str, name: &str, line: u32) -> Vec<String> {
        let mut found: Vec<String> = self
            .head
            .bindings()
            .iter()
            .filter(|b: &&Binding| {
                b.file.as_str() == file && b.name.as_str() == name && b.site.lines.0 == line
            })
            .map(|b| match &b.resolution {
                Resolution::Resolved { target, .. } => {
                    format!("{:?} {}", b.relation, self.where_is(target))
                }
                Resolution::Candidate { targets, reason } => format!(
                    "candidate({reason:?}) {}",
                    targets
                        .iter()
                        .map(|t| self.where_is(t))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                other => format!("{other:?}"),
            })
            .collect();
        found.sort();
        found
    }
}

const FUNCTION: &str = "CREATE SCHEMA lib;

CREATE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER
LANGUAGE plpgsql AS $$
BEGIN
  RETURN 1;
END;
$$;
";

const USE: &str = "CREATE OR REPLACE FUNCTION lib.refresh_all() RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM lib.refresh_book(1);
END;
$$;

SELECT lib.refresh_book(b.album_id) FROM books b;
";

const DECOYS: &str = "-- lib.refresh_book(x) in a comment
SELECT 'lib.refresh_book(y)' AS s;
SELECT other.refresh_book(1);
CREATE TABLE refresh_book (id int);
DROP FUNCTION IF EXISTS lib.refresh_book(uuid);
";

const VIEW: &str = "CREATE OR REPLACE VIEW lib.book_view AS
  SELECT b.id, lib.refresh_book(b.album) AS n FROM books b;
";

const PROC: &str = "CREATE PROCEDURE lib.rebuild()
LANGUAGE plpgsql AS $body$
BEGIN
  PERFORM refresh_book(NULL);
END;
$body$;
";

fn migrations() -> Fixture {
    snapshot(&[
        ("db/V1__fn.sql", FUNCTION),
        ("db/V2__use.sql", USE),
        ("db/V4__decoys.sql", DECOYS),
        ("db/V5__view.sql", VIEW),
        (
            "db/V6__report.sql",
            "SELECT n FROM lib.book_view WHERE n > 0;\n",
        ),
        ("db/V7__proc.sql", PROC),
    ])
}

const ROUTINE: &str = "db/V1__fn.sql::lib.refresh_book";

#[test]
fn a_qualified_call_lands_on_the_routine_wherever_it_is_made() {
    let fx = migrations();
    let call = format!("Calls {ROUTINE}");
    assert_eq!(
        fx.lands("db/V2__use.sql", "refresh_book", 4),
        [call.clone()]
    );
    assert_eq!(
        fx.lands("db/V2__use.sql", "refresh_book", 8),
        [call.clone()]
    );
    assert_eq!(fx.lands("db/V5__view.sql", "refresh_book", 2), [call]);
}

#[test]
fn an_unqualified_name_against_another_schema_is_the_search_paths_to_say() {
    let fx = migrations();
    assert_eq!(
        fx.lands("db/V7__proc.sql", "refresh_book", 4),
        [format!("candidate(SearchPath) {ROUTINE}")]
    );
}

#[test]
fn comments_strings_declarations_and_another_schema_are_no_use() {
    let fx = migrations();
    let uses: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "db/V4__decoys.sql")
        .collect();
    assert_eq!(uses.len(), 1, "{uses:?}");
    assert!(matches!(uses[0].resolution, Resolution::Unresolved { .. }));
}

#[test]
fn a_view_is_read_by_its_readers() {
    let fx = migrations();
    assert_eq!(
        fx.lands("db/V6__report.sql", "book_view", 1),
        ["Reads db/V5__view.sql::lib.book_view"]
    );
    assert!(
        fx.head
            .bindings()
            .iter()
            .all(|b| b.relation != Relation::Calls
                || !matches!(&b.resolution, Resolution::Resolved { target, .. }
                    if fx.where_is(target).ends_with("book_view")))
    );
}

#[test]
fn a_redefinition_is_the_same_object_and_every_definition_is_reached() {
    let fx = snapshot(&[
        ("db/V1__fn.sql", FUNCTION),
        ("db/V2__use.sql", USE),
        (
            "db/V3__redo.sql",
            "CREATE OR REPLACE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER\nLANGUAGE plpgsql AS $$\nBEGIN\n  RETURN 0;\nEND;\n$$;\n",
        ),
    ]);
    assert_eq!(
        fx.lands("db/V2__use.sql", "refresh_book", 4),
        [
            "Calls db/V1__fn.sql::lib.refresh_book".to_string(),
            "Calls db/V3__redo.sql::lib.refresh_book".to_string()
        ]
    );
}
