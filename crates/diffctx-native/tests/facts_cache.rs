//! The facts cache and `facts_of`, on real git repositories: what is kept, what
//! is a miss, what a bad entry costs, and what a blob read at two paths shares.

mod facts_fixture;
mod snapshot_fixture;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use _diffctx::facts::{
    CacheKey, FINGERPRINT, Fact, FactsCache, Import, ImportKind, Language, ModulePath, Name,
    SCHEMA, Scope, Span, Star, blob_oid, facts_of, hash_tokens, lang,
};
use _diffctx::snapshot::{Loss, RangeSpec, RepoPath, SideTag};
use facts_fixture::{Toy, cache_in, context, entries, leftovers, review};
use snapshot_fixture::Repo;

const SOURCE: &str = "def f(a):\n    return a + 1\n\ndef g():\n    return 2\n";

fn path(text: &str) -> RepoPath {
    RepoPath::new(text).expect("a path")
}

fn one_commit(files: &[(&str, &str)]) -> (Repo, _diffctx::snapshot::Oid) {
    let repo = Repo::init();
    for (name, text) in files {
        repo.write(name, text);
    }
    let head = repo.commit("only");
    (repo, head)
}

#[test]
fn an_entry_written_by_one_cache_is_read_by_the_next_without_extracting() {
    let dir = tempfile::tempdir().expect("dir");
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let ctx = review(&repo, &head, &head);

    let toy = Toy::new();
    let first = cache_in(dir.path(), &toy);
    let written = facts_of(&ctx, SideTag::Head, &first, &path("m.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 1);
    assert_eq!(first.stats().misses, 1);
    assert_eq!(first.stats().writes, 1);
    assert_eq!(entries(dir.path()).len(), 1);

    let again = facts_of(&ctx, SideTag::Head, &first, &path("m.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 1, "a second look in one process is a hit");
    assert_eq!(first.stats().memory_hits, 1);
    assert!(Arc::ptr_eq(&written.blob, &again.blob));

    let fresh_process = cache_in(dir.path(), &toy);
    let read = facts_of(&ctx, SideTag::Head, &fresh_process, &path("m.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 1, "the entry on disk serves the next process");
    assert_eq!(fresh_process.stats().disk_hits, 1);
    assert_eq!(read.blob.facts, written.blob.facts);
    assert_eq!(read.blob.language, Language::Python);
}

#[test]
fn an_entry_of_another_build_fingerprint_or_grammar_is_a_miss() {
    let dir = tempfile::tempdir().expect("dir");
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let look = |toy: &Toy| {
        let cache = cache_in(dir.path(), toy);
        facts_of(&ctx, SideTag::Head, &cache, &path("m.py")).expect("facts");
    };
    look(&toy);
    look(&toy);
    assert_eq!(toy.calls(), 1);

    let mut newer = toy.clone();
    newer.build += 1;
    look(&newer);
    assert_eq!(
        toy.calls(),
        2,
        "an entry of another build fingerprint is a miss"
    );

    let mut regrown = toy.clone();
    regrown.grammar += 1;
    look(&regrown);
    assert_eq!(toy.calls(), 3, "a new grammar digest re-extracts");

    look(&toy);
    assert_eq!(toy.calls(), 3, "the first entry is still there");
    assert_eq!(entries(dir.path()).len(), 3);
}

#[test]
fn the_key_names_its_five_parts() {
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let dir = tempfile::tempdir().expect("dir");
    let toy = Toy::new();
    let ctx = review(&repo, &head, &head);
    facts_of(
        &ctx,
        SideTag::Head,
        &cache_in(dir.path(), &toy),
        &path("m.py"),
    )
    .expect("facts");
    let key = CacheKey {
        blob: repo.rev("HEAD:m.py"),
        language: Language::Python,
        build: 1,
        grammar: 7,
        schema: SCHEMA,
    };
    let stored = entries(dir.path());
    let name = stored[0]
        .file_name()
        .expect("a name")
        .to_string_lossy()
        .into_owned();
    assert!(
        name.starts_with(&format!("{}-python-0000000000000001-", key.blob)),
        "{name}"
    );
    assert!(
        name.ends_with(&format!("-0000000000000007-{SCHEMA}.json")),
        "{name}"
    );
}

#[test]
fn one_blob_at_two_paths_is_extracted_once_and_bound_to_each_module() {
    let (repo, head) = one_commit(&[("pkg/a.py", SOURCE), ("pkg/b.py", SOURCE)]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let cache = FactsCache::in_memory().with_registry(toy.registry());
    let a = facts_of(&ctx, SideTag::Head, &cache, &path("pkg/a.py"))
        .expect("facts")
        .expect("some");
    let b = facts_of(&ctx, SideTag::Head, &cache, &path("pkg/b.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 1);
    assert!(Arc::ptr_eq(&a.blob, &b.blob));
    assert_eq!(a.module.as_str(), "pkg.a");
    assert_eq!(b.module.as_str(), "pkg.b");
    let local = &a.defs().next().expect("a definition").local;
    assert_ne!(a.key(local), b.key(local), "a moved file changes its keys");
}

#[test]
fn a_disabled_cache_extracts_every_time_and_writes_nothing() {
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let cache = FactsCache::disabled().with_registry(toy.registry());
    for _ in 0..2 {
        facts_of(&ctx, SideTag::Head, &cache, &path("m.py")).expect("facts");
    }
    assert_eq!(toy.calls(), 2);
    assert_eq!(cache.stats().writes, 0);
}

fn damage(dir: &std::path::Path, how: impl Fn(Vec<u8>) -> Vec<u8>) {
    let stored = entries(dir);
    assert_eq!(stored.len(), 1);
    let bytes = std::fs::read(&stored[0]).expect("read");
    std::fs::write(&stored[0], how(bytes)).expect("write");
}

#[test]
fn a_corrupt_entry_is_a_miss_that_is_counted_and_rewritten() {
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let ctx = review(&repo, &head, &head);
    let ways: [(&str, fn(Vec<u8>) -> Vec<u8>); 4] = [
        ("a flipped byte in the body", |mut bytes| {
            let at = bytes.len() - 3;
            bytes[at] ^= 0x01;
            bytes
        }),
        ("a truncated file", |mut bytes| {
            bytes.truncate(bytes.len() / 2);
            bytes
        }),
        ("a wrong checksum line", |mut bytes| {
            bytes[0] = if bytes[0] == b'0' { b'1' } else { b'0' };
            bytes
        }),
        ("garbage", |_| b"not an entry".to_vec()),
    ];
    for (what, make) in ways {
        let dir = tempfile::tempdir().expect("dir");
        let toy = Toy::new();
        let original = facts_of(
            &ctx,
            SideTag::Head,
            &cache_in(dir.path(), &toy),
            &path("m.py"),
        )
        .expect("facts")
        .expect("some");
        damage(dir.path(), make);

        let cache = cache_in(dir.path(), &toy);
        let rebuilt = facts_of(&ctx, SideTag::Head, &cache, &path("m.py"))
            .expect("a bad entry never fails the answer")
            .expect("some");
        assert_eq!(toy.calls(), 2, "{what}: extracted again");
        assert_eq!(cache.stats().discarded, 1, "{what}");
        assert_eq!(cache.stats().disk_hits, 0, "{what}");
        assert_eq!(rebuilt.blob.facts, original.blob.facts, "{what}");

        let healed = cache_in(dir.path(), &toy);
        facts_of(&ctx, SideTag::Head, &healed, &path("m.py")).expect("facts");
        assert_eq!(toy.calls(), 2, "{what}: the rewritten entry is read");
        assert_eq!(healed.stats().disk_hits, 1, "{what}");
    }
}

#[test]
fn an_unwritable_directory_costs_a_re_extraction_and_never_the_answer() {
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let ctx = review(&repo, &head, &head);
    let blocker = tempfile::tempdir().expect("dir");
    let file = blocker.path().join("not-a-dir");
    std::fs::write(&file, "x").expect("write");
    let toy = Toy::new();
    let cache = cache_in(&file.join("cache"), &toy);
    let found = facts_of(&ctx, SideTag::Head, &cache, &path("m.py")).expect("facts");
    assert!(found.is_some());
    assert_eq!(cache.stats().write_failures, 1);
}

#[test]
fn concurrent_writers_of_one_key_leave_a_whole_entry_and_no_temporaries() {
    let dir = tempfile::tempdir().expect("dir");
    let (repo, head) = one_commit(&[("m.py", SOURCE)]);
    let toy = Toy::new();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let ctx = review(&repo, &head, &head);
                let cache = cache_in(dir.path(), &toy);
                facts_of(&ctx, SideTag::Head, &cache, &path("m.py"))
                    .expect("facts")
                    .expect("some");
            });
        }
    });
    assert!(
        leftovers(dir.path()).is_empty(),
        "{:?}",
        leftovers(dir.path())
    );
    assert_eq!(entries(dir.path()).len(), 1);
    let calls = toy.calls();
    let ctx = review(&repo, &head, &head);
    let cache = cache_in(dir.path(), &toy);
    facts_of(&ctx, SideTag::Head, &cache, &path("m.py")).expect("facts");
    assert_eq!(toy.calls(), calls, "the surviving entry is whole");
    assert_eq!(cache.stats().discarded, 0);
}

#[test]
fn past_its_cap_the_least_recently_used_entries_go() {
    let dir = tempfile::tempdir().expect("dir");
    let files: Vec<(String, String)> = (0..24)
        .map(|n| {
            (
                format!("m{n}.py"),
                format!("def f{n}(a):\n    return a + {n}\n"),
            )
        })
        .collect();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let (repo, head) = one_commit(&borrowed);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let cap = 6_000;
    let cache = FactsCache::new(dir.path().to_path_buf())
        .with_cap(cap)
        .with_registry(toy.registry());
    for (name, _) in &files {
        facts_of(&ctx, SideTag::Head, &cache, &path(name)).expect("facts");
    }
    let held: u64 = entries(dir.path())
        .iter()
        .map(|entry| std::fs::metadata(entry).expect("metadata").len())
        .sum();
    assert!(cache.stats().evicted > 0);
    assert!(held <= cap, "{held} bytes over a cap of {cap}");
    assert_eq!(cache.stats().writes, 24);
}

#[test]
fn an_unchanged_working_tree_file_shares_the_commits_entry() {
    let dir = tempfile::tempdir().expect("dir");
    let (repo, head) = one_commit(&[("m.py", SOURCE), ("n.py", SOURCE)]);
    repo.write("n.py", "def other():\n    return 3\n");
    let toy = Toy::new();
    let cache = cache_in(dir.path(), &toy);
    let ctx = context(&repo, RangeSpec::WorkingTree { base: head });

    let committed = facts_of(&ctx, SideTag::Base, &cache, &path("m.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 1);
    let working = facts_of(&ctx, SideTag::Head, &cache, &path("m.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 1, "the same bytes are the same blob");
    assert!(Arc::ptr_eq(&committed.blob, &working.blob));

    let edited = facts_of(&ctx, SideTag::Head, &cache, &path("n.py"))
        .expect("facts")
        .expect("some");
    assert_eq!(toy.calls(), 2);
    assert_eq!(
        edited.defs().next().expect("a def").local.name.as_str(),
        "other"
    );
}

#[test]
fn a_working_tree_blob_id_is_gits() {
    let repo = Repo::init();
    let sizes = [0usize, 1, 3, 55, 56, 57, 63, 64, 65, 119, 120, 1000];
    for (n, size) in sizes.iter().enumerate() {
        let text: String = (0..*size)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let name = format!("blob{n}.txt");
        repo.write(&name, &text);
        let git = repo.git(&["hash-object", &name]);
        let ours = blob_oid(text.as_bytes()).expect("an id");
        assert_eq!(ours.as_str(), git.trim(), "{size} bytes");
    }
}

#[test]
fn a_file_without_an_extension_gets_its_language_from_its_shebang() {
    let (repo, head) = one_commit(&[
        (
            "bin/tool",
            "#!/usr/bin/env python3\ndef run():\n    return 1\n",
        ),
        (
            "bin/direct",
            "#!/usr/bin/python\ndef run():\n    return 1\n",
        ),
        ("bin/node-tool", "#!/usr/bin/env node\nrun()\n"),
        ("LICENSE", "Permission is hereby granted\n"),
        (
            "run.sh",
            "#!/usr/bin/env python3\ndef run():\n    return 1\n",
        ),
    ]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let cache = FactsCache::disabled().with_registry(toy.registry());
    let look = |name: &str| facts_of(&ctx, SideTag::Head, &cache, &path(name)).expect("facts");
    assert_eq!(
        look("bin/tool").expect("python").language(),
        Language::Python
    );
    assert!(look("bin/direct").is_some());
    assert!(
        look("bin/node-tool").is_none(),
        "no extractor for JavaScript here"
    );
    assert!(look("LICENSE").is_none());
    assert!(
        look("run.sh").is_none(),
        "a named extension decides, not the shebang"
    );
}

#[test]
fn a_path_the_snapshot_does_not_hold_or_withholds_has_no_facts() {
    let (repo, head) = one_commit(&[
        ("m.py", SOURCE),
        ("deploy/id_rsa", "def f():\n    return 1\n"),
    ]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let cache = FactsCache::disabled().with_registry(toy.registry());
    for name in ["missing.py", "deploy/id_rsa", "sub"] {
        let found = facts_of(&ctx, SideTag::Head, &cache, &path(name)).expect("no error");
        assert!(found.is_none(), "{name}");
    }
    assert_eq!(toy.calls(), 0);
}

#[test]
fn a_panic_is_a_loss_of_that_file_and_is_not_kept() {
    let dir = tempfile::tempdir().expect("dir");
    let (repo, head) = one_commit(&[
        ("bad.py", "#panic\ndef f():\n    return 1\n"),
        ("m.py", SOURCE),
    ]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let cache = cache_in(dir.path(), &toy);
    let bad = facts_of(&ctx, SideTag::Head, &cache, &path("bad.py"))
        .expect("the run goes on")
        .expect("some");
    assert!(bad.blob.facts.is_empty());
    assert_eq!(
        bad.losses(),
        vec![Loss::ExtractorFailed {
            path: path("bad.py")
        }]
    );
    assert!(entries(dir.path()).is_empty());
    let good = facts_of(&ctx, SideTag::Head, &cache, &path("m.py")).expect("facts");
    assert!(good.is_some());
}

#[test]
fn parse_errors_survive_the_cache_and_name_the_path_they_are_read_for() {
    let dir = tempfile::tempdir().expect("dir");
    let broken = "#parse-error\ndef f():\n    return 1\n";
    let (repo, head) = one_commit(&[("one.py", broken), ("two.py", broken)]);
    let ctx = review(&repo, &head, &head);
    let toy = Toy::new();
    let first = facts_of(
        &ctx,
        SideTag::Head,
        &cache_in(dir.path(), &toy),
        &path("one.py"),
    )
    .expect("facts")
    .expect("some");
    let second = facts_of(
        &ctx,
        SideTag::Head,
        &cache_in(dir.path(), &toy),
        &path("two.py"),
    )
    .expect("facts")
    .expect("some");
    assert_eq!(toy.calls(), 1);
    assert_eq!(
        first.losses(),
        vec![Loss::ParseError {
            path: path("one.py"),
            lines: vec![(1, 1)]
        }]
    );
    assert_eq!(
        second.losses(),
        vec![Loss::ParseError {
            path: path("two.py"),
            lines: vec![(1, 1)]
        }]
    );
}

#[test]
fn facts_come_in_source_order_and_hashes_are_delimited() {
    use _diffctx::facts::{BlobFacts, Fact, PathMention, Scope, Span};
    let mention = |at: u32, text: &str| {
        Fact::PathMention(PathMention {
            site: Span::new(at..at + 1, (1, 1)),
            enclosing: Scope::Module,
            text: text.to_string(),
        })
    };
    let mut blob = BlobFacts::new(Language::Python);
    blob.facts = vec![mention(30, "c"), mention(10, "a"), mention(20, "b")];
    blob.in_source_order();
    let order: Vec<u32> = blob.facts.iter().map(|f| f.site().bytes.start).collect();
    assert_eq!(order, [10, 20, 30]);

    assert_ne!(
        hash_tokens([b"ab".as_slice(), b"c"]),
        hash_tokens([b"a".as_slice(), b"bc"])
    );
    assert_eq!(hash_tokens([]), None);
}

#[test]
fn the_shipped_grammars_have_a_digest() {
    assert_ne!(lang::grammar_digest(Language::Python), 0);
    assert_ne!(
        lang::grammar_digest(Language::Python),
        lang::grammar_digest(Language::Rust)
    );
}

#[test]
fn a_cache_is_keyed_on_the_fingerprint_of_the_build_unless_told_otherwise() {
    assert_eq!(FactsCache::disabled().fingerprint(), FINGERPRINT);
    assert_ne!(FINGERPRINT, 0);
    assert_eq!(FactsCache::disabled().with_fingerprint(9).fingerprint(), 9);
}

/// Every value a field of the facts can hold has to come back from the cache as
/// it went in: an encoding that writes two values alike (a nested `Option` writes
/// `Some(None)` as `null`) makes a cache hit answer differently from a miss.
#[test]
fn every_value_of_an_import_star_survives_the_encoding_the_cache_uses() {
    for star in [Star::None, Star::Glob, Star::Alias(Name::new("ns"))] {
        let fact = Fact::Import(Import {
            site: Span::default(),
            enclosing: Scope::Module,
            module: ModulePath::new("m"),
            relative: 0,
            kind: ImportKind::Static,
            names: Vec::new(),
            star: star.clone(),
            default: None,
        });
        let written = serde_json::to_vec(&fact).expect("encodes");
        let read: Fact = serde_json::from_slice(&written).expect("decodes");
        assert_eq!(read, fact, "{star:?}");
    }
}

fn read_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The sources of the impact and symbol corpus fixtures, each under a name of its
/// own, and the sources of this repository in the languages that have facts.
fn corpus_sources() -> Vec<(String, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut found = Vec::new();
    for mode in ["impact", "symbol"] {
        for entry in walkdir::WalkDir::new(root.join("tests/cases").join(mode))
            .sort_by_file_name()
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "yaml"))
        {
            let Some(text) = read_text(entry.path()) else {
                continue;
            };
            let Ok(case) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
                continue;
            };
            for group in ["initial_files", "changed_files", "untracked_files"] {
                let Some(files) = case["repo"][group].as_mapping() else {
                    continue;
                };
                for (name, content) in files {
                    if let (Some(name), Some(content)) = (name.as_str(), content.as_str()) {
                        let at = found.len();
                        found.push((format!("case{at}/{name}"), content.to_string()));
                    }
                }
            }
        }
    }
    for (dir, extension) in [("crates/diffctx-native/src", "rs"), ("src/diffctx", "py")] {
        for entry in walkdir::WalkDir::new(root.join(dir))
            .sort_by_file_name()
            .into_iter()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == extension))
        {
            if let Some(text) = read_text(entry.path()) {
                let relative = entry.path().strip_prefix(&root).unwrap_or(entry.path());
                found.push((
                    format!("own/{}", relative.to_string_lossy().replace('\\', "/")),
                    text,
                ));
            }
        }
    }
    found
}

/// The class test of the cache: for every file of every corpus fixture, what a
/// process reads back from the disk entry equals what the extractor made.
#[test]
fn every_registered_extractors_facts_equal_themselves_after_the_caches_round_trip() {
    let sources = corpus_sources();
    let refs: Vec<(&str, &str)> = sources
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let (repo, head) = one_commit(&refs);
    let ctx = review(&repo, &head, &head);
    let dir = tempfile::tempdir().expect("dir");

    let cold = FactsCache::new(dir.path().to_path_buf());
    let mut made = Vec::new();
    for (name, _) in &sources {
        if let Some(file) = facts_of(&ctx, SideTag::Head, &cold, &path(name)).expect("facts") {
            made.push((name, file.blob));
        }
    }
    assert!(made.len() > 100, "{} files were read", made.len());

    let warm = FactsCache::new(dir.path().to_path_buf());
    for (name, blob) in &made {
        let read = facts_of(&ctx, SideTag::Head, &warm, &path(name))
            .expect("facts")
            .expect("some");
        assert_eq!(*read.blob, **blob, "{name} is not what the cache was given");
    }
    assert_eq!(warm.stats().discarded, 0);
    assert!(
        warm.stats().disk_hits > 0,
        "the second process read nothing"
    );

    let globs = made
        .iter()
        .flat_map(|(_, blob)| &blob.facts)
        .filter(|fact| matches!(fact, Fact::Import(i) if i.star == Star::Glob))
        .count();
    assert!(
        globs > 0,
        "no corpus file has a glob import to hold the test to"
    );
}

fn answer(repo: &Repo, cache: &Path) -> Vec<u8> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_diffctx"));
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_PREFIX",
    ] {
        command.env_remove(name);
    }
    let out = command
        .current_dir(repo.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("DIFFCTX_CACHE_DIR", cache)
        .args(["--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", "json"])
        .args(["-q", "--timeout", "600"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// A run with a cold cache and one with a warm cache answer with the same bytes,
/// on fixtures whose answer depends on a star import.
#[test]
fn a_cold_cache_and_a_warm_cache_give_byte_identical_answers_through_star_imports() {
    const MANIFEST: &str =
        "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
    type Files<'a> = &'a [(&'a str, &'a str)];
    let fixtures: [(&str, Files, Files); 4] = [
        (
            "Python `from lib import *`",
            &[
                ("lib.py", "def helper():\n    return 1\n"),
                (
                    "app.py",
                    "from lib import *\n\n\ndef run():\n    return helper()\n",
                ),
            ],
            &[("lib.py", "def helper():\n    return 2\n")],
        ),
        (
            "Rust `use a::*`",
            &[
                ("Cargo.toml", MANIFEST),
                ("src/lib.rs", "pub mod a;\npub mod user;\n"),
                ("src/a.rs", "pub fn f() -> u32 {\n    1\n}\n"),
                (
                    "src/user.rs",
                    "use crate::a::*;\n\npub fn g() -> u32 {\n    f()\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn gs() {\n        assert_eq!(g(), 1);\n    }\n}\n",
                ),
            ],
            &[("src/a.rs", "pub fn f() -> u32 {\n    2\n}\n")],
        ),
        (
            "Kotlin `import p.*`",
            &[
                (
                    "src/main/kotlin/p/Util.kt",
                    "package p\n\nfun slug(s: String): String = s\n",
                ),
                (
                    "src/main/kotlin/q/Use.kt",
                    "package q\n\nimport p.*\n\nfun title(s: String): String = slug(s)\n",
                ),
            ],
            &[(
                "src/main/kotlin/p/Util.kt",
                "package p\n\nfun slug(s: String): String = s.trim()\n",
            )],
        ),
        (
            "TypeScript `export *`",
            &[
                (
                    "src/a.ts",
                    "export function f(): number {\n  return 1;\n}\n",
                ),
                ("src/index.ts", "export * from './a';\n"),
                (
                    "src/use.ts",
                    "import { f } from './index';\n\nexport function g(): number {\n  return f();\n}\n",
                ),
            ],
            &[(
                "src/a.ts",
                "export function f(): number {\n  return 2;\n}\n",
            )],
        ),
    ];
    for (what, base, head) in fixtures {
        let repo = Repo::init();
        for (name, text) in base {
            repo.write(name, text);
        }
        repo.commit("base");
        for (name, text) in head {
            repo.write(name, text);
        }
        repo.commit("head");
        let cache = tempfile::tempdir().expect("dir");
        let cold = answer(&repo, cache.path());
        assert!(
            entries(&cache.path().join("diffctx").join("facts")).len() > 1,
            "{what}: the cold run kept nothing for the warm one"
        );
        let warm = answer(&repo, cache.path());
        assert_eq!(
            String::from_utf8_lossy(&warm),
            String::from_utf8_lossy(&cold),
            "{what}: a cache hit answered differently from a miss"
        );
        let doc: serde_json::Value = serde_json::from_slice(&cold).expect("json");
        assert!(
            doc["definitions"].as_array().is_some_and(|c| !c.is_empty()),
            "{what}: {doc:#}"
        );
    }
}
