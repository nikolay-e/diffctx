//! The contract types of absence by occurrences and the byte scan that finds
//! them (`occurrence`), on file contents: the garbage fixtures of the corpus and
//! small sources whose answer is read off the text. Nothing runs diffctx.

use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::{
    BlobFacts, DefKey, Extractor, FileFacts, Language, Lexical, LocalKey, ModulePath, ProjectId,
};
use _diffctx::occurrence::{
    Account, Alias, AliasBound, AliasScope, Basis, Case, Class, Entry, Hedge, Implicit,
    MAX_ALIAS_HOPS, MAX_ALIAS_NAMES, Occurrence, Outcome, Reason, Unread, ValuePlace, Verdict,
    scan,
};
use _diffctx::resolve::{CandidateReason, Relation};
use _diffctx::snapshot::{Loss, RepoPath, RunBudget, SideTag};
use proptest::test_runner::{Config, RngSeed, TestCaseError, TestRunner};
use regex::Regex;

fn path(name: &str) -> RepoPath {
    RepoPath::new(name).expect("a repository path")
}

fn file_of(blob: BlobFacts) -> FileFacts {
    FileFacts {
        path: path("src/sample"),
        project: ProjectId::default(),
        module: ModulePath::new("sample"),
        blob: Arc::new(blob),
        outdated_grammar: false,
    }
}

fn file_with(language: Language, lexical: Vec<(Range<u32>, Lexical)>) -> FileFacts {
    let mut blob = BlobFacts::new(language);
    blob.lexical = lexical;
    blob.in_source_order();
    file_of(blob)
}

fn scan_names(language: Language, text: &str, names: &[&str]) -> Vec<Range<u32>> {
    let file = file_with(language, Vec::new());
    let found = scan(&file, text.as_bytes(), names, Case::of(language));
    assert!(!found.lost);
    assert!(found.mentions.is_empty());
    found.code.into_iter().map(|hit| hit.bytes).collect()
}

fn fixture(name: &str) -> String {
    let at = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/garbage")
        .join(name);
    std::fs::read_to_string(at).expect("a fixture file")
}

#[test]
fn a_name_stands_alone_between_bytes_no_identifier_has() {
    let text = "f(g(f))\nxf f1 _f f.f \u{e9}f\n";
    assert_eq!(
        scan_names(Language::Python, text, &["f"]),
        vec![0..1, 4..5, 17..18, 19..20]
    );
}

#[test]
fn dollar_joins_an_identifier_where_the_language_says() {
    let text = "$f f f$ $f$ f";
    assert_eq!(
        scan_names(Language::TypeScript, text, &["f"]),
        vec![3..4, 12..13]
    );
    assert_eq!(scan_names(Language::Java, text, &["f"]), vec![3..4, 12..13]);
    assert_eq!(
        scan_names(Language::Php, "$f f", &["f"]),
        vec![1..2, 3..4],
        "a PHP variable is its name after the dollar"
    );
}

#[test]
fn a_dockerfile_name_with_a_dash_is_one_identifier() {
    let text = "AS rust-builder\nCOPY --from=builder x";
    assert_eq!(
        scan_names(Language::Dockerfile, text, &["builder"]),
        vec![28..35]
    );
}

#[test]
fn php_names_match_whatever_their_case() {
    let text = "Foo foo FOO";
    assert_eq!(Case::of(Language::Php), Case::Insensitive);
    assert_eq!(Case::of(Language::Python), Case::Sensitive);
    assert_eq!(
        scan_names(Language::Php, text, &["foo"]),
        vec![0..3, 4..7, 8..11]
    );
    assert_eq!(scan_names(Language::Python, text, &["foo"]), vec![4..7]);
}

#[test]
fn two_spellings_of_one_place_are_one_occurrence() {
    let text = "alpha beta";
    assert_eq!(
        scan_names(Language::Python, text, &["alpha", "beta", "alpha"]),
        vec![0..5, 6..10]
    );
}

