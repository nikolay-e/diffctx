use std::path::{Path, PathBuf};

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::weights::EDGE_WEIGHTS;
use crate::types::Fragment;

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edges_from_ids};

fn is_sql_file(path: &Path) -> bool {
    base::has_ext(path, &[".sql"])
}

// A name keeps the schema it is spelled behind: `stems.assets` and
// `library.assets` are two tables.
static CREATE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?mi)^\s*CREATE\s+(?:OR\s+REPLACE\s+)?(?:TABLE|VIEW|FUNCTION|PROCEDURE|TYPE|INDEX|TRIGGER|SEQUENCE|SCHEMA)\s+(?:IF\s+NOT\s+EXISTS\s+)?((?:[\w"`]+\.)*[\w"`]+)"#).unwrap()
});
static REFERENCES_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?mi)REFERENCES\s+((?:[\w"`]+\.)*[\w"`]+)"#).unwrap());
static TABLE_REF_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"(?mi)(?:FROM|JOIN|INTO|UPDATE|DELETE\s+FROM|ALTER\s+TABLE|DROP\s+TABLE|TRUNCATE\s+TABLE|INSERT\s+INTO)\s+((?:[\w"`]+\.)*[\w"`]+)"#).unwrap()
});

/// Schemas a database searches without being told.
const DEFAULT_SCHEMAS: &[&str] = &["public", "dbo", "main"];

/// A table name as `(schema, name)`; the schema is absent when the name is
/// bare or sits behind a default schema.
fn split_name(raw: &str) -> (Option<String>, String) {
    let cleaned = raw.replace(['"', '`'], "").to_lowercase();
    let mut parts = cleaned.rsplit('.');
    let name = parts.next().unwrap_or_default().to_string();
    let schema = parts
        .next()
        .filter(|s| !DEFAULT_SCHEMAS.contains(s))
        .map(str::to_string);
    (schema, name)
}

static SQL_KEYWORDS: Lazy<FxHashSet<&str>> = Lazy::new(|| {
    base::kw(concat!(
        "select from where and or not in exists null true false set values as on using left right ",
        "inner outer cross group order by having limit offset union all distinct case when then ",
        "else end if begin declare returns return ",
    ))
});
type TableName = (Option<String>, String);

fn names_in(re: &Regex, content: &str) -> impl Iterator<Item = TableName> {
    re.captures_iter(content)
        .map(|c| split_name(&c[1]))
        .filter(|(_, n)| !SQL_KEYWORDS.contains(n.as_str()))
        .collect::<Vec<_>>()
        .into_iter()
}

fn extract_creates(content: &str) -> FxHashSet<TableName> {
    names_in(&CREATE_RE, content).collect()
}

fn extract_table_refs(content: &str) -> FxHashSet<TableName> {
    names_in(&TABLE_REF_RE, content)
        .chain(names_in(&REFERENCES_RE, content))
        .collect()
}

/// The files a reference reaches. A schema-qualified name reaches that
/// schema's table; a bare one reaches the bare tables and, when the name is
/// defined behind one schema only, that schema's table. Two schemas holding
/// one name never reach each other.
fn creators<'a, T>(
    created: &'a FxHashMap<TableName, Vec<T>>,
    reference: &TableName,
) -> Vec<&'a Vec<T>> {
    let (schema, name) = reference;
    let behind_schemas: Vec<&TableName> = created
        .keys()
        .filter(|(s, n)| n == name && s.is_some())
        .collect();
    let bare = created.get(&(None, name.clone()));
    match schema {
        Some(_) => created
            .get(reference)
            .into_iter()
            .chain(bare.filter(|_| behind_schemas.is_empty()))
            .collect(),
        None => bare
            .into_iter()
            .chain(
                (behind_schemas.len() == 1)
                    .then(|| created.get(behind_schemas[0]))
                    .flatten(),
            )
            .collect(),
    }
}

pub struct SqlEdgeBuilder;

impl EdgeBuilder for SqlEdgeBuilder {
    fn build(&self, fragments: &[Fragment], _repo_root: Option<&Path>) -> EdgeDict {
        let Some(frags) = base::frags_where(fragments, |f| is_sql_file(Path::new(f.path()))) else {
            return FxHashMap::default();
        };

        let fk_w = EDGE_WEIGHTS["sql_fk"].forward;
        let table_ref_w = EDGE_WEIGHTS["sql_table_ref"].forward;
        let reverse_factor = EDGE_WEIGHTS["sql_fk"].reverse_factor;

        let mut table_to_frags: FxHashMap<TableName, Vec<_>> = FxHashMap::default();
        for f in &frags {
            for name in extract_creates(&f.content) {
                table_to_frags.entry(name).or_default().push(f.id.clone());
            }
        }

        let mut edges: EdgeDict = FxHashMap::default();

        for f in &frags {
            let self_creates = extract_creates(&f.content);
            let has_fk = REFERENCES_RE.is_match(&f.content);
            for tref in extract_table_refs(&f.content) {
                if self_creates.contains(&tref) {
                    continue;
                }
                let w = if has_fk && names_in(&REFERENCES_RE, &f.content).any(|n| n == tref) {
                    fk_w
                } else {
                    table_ref_w
                };
                for targets in creators(&table_to_frags, &tref) {
                    add_edges_from_ids(&mut edges, &f.id, targets, w, reverse_factor);
                }
            }
        }
        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        base::discover_by_extracted_refs(
            changed,
            candidates,
            repo_root,
            file_cache,
            is_sql_file,
            |content| {
                extract_table_refs(content)
                    .into_iter()
                    .map(|(schema, name)| match schema {
                        Some(schema) => format!("{schema}.{name}"),
                        None => name,
                    })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linked(files: &[(&str, &str)]) -> Vec<(String, String)> {
        let fragments: Vec<Fragment> = files
            .iter()
            .flat_map(|(p, c)| crate::parsers::fragment_file(std::sync::Arc::from(*p), c))
            .collect();
        let mut pairs: Vec<(String, String)> = SqlEdgeBuilder
            .build(&fragments, None)
            .keys()
            .map(|(src, dst)| (src.path.to_string(), dst.path.to_string()))
            .collect();
        pairs.sort();
        pairs.dedup();
        pairs
    }

    #[test]
    fn a_table_is_its_schema_and_its_name() {
        let files = [
            ("a/0001.sql", "CREATE TABLE stems.assets (id int);\n"),
            ("b/0001.sql", "CREATE TABLE library.assets (id int);\n"),
            (
                "a/0002.sql",
                "ALTER TABLE stems.assets ADD COLUMN c text;\n",
            ),
            ("c/0001.sql", "CREATE TABLE orders (id int);\n"),
            (
                "c/0002.sql",
                "ALTER TABLE public.orders ADD COLUMN c text;\n",
            ),
            ("d/q.sql", "SELECT * FROM assets;\n"),
        ];
        let pairs = linked(&files);
        let has = |a: &str, b: &str| pairs.contains(&(a.to_string(), b.to_string()));
        assert!(has("a/0002.sql", "a/0001.sql"));
        assert!(!has("a/0002.sql", "b/0001.sql"));
        assert!(
            has("c/0002.sql", "c/0001.sql"),
            "public is the default schema"
        );
        // `assets` is defined behind two schemas: a bare name picks neither.
        assert!(!has("d/q.sql", "a/0001.sql") && !has("d/q.sql", "b/0001.sql"));
    }
}
