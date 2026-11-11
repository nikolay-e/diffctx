//! The SQL extractor: routines and views are definitions, calls of routines and
//! reads of views are references, and comments, strings and the head of a
//! definition are neither (#459).

use _diffctx::facts::lang::sql::SqlExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Extractor, Fact, Language, Namespace, Ref, Scope, UseKind,
};
use _diffctx::snapshot::RunBudget;

fn extract(source: &str) -> BlobFacts {
    SqlExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn refs(blob: &BlobFacts) -> Vec<&Ref> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Reference(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn def<'a>(blob: &'a BlobFacts, name: &str) -> &'a Def {
    blob.defs()
        .find(|d| d.local.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name}"))
}

const FUNCTION: &str = "CREATE SCHEMA lib;

CREATE FUNCTION lib.refresh_book(target_album UUID) RETURNS INTEGER
LANGUAGE plpgsql AS $$
DECLARE
  n INTEGER;
BEGIN
  UPDATE books SET refreshed = now() WHERE album = target_album;
  RETURN n;
END;
$$;
";

const USE: &str = "CREATE OR REPLACE FUNCTION lib.refresh_all() RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
  FOR album IN SELECT id FROM albums LOOP
    PERFORM lib.refresh_book(album);
  END LOOP;
END;
$$;

SELECT lib.refresh_book(b.album_id) FROM books b;
";

#[test]
fn a_routine_is_a_function_named_in_its_schema_over_its_whole_statement() {
    let blob = extract(FUNCTION);
    assert_eq!(blob.language, Language::Sql);
    let d = def(&blob, "refresh_book");
    assert_eq!(d.kind, DefKind::Function);
    assert_eq!(d.local.container[0].as_str(), "lib");
    assert_eq!(d.local.ns, Namespace::Value);
    assert_eq!(d.span.lines, (3, 11));
    assert_eq!(d.name_span.lines, (3, 3));
    assert_eq!(
        d.signature.as_deref(),
        Some(
            "CREATE FUNCTION lib . refresh_book ( target_album UUID ) RETURNS INTEGER LANGUAGE plpgsql"
        )
    );
}

#[test]
fn a_redefinition_is_the_same_signature_without_or_replace() {
    let replaced = extract("CREATE OR REPLACE FUNCTION f() RETURNS int AS $$ SELECT 1 $$;");
    let created = extract("CREATE FUNCTION f() RETURNS int AS $$ SELECT 1 $$;");
    assert_eq!(def(&replaced, "f").signature, def(&created, "f").signature);
    assert_eq!(def(&replaced, "f").body_hash, def(&created, "f").body_hash);
}

#[test]
fn a_call_in_a_body_and_one_at_the_top_are_owned_by_the_body_and_the_module() {
    let blob = extract(USE);
    let calls: Vec<(u32, String)> = refs(&blob)
        .into_iter()
        .filter(|r| r.use_kind == UseKind::Call)
        .map(|r| {
            let owner = match &r.enclosing {
                Scope::Module => "module".to_string(),
                Scope::Def(key) => key.name.as_str().to_string(),
                Scope::Callback { .. } => "callback".to_string(),
            };
            (r.site.lines.0, owner)
        })
        .collect();
    assert_eq!(
        calls,
        [(5, "refresh_all".to_string()), (10, "module".to_string())]
    );
    let call = refs(&blob)
        .into_iter()
        .find(|r| r.site.lines.0 == 5)
        .expect("the call");
    assert_eq!(call.name.as_str(), "refresh_book");
    assert_eq!(call.qualifier[0].as_str(), "lib");
}

#[test]
fn comments_strings_declarations_and_dollar_quoted_literals_name_nothing() {
    let blob = extract(
        "-- lib.refresh_book(x) in a comment
SELECT 'lib.refresh_book(y)' AS s, $tag$lib.refresh_book(z)$tag$ AS t;
CREATE TABLE refresh_book (id int);
INSERT INTO refresh_book (id) VALUES (1);
DROP FUNCTION IF EXISTS lib.refresh_book(uuid);
",
    );
    assert!(refs(&blob).is_empty(), "{:?}", refs(&blob));
}

#[test]
fn a_view_is_an_object_read_after_from() {
    let blob = extract(
        "CREATE OR REPLACE VIEW lib.book_view AS
  SELECT b.id, lib.refresh_book(b.album) AS n FROM books b;

SELECT n FROM lib.book_view WHERE n > 0;
",
    );
    assert_eq!(def(&blob, "book_view").kind, DefKind::Object);
    assert_eq!(def(&blob, "book_view").local.ns, Namespace::Type);
    let reads: Vec<&str> = refs(&blob)
        .into_iter()
        .filter(|r| r.use_kind == UseKind::Read)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(reads, ["books", "book_view"]);
}

#[test]
fn a_routine_does_not_call_itself() {
    let blob = extract("CREATE FUNCTION f(n int) RETURNS int AS $$ SELECT f(n - 1) $$;");
    assert!(refs(&blob).is_empty());
}