fn classified_source() -> (String, Vec<(Range<u32>, Lexical)>) {
    let text =
        "# helper(x)\ndef helper(y):\n    return \"helper\" + f\"{helper(y)}\"\n".to_string();
    let literal = text.find("\"helper\"").expect("the literal");
    let fstring = text.find("f\"{").expect("the f-string") + 1;
    let tail = text.find("}\"").expect("the end of the f-string") + 1;
    let lexical = vec![
        (0..11, Lexical::Comment),
        (literal as u32..literal as u32 + 8, Lexical::String),
        (fstring as u32..fstring as u32 + 1, Lexical::String),
        (tail as u32..tail as u32 + 1, Lexical::String),
    ];
    (text, lexical)
}

#[test]
fn a_comment_is_no_occurrence_a_string_is_a_mention_and_the_rest_is_code() {
    let (text, lexical) = classified_source();
    let file = file_with(Language::Python, lexical);
    let found = scan(&file, text.as_bytes(), &["helper"], Case::Sensitive);
    assert!(!found.lost);
    let code: Vec<(u32, Class)> = found.code.iter().map(|hit| (hit.line, hit.class)).collect();
    assert_eq!(code, vec![(2, Class::Code), (3, Class::Code)]);
    let mentions: Vec<(u32, Class)> = found
        .mentions
        .iter()
        .map(|hit| (hit.line, hit.class))
        .collect();
    assert_eq!(mentions, vec![(3, Class::Mention)]);
    let declared = text.find("def helper").expect("the definition") + 4;
    assert_eq!(found.code[0].bytes, declared as u32..declared as u32 + 6);
    assert_eq!(found.code[0].file, path("src/sample"));
}

#[test]
fn a_file_with_a_loss_reads_every_spelling_as_code() {
    let (text, lexical) = classified_source();
    let mut blob = BlobFacts::new(Language::Python);
    blob.lexical = lexical;
    blob.parse_error(vec![(1, 1)]);
    let file = file_of(blob);
    let found = scan(&file, text.as_bytes(), &["helper"], Case::Sensitive);
    assert!(found.lost);
    assert_eq!(found.code.len(), 4, "the comment and the literal count too");
    assert!(found.mentions.is_empty());
}

#[test]
fn a_language_without_ranges_has_only_code() {
    let (text, _) = classified_source();
    let file = file_with(Language::Python, Vec::new());
    let found = scan(&file, text.as_bytes(), &["helper"], Case::Sensitive);
    assert_eq!(found.code.len(), 4);
}

#[test]
fn the_range_of_a_byte_is_found_after_sorting() {
    let mut blob = BlobFacts::new(Language::Python);
    assert_eq!(blob.lexical_at(0), None);
    blob.lexical = vec![(8..9, Lexical::String), (2..5, Lexical::Comment)];
    blob.in_source_order();
    let seen: Vec<Option<Lexical>> = (0..10).map(|at| blob.lexical_at(at)).collect();
    let (c, s) = (Some(Lexical::Comment), Some(Lexical::String));
    assert_eq!(seen, vec![None, None, c, c, c, None, None, None, s, None]);
}

/// The names the fixture's own text is made of, with the ranges its strings
/// take (a literal is cut around its `{...}` pieces: those are code).
fn string_pieces(text: &str) -> Vec<(Range<u32>, Lexical)> {
    let bytes = text.as_bytes();
    let mut pieces = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'"' {
            at += 1;
            continue;
        }
        let mut piece = at;
        let mut end = at + 1;
        while bytes[end] != b'"' {
            if bytes[end] == b'{' {
                pieces.push((piece as u32..end as u32, Lexical::String));
                while bytes[end] != b'}' {
                    end += 1;
                }
                piece = end + 1;
            }
            end += 1;
        }
        pieces.push((piece as u32..end as u32 + 1, Lexical::String));
        at = end + 1;
    }
    pieces
}

fn line_of(text: &str, at: u32) -> u32 {
    1 + text.as_bytes()[..at as usize]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count() as u32
}

