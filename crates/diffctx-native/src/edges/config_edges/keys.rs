//! The key table as an edge builder: the typed links of `crate::keytable`
//! drawn as graph edges, and its counterparts of a change as discovery.

use std::path::{Path, PathBuf};

use rustc_hash::FxHashMap;
#[cfg(test)]
use rustc_hash::FxHashSet;

use super::super::EdgeDict;
use super::super::base::{EdgeBuilder, add_edge};
use crate::keytable::{Table, related_files};
use crate::types::Fragment;
#[cfg(test)]
use crate::types::FragmentId;

pub struct KeyTableEdgeBuilder;

impl EdgeBuilder for KeyTableEdgeBuilder {
    fn is_key_table(&self) -> bool {
        true
    }

    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        self.build_deciding(fragments, repo_root).0
    }

    fn build_deciding(
        &self,
        fragments: &[Fragment],
        repo_root: Option<&Path>,
    ) -> (EdgeDict, Vec<(String, String)>) {
        let table = Table::from_fragments(fragments, repo_root);
        let mut ruled_on = table.ruled_on_pairs();
        ruled_on.extend(table.untyped_spellings(fragments));
        let mut edges = EdgeDict::default();
        let mut reported = 0u64;
        for (n, pair) in table.live_links().into_iter().enumerate() {
            if !crate::resource::poll_emissions(n, 64, edges.len() as u64, &mut reported) {
                break;
            }
            let link = table.link(&pair);
            let (src, dst, forward, reverse_factor) =
                (link.src, link.dst, link.forward, link.reverse_factor);
            match link.src_depends {
                Some(true) => add_edge(&mut edges, src, dst, forward, reverse_factor),
                Some(false) => add_edge(&mut edges, dst, src, forward, reverse_factor),
                None => {
                    add_edge(&mut edges, src, dst, forward, reverse_factor);
                    add_edge(&mut edges, dst, src, forward, reverse_factor);
                }
            }
        }
        (edges, ruled_on)
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        related_files(changed, candidates, repo_root, file_cache)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(files: &[(&str, &str)]) -> FxHashMap<PathBuf, String> {
        files
            .iter()
            .map(|(p, c)| (PathBuf::from(p), (*c).to_string()))
            .collect()
    }

    fn discovered(changed: &[&str], files: &[(&str, &str)]) -> Vec<String> {
        let all: Vec<PathBuf> = files.iter().map(|(p, _)| PathBuf::from(p)).collect();
        let changed: Vec<PathBuf> = changed.iter().map(PathBuf::from).collect();
        KeyTableEdgeBuilder
            .discover_related_files(&changed, &all, Some(Path::new(".")), Some(&cache(files)))
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect()
    }

    const FILES: &[(&str, &str)] = &[
        (
            "cmd/main.go",
            "package main\nfunc f() { os.Getenv(\"CACHE_TTL\") }\n",
        ),
        (
            "deploy/server.yaml",
            "kind: Deployment\nspec:\n  containers:\n    - name: s\n      env:\n        - name: CACHE_TTL\n          value: \"1\"\n",
        ),
        (
            "deploy/other.yaml",
            "kind: Deployment\nspec:\n  containers:\n    - name: o\n      env:\n        - name: CACHE_TTL_JITTER\n          value: \"1\"\n",
        ),
    ];

    #[test]
    fn a_changed_reader_brings_its_provider_and_only_it() {
        assert_eq!(
            discovered(&["cmd/main.go"], FILES),
            vec!["deploy/server.yaml"]
        );
    }

    #[test]
    fn a_changed_provider_brings_its_reader() {
        assert_eq!(
            discovered(&["deploy/server.yaml"], FILES),
            vec!["cmd/main.go"]
        );
    }

    const TWO_UNITS: &[(&str, &str)] = &[
        ("svc/api/Dockerfile", "FROM python:3.12\n"),
        ("svc/api/.env.example", "SINK_URL=a\n"),
        ("svc/worker/Dockerfile", "FROM python:3.12\n"),
        ("svc/worker/.env.example", "SINK_URL=b\n"),
    ];

    #[test]
    fn two_definitions_in_two_units_link_only_through_a_reader() {
        assert!(discovered(&["svc/api/.env.example"], TWO_UNITS).is_empty());
        let mut with_reader = TWO_UNITS.to_vec();
        with_reader.push((
            "svc/api/app.py",
            "import os\nSINK = os.environ[\"SINK_URL\"]\n",
        ));
        let found = discovered(&["svc/api/.env.example"], &with_reader);
        assert_eq!(found, vec!["svc/api/app.py"]);
    }

    fn discovered_with(diff: &str, changed: &[&str], files: &[(&str, &str)]) -> Vec<String> {
        let lines = crate::changed_lines::ChangedLines::from_diff(diff);
        crate::resource::with_changed_lines(Some(std::sync::Arc::new(lines)), || {
            discovered(changed, files)
        })
    }

    const UNTOUCHED: &[(&str, &str)] = &[
        ("svc/api/Dockerfile", "FROM python:3.12\n"),
        ("svc/api/.env.example", "DB_URL=x\nTTL=60\n"),
        ("svc/api/app.py", "import os\nT = os.environ[\"TTL\"]\n"),
        ("svc/worker/Dockerfile", "FROM python:3.12\n"),
        (
            "svc/worker/app.py",
            "import os\nD = os.environ[\"DB_URL\"]\n",
        ),
    ];

    #[test]
    fn only_the_keys_on_changed_lines_drive_discovery() {
        let diff = "diff --git a/svc/api/.env.example b/svc/api/.env.example\n--- a/svc/api/.env.example\n+++ b/svc/api/.env.example\n@@ -1 +2 @@\n+TTL=60\n";
        let changed = ["svc/api/.env.example"];
        assert_eq!(
            discovered_with(diff, &changed, UNTOUCHED),
            vec!["svc/api/app.py"],
            "the untouched DB_URL must not bring the other unit's reader"
        );
        assert_eq!(
            discovered(&changed, UNTOUCHED),
            vec!["svc/api/app.py", "svc/worker/app.py"],
            "without the change's lines every key counts"
        );
    }

    #[test]
    fn a_removed_key_still_brings_its_readers() {
        let diff = "diff --git a/svc/api/.env.example b/svc/api/.env.example\n--- a/svc/api/.env.example\n+++ b/svc/api/.env.example\n@@ -2 +1,0 @@\n-TTL=60\n";
        let after: Vec<(&str, &str)> = UNTOUCHED
            .iter()
            .map(|(p, c)| {
                (
                    *p,
                    if *p == "svc/api/.env.example" {
                        "DB_URL=x\n"
                    } else {
                        *c
                    },
                )
            })
            .collect();
        assert_eq!(
            discovered_with(diff, &["svc/api/.env.example"], &after),
            vec!["svc/api/app.py"]
        );
    }

    #[test]
    fn a_renamed_compose_variable_still_names_the_readers_of_the_old_name() {
        let diff = "diff --git a/svc/api/docker-compose.yml b/svc/api/docker-compose.yml\n--- a/svc/api/docker-compose.yml\n+++ b/svc/api/docker-compose.yml\n@@ -4 +4 @@\n-      - OLD_HOST=x\n+      - NEW_HOST=x\n";
        let files = [
            ("svc/api/Dockerfile", "FROM python:3.12\n"),
            (
                "svc/api/docker-compose.yml",
                "services:\n  app:\n    environment:\n      - NEW_HOST=x\n",
            ),
            (
                "svc/api/app.py",
                "import os\nH = os.environ[\"OLD_HOST\"]\n",
            ),
            (
                "svc/api/other.py",
                "import os\nH = os.environ[\"UNRELATED\"]\n",
            ),
        ];
        assert_eq!(
            discovered_with(diff, &["svc/api/docker-compose.yml"], &files),
            vec!["svc/api/app.py"]
        );
    }

    const SCHEMAS: &[(&str, &str)] = &[
        ("stems/Dockerfile", "FROM python:3.12\n"),
        ("stems/db/0007.sql", "CREATE TABLE stems.assets (id int);\n"),
        ("stems/app/q.py", "db.execute(\"SELECT id FROM assets\")\n"),
        ("library/Dockerfile", "FROM python:3.12\n"),
        (
            "library/db/0001.sql",
            "CREATE TABLE library.assets (id int);\n",
        ),
        (
            "library/app/q.py",
            "db.execute(\"SELECT id FROM library.assets\")\n",
        ),
        ("reports/Dockerfile", "FROM python:3.12\n"),
        ("reports/q.py", "db.execute(\"SELECT id FROM assets\")\n"),
    ];

    #[test]
    fn two_schemas_holding_one_table_name_never_link() {
        // A bare name is the unit's own table: the stems query meets only the
        // stems migration, and the other schema's files and the bare name in
        // a unit with no such table stay out.
        assert_eq!(
            discovered(&["stems/db/0007.sql"], SCHEMAS),
            vec!["stems/app/q.py"]
        );
        assert_eq!(
            discovered(&["library/db/0001.sql"], SCHEMAS),
            vec!["library/app/q.py"]
        );
        assert!(discovered(&["reports/q.py"], SCHEMAS).is_empty());
    }

    const MOUNTED: &[(&str, &str)] = &[
        (
            "api/main.py",
            "from fastapi import FastAPI\nfrom app.routers import items\n\napp = FastAPI()\napp.include_router(items.router, prefix=\"/api\")\n",
        ),
        (
            "api/routers/items.py",
            "router = APIRouter(prefix=\"/items\")\n\n@router.get(\"/{item_id}\")\ndef one(item_id): ...\n",
        ),
        (
            "web/items.ts",
            "const API = \"/api\";\nexport const one = (id) => fetch(`${API}/items/${id}`);\n",
        ),
        (
            "web/other.ts",
            "export const other = (id) => fetch(`/items/${id}`);\n",
        ),
        (
            "edge/nginx.conf",
            "server {\n  location /api/items/ {\n    proxy_pass http://api;\n  }\n}\n",
        ),
        (
            "edge/other.conf",
            "server {\n  location /api/ {\n    proxy_pass http://api;\n  }\n}\n",
        ),
    ];

    #[test]
    fn a_router_is_served_behind_the_prefixes_it_is_mounted_under() {
        // The handler's own file says `/items/{id}`; the mount in main.py puts
        // `/api` in front, so the client that spells `/api/items/..` and the
        // proxy locations whose prefix it falls under are its readers, and the
        // client that spells `/items/..` is not.
        assert_eq!(
            discovered(&["api/routers/items.py"], MOUNTED),
            vec!["edge/nginx.conf", "edge/other.conf", "web/items.ts"]
        );
        assert_eq!(
            discovered(&["web/items.ts"], MOUNTED),
            vec!["api/routers/items.py"]
        );
    }

    const SINK_DIFF: &str = "diff --git a/svc/app.env b/svc/app.env\n--- a/svc/app.env\n+++ b/svc/app.env\n@@ -1 +1 @@\n+ZZSINKHOST=http://a\n";

    const SINK: &[(&str, &str)] = &[
        ("svc/app.env", "ZZSINKHOST=http://a\n"),
        (
            "svc/app.py",
            "import os\nSINK = os.environ[\"ZZSINKHOST\"]\n",
        ),
        ("tools/loadtest.py", "ZZSINKHOST = \"http://localhost:9\"\n"),
        ("tools/other.py", "SINK_COUNT = 3\n"),
    ];

    fn untyped_pairs(diff: Option<&str>, files: &[(&str, &str)]) -> Vec<(String, String)> {
        let fragments: Vec<Fragment> = files
            .iter()
            .flat_map(|(p, c)| crate::parsers::fragment_file(std::sync::Arc::from(*p), c))
            .collect();
        let lines =
            diff.map(|d| std::sync::Arc::new(crate::changed_lines::ChangedLines::from_diff(d)));
        crate::resource::with_changed_lines(lines, || {
            KeyTableEdgeBuilder.build_deciding(&fragments, None).1
        })
    }

    fn holds(pairs: &[(String, String)], a: &str, b: &str) -> bool {
        pairs.iter().any(|(x, y)| x == a && y == b)
    }

    #[test]
    fn a_variable_the_change_names_owns_its_name_against_a_local_constant() {
        let pairs = untyped_pairs(Some(SINK_DIFF), SINK);
        assert!(
            holds(&pairs, "svc/app.env", "tools/loadtest.py"),
            "the constant spelled like the key is not what the change reaches"
        );
        assert!(
            !holds(&pairs, "svc/app.env", "tools/other.py"),
            "a file that spells another name is no business of the key"
        );
        assert!(
            !holds(&pairs, "svc/app.py", "tools/loadtest.py"),
            "the reader's line is not on the change"
        );
    }

    #[test]
    fn a_name_the_table_did_not_resolve_stays_with_the_name_match() {
        let without_reader: Vec<(&str, &str)> = SINK
            .iter()
            .filter(|(p, _)| *p != "svc/app.py")
            .copied()
            .collect();
        assert!(untyped_pairs(Some(SINK_DIFF), &without_reader).is_empty());
        let unmarked = untyped_pairs(None, SINK);
        assert!(
            holds(&unmarked, "svc/app.env", "svc/app.py"),
            "the typed pair is ruled on with or without the change"
        );
        assert!(
            !holds(&unmarked, "svc/app.env", "tools/loadtest.py"),
            "without the change's lines no record is the changed one"
        );
    }

    const NESTED_PRICE: &str = "demo:\n  currency: EUR\n  total: ${demo.item-price:10}\n";
    const FLAT_PRICE: &str = "currency: EUR\ntotal: ${demo.item-price:10}\n";
    const APPLICATION: &str = "billing/src/main/resources/application.yml";

    /// The edges the key table draws for a change that adds the last line of
    /// `application`, with the fragments the change seeds.
    fn price_edges(application: &str, line: u32) -> (EdgeDict, FxHashSet<FragmentId>) {
        let files = [
            (
                "billing/Dockerfile",
                "FROM eclipse-temurin:21\nCOPY . /srv\n",
            ),
            ("billing/.env.example", "DEMO_ITEMPRICE=10\n"),
            (APPLICATION, application),
        ];
        let fragments: Vec<Fragment> = files
            .iter()
            .flat_map(|(p, c)| crate::parsers::fragment_file(std::sync::Arc::from(*p), c))
            .collect();
        let added = application.lines().last().unwrap_or_default();
        let diff = format!(
            "diff --git a/{APPLICATION} b/{APPLICATION}\n--- a/{APPLICATION}\n+++ b/{APPLICATION}\n@@ -{},0 +{line} @@\n+{added}\n",
            line - 1
        );
        let hunks = [crate::types::DiffHunk {
            path: std::sync::Arc::from(APPLICATION),
            new_start: line,
            new_len: 1,
            old_start: line - 1,
            old_len: 0,
        }];
        let core = crate::core::identify_core_fragments(&hunks, &fragments);
        let lines = std::sync::Arc::new(crate::changed_lines::ChangedLines::from_diff(&diff));
        let edges = crate::resource::with_changed_lines(Some(lines), || {
            KeyTableEdgeBuilder.build(&fragments, None)
        });
        (edges, core)
    }

    #[test]
    fn a_typed_link_leaves_the_fragment_the_change_seeds_flat_or_nested() {
        for (application, line) in [(FLAT_PRICE, 2), (NESTED_PRICE, 3)] {
            let (edges, core) = price_edges(application, line);
            assert!(
                edges
                    .keys()
                    .any(|(src, dst)| core.contains(src)
                        && dst.path.as_ref() == "billing/.env.example"),
                "no edge from the seeded fragment {core:?} to the variable in {application:?}: {edges:?}"
            );
        }
    }
}
