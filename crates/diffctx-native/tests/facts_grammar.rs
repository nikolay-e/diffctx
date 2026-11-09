//! A parse error the newest release of the grammar does not have is named as
//! an outdated grammar, and the witness file the bump job writes keeps the
//! shape the reader of it expects.

use std::sync::Arc;

use _diffctx::facts::{BlobFacts, FileFacts, Language, ModulePath, ProjectId};
use _diffctx::snapshot::{Loss, RepoPath};

fn file(outdated_grammar: bool) -> FileFacts {
    let mut blob = BlobFacts::new(Language::Python);
    blob.parse_error(vec![(2, 3), (7, 7)]);
    FileFacts {
        path: RepoPath::new("pkg/new_syntax.py").expect("a path"),
        project: ProjectId::new(""),
        module: ModulePath::new("pkg.new_syntax"),
        blob: Arc::new(blob),
        outdated_grammar,
    }
}

#[test]
fn a_witnessed_blob_is_an_outdated_grammar_not_a_malformed_file() {
    let losses = file(true).losses();
    assert!(
        matches!(
            losses.as_slice(),
            [Loss::GrammarOutdated { path, lines }]
                if path.as_str() == "pkg/new_syntax.py" && lines == &[(2, 3), (7, 7)]
        ),
        "{losses:?}"
    );
    assert_eq!(
        losses[0].to_string(),
        "pkg/new_syntax.py: the pinned grammar reads lines 2-3, 7 as errors and a newer release reads them"
    );
    assert_eq!(losses[0].kind(), "grammar_outdated");
}

#[test]
fn an_unwitnessed_blob_stays_a_plain_parse_error() {
    let losses = file(false).losses();
    assert!(
        matches!(losses.as_slice(), [Loss::ParseError { .. }]),
        "{losses:?}"
    );
}

#[test]
fn the_witness_file_holds_a_language_and_a_blob_id_per_line() {
    let witnesses = include_str!("../src/facts/grammar_witnesses.txt");
    for line in witnesses
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        let (language, oid) = line.split_once(' ').expect("`<Language> <blob id>`");
        assert!(
            language.chars().all(|c| c.is_ascii_alphanumeric()),
            "{line}"
        );
        assert!(
            oid.len() == 40 && oid.bytes().all(|b| b.is_ascii_hexdigit()),
            "{line}"
        );
    }
}