#[test]
fn the_scan_of_a_fixture_file_agrees_with_a_word_match() {
    let text = fixture("garbage_handlers.py");
    let strings = string_pieces(&text);
    let file = file_with(Language::Python, strings.clone());
    let names = [
        "event",
        "event_type",
        "garbage_event_dispatcher",
        "garbage_request_alpha_processed",
        "GarbageHandlerAlpha",
        "self",
        "request",
    ];
    let mut mentions_seen = 0;
    for name in names {
        let word = Regex::new(&format!(r"\b{}\b", regex::escape(name))).expect("a pattern");
        let (mut code, mut mentions) = (Vec::new(), Vec::new());
        for hit in word.find_iter(&text) {
            let bytes = hit.start() as u32..hit.end() as u32;
            let inside = strings
                .iter()
                .any(|(range, _)| range.start <= bytes.start && bytes.start < range.end);
            if inside {
                mentions.push(bytes);
            } else {
                code.push(bytes);
            }
        }
        let found = scan(&file, text.as_bytes(), &[name], Case::Sensitive);
        let seen_code: Vec<Range<u32>> = found.code.iter().map(|hit| hit.bytes.clone()).collect();
        let seen_mentions: Vec<Range<u32>> =
            found.mentions.iter().map(|hit| hit.bytes.clone()).collect();
        assert_eq!(seen_code, code, "code spellings of {name}");
        assert_eq!(seen_mentions, mentions, "mentions of {name}");
        for hit in found.code.iter().chain(&found.mentions) {
            assert_eq!(hit.line, line_of(&text, hit.bytes.start), "line of {name}");
        }
        mentions_seen += mentions.len();
    }
    assert!(
        mentions_seen > 0,
        "the fixture holds a name inside a string"
    );
}

#[test]
fn every_definition_name_the_extractor_reads_is_an_occurrence_at_its_span() {
    let text = fixture("garbage_handlers.py");
    let blob = PythonExtractor.extract(text.as_bytes(), &RunBudget::default());
    let file = file_of(blob);
    let mut read = 0;
    for def in file.defs() {
        let name = def.local.name.as_str();
        let found = scan(&file, text.as_bytes(), &[name], Case::Sensitive);
        let at = found
            .code
            .iter()
            .find(|hit| hit.bytes == def.name_span.bytes)
            .unwrap_or_else(|| panic!("{name} has no occurrence at its name span"));
        assert_eq!(at.line, def.name_span.lines.0, "line of {name}");
        read += 1;
    }
    assert!(read >= 8, "the fixture's definitions were read: {read}");
}

/// A name placed among the words of a real file, in code, in comments and in
/// strings, with and without identifier bytes glued to it: the scan finds
/// exactly the bare ones, and says which are code and which are mentions.
#[test]
fn a_placed_identifier_is_found_by_the_place_it_was_put_in() {
    const NAME: &str = "QzxUniq";
    let source = fixture("garbage_services.py");
    let mut words: Vec<String> = Regex::new(r"[A-Za-z_][A-Za-z0-9_]*")
        .expect("a pattern")
        .find_iter(&source)
        .map(|word| word.as_str().to_string())
        .collect();
    words.sort();
    words.dedup();
    assert!(words.len() > 10);
    for glued in [
        format!("{NAME}x"),
        format!("x{NAME}"),
        format!("_{NAME}"),
        format!("{NAME}9"),
    ] {
        words.push(glued);
    }
    let bare = words.len();
    let segments = proptest::collection::vec(
        (0u8..3, proptest::collection::vec(0usize..=bare, 1..5)),
        1..12,
    );
    let config = Config {
        cases: 256,
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(0x0cc0_0001),
        ..Config::default()
    };
    let outcome = TestRunner::new(config).run(&segments, |segments| {
        let mut text = String::new();
        let mut lexical = Vec::new();
        let (mut code, mut mentions) = (Vec::new(), Vec::new());
        for (index, (kind, picks)) in segments.iter().enumerate() {
            let line = 1 + index as u32;
            let start = text.len();
            let (open, close) = match *kind {
                1 => ("# ", ""),
                2 => ("\"", "\""),
                _ => ("", ""),
            };
            text.push_str(open);
            for (n, pick) in picks.iter().enumerate() {
                if n > 0 {
                    text.push(' ');
                }
                if *pick == bare {
                    let at = text.len() as u32;
                    let placed = (at..at + NAME.len() as u32, line);
                    match *kind {
                        1 => {}
                        2 => mentions.push(placed),
                        _ => code.push(placed),
                    }
                    text.push_str(NAME);
                } else {
                    text.push_str(&words[*pick]);
                }
            }
            text.push_str(close);
            match *kind {
                1 => lexical.push((start as u32..text.len() as u32, Lexical::Comment)),
                2 => lexical.push((start as u32..text.len() as u32, Lexical::String)),
                _ => {}
            }
            text.push('\n');
        }
        let file = file_with(Language::Python, lexical);
        let found = scan(&file, text.as_bytes(), &[NAME], Case::Sensitive);
        let seen = |hits: &[Occurrence]| -> Vec<(Range<u32>, u32)> {
            hits.iter()
                .map(|hit| (hit.bytes.clone(), hit.line))
                .collect()
        };
        if seen(found.code.as_slice()) != code || seen(found.mentions.as_slice()) != mentions {
            return Err(TestCaseError::fail(format!(
                "code {:?} want {code:?}, mentions {:?} want {mentions:?} in {text:?}",
                seen(found.code.as_slice()),
                seen(found.mentions.as_slice())
            )));
        }
        Ok(())
    });
    if let Err(failure) = outcome {
        panic!("{failure}");
    }
}

fn key() -> DefKey {
    DefKey {
        project: ProjectId::default(),
        language: Language::Python,
        module: ModulePath::new("sample"),
        local: LocalKey::value("f"),
    }
}

fn account(relation: Relation) -> Account {
    Account::new(key(), relation, SideTag::Head)
}

fn entry(line: u32, relation: Option<Relation>, outcome: Outcome, basis: Basis) -> Entry {
    Entry {
        occurrence: Occurrence {
            file: path("app/x.py"),
            bytes: 10..11,
            line,
            class: Class::Code,
        },
        relation,
        outcome,
        basis,
    }
}

fn candidate(line: u32, relation: Option<Relation>) -> Entry {
    entry(
        line,
        relation,
        Outcome::Candidate(Reason::Resolver(CandidateReason::UntypedReceiver)),
        Basis::Unproven,
    )
}

fn hedges(verdict: Verdict) -> Vec<Hedge> {
    match verdict {
        Verdict::Complete => Vec::new(),
        Verdict::Partial(hedges) => hedges,
    }
}

#[test]
fn an_account_with_every_occurrence_explained_is_complete() {
    let mut account = account(Relation::Calls);
    account.entries = vec![
        entry(1, Some(Relation::Calls), Outcome::Ours, Basis::Resolved),
        entry(2, None, Outcome::Declaration, Basis::DefinitionName),
        entry(3, None, Outcome::NotOurs, Basis::Local),
        entry(
            4,
            None,
            Outcome::Alias(Alias {
                name: "g".into(),
                scope: AliasScope::File(path("app/x.py")),
                hops: 1,
            }),
            Basis::RenamedImport,
        ),
    ];
    account.mentions = 3;
    assert!(account.verdict().is_complete());
    assert_eq!(account.verdict().as_str(), "complete");
    assert!(account_of_nothing().verdict().is_complete());
}

fn account_of_nothing() -> Account {
    account(Relation::Reads)
}

#[test]
fn a_candidate_names_its_file_and_line() {
    let mut account = account(Relation::Calls);
    account.entries = vec![candidate(7, Some(Relation::Calls))];
    let verdict = account.verdict();
    assert_eq!(verdict.as_str(), "partial");
    let hedges = hedges(verdict);
    assert_eq!(hedges.len(), 1);
    assert_eq!(hedges[0].to_string(), "candidate at app/x.py:7");
}

#[test]
fn a_candidate_blocks_the_relation_it_carries_and_all_when_it_carries_none() {
    for relation in Relation::ALL {
        let mut other = account(relation);
        other.entries = vec![candidate(1, Some(Relation::Calls))];
        assert_eq!(
            other.verdict().is_complete(),
            relation != Relation::Calls,
            "a calls candidate against {relation:?}"
        );
        let mut open = account(relation);
        open.entries = vec![candidate(1, None)];
        assert!(
            !open.verdict().is_complete(),
            "no relation against {relation:?}"
        );
    }
}

#[test]
fn a_leak_makes_calls_partial_and_names_the_place() {
    let leak = entry(
        5,
        Some(Relation::PassesAsValue),
        Outcome::Leak(ValuePlace::Argument),
        Basis::ValuePosition,
    );
    let mut calls = account(Relation::Calls);
    calls.entries = vec![leak.clone()];
    let hedges = hedges(calls.verdict());
    assert_eq!(hedges.len(), 1);
    assert_eq!(hedges[0].to_string(), "leak at app/x.py:5");
    let mut reads = account(Relation::Reads);
    reads.entries = vec![leak];
    assert!(reads.verdict().is_complete());
}

#[test]
fn each_implicit_kind_blocks_calls_and_only_calls() {
    for kind in Implicit::ALL {
        let mut calls = account(Relation::Calls);
        calls.implicit = vec![kind];
        assert_eq!(hedges(calls.verdict()), vec![Hedge::Implicit(kind)]);
        let mut reads = account(Relation::Reads);
        reads.implicit = vec![kind];
        assert!(reads.verdict().is_complete(), "{kind:?} against reads");
    }
    let names: Vec<&str> = Implicit::ALL.iter().map(|kind| kind.as_str()).collect();
    let mut distinct = names.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), names.len());
}

#[test]
fn what_is_unread_or_unbounded_blocks_every_relation() {
    for relation in Relation::ALL {
        let mut unread = account(relation);
        unread.unread = vec![Unread {
            file: path("app/y.py"),
            loss: Loss::ReadFailed {
                path: path("app/y.py"),
                side: SideTag::Head,
            },
        }];
        assert_eq!(
            hedges(unread.verdict())[0].to_string(),
            "app/y.py was not read"
        );
        let mut bound = account(relation);
        bound.alias_bound = AliasBound::exceeded(MAX_ALIAS_HOPS + 1, 2);
        assert!(!bound.verdict().is_complete());
        let mut common = account(relation);
        common.too_common = Some(900);
        assert!(!common.verdict().is_complete());
    }
}

#[test]
fn the_alias_bound_is_past_the_constants_and_not_at_them() {
    assert_eq!(AliasBound::exceeded(MAX_ALIAS_HOPS, MAX_ALIAS_NAMES), None);
    assert_eq!(
        AliasBound::exceeded(MAX_ALIAS_HOPS + 1, 1),
        Some(AliasBound {
            hops: MAX_ALIAS_HOPS + 1,
            names: 1
        })
    );
    assert!(AliasBound::exceeded(1, MAX_ALIAS_NAMES + 1).is_some());
}

#[test]
fn only_the_bases_the_accounting_checks_prove_not_ours() {
    for basis in [
        Basis::Local,
        Basis::Builtin,
        Basis::OtherDefinition,
        Basis::Manifests,
        Basis::BareName,
    ] {
        assert!(basis.proves_not_ours(), "{basis:?}");
    }
    for basis in [Basis::Resolved, Basis::RenamedImport, Basis::Unproven] {
        assert!(!basis.proves_not_ours(), "{basis:?}");
    }
    assert_eq!(Outcome::NotOurs.as_str(), "not_ours");
    assert_eq!(
        Outcome::Candidate(Reason::Unexplained).as_str(),
        "candidate"
    );
    assert_eq!(Basis::Local.as_str(), "local");
}
