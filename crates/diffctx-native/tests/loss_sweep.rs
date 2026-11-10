// A loss in the n-th file, for every kind of loss, for every family (arch
// migration, phase 1). `DIFFCTX_TEST_LOSS=<kind>:<n>` gives the n-th
// (0-based, by path) file of the changed definition's interaction group a
// loss of that kind, as `DIFFCTX_TEST_READ_FAULT` fails the n-th blob read:
// the answer is then an error, a named gap, or the whole truth the clean run
// gave. A file that spells the definition's name and has a loss is never read
// as a file that was seen, so its loss reaches the answer: of the files that
// spell the name, as many are struck as the sweep reaches, and each of them
// shows in the answer.
//
// The kinds are the variants of `Loss`: `ordinal` matches the enum without a
// wildcard, so a variant added to it stops this file compiling until it has a
// sample here and so a place in the sweep.
//
// No case reads the clock; a failing cell prints the knob that replays it:
//   DIFFCTX_TEST_LOSS=<kind>:<n> diffctx . --diff HEAD~1..HEAD --mode impact -f json

use std::collections::BTreeSet;
use std::process::Output;

use _diffctx::snapshot::{Loss, NotAnalysedReason, RepoPath, SideTag, Stage};
use serde_json::Value;

mod support;
use support::answer::{changed_rows, relations};
use support::families::{
    self, Family, Planted, Query, Shape, fingerprint, names_link, queries, says_empty,
};

// The number of variants of `Loss`; `ordinal` is the check that it is right.
const VARIANTS: usize = 12;

fn ordinal(loss: &Loss) -> usize {
    match loss {
        Loss::ReadFailed { .. } => 0,
        Loss::SnapshotChanged { .. } => 1,
        Loss::TooLarge { .. } => 2,
        Loss::Withheld { .. } => 3,
        Loss::NotAnalysed { .. } => 4,
        Loss::ManifestUnread { .. } => 5,
        Loss::ParseError { .. } => 6,
        Loss::GrammarOutdated { .. } => 7,
        Loss::Unsupported { .. } => 8,
        Loss::Deadline { .. } => 9,
        Loss::ExtractorFailed { .. } => 10,
        Loss::AmbiguousMatch { .. } => 11,
    }
}

fn samples() -> Vec<Loss> {
    let path = || RepoPath::new("sample.txt").expect("a path");
    vec![
        Loss::ReadFailed {
            path: path(),
            side: SideTag::Head,
        },
        Loss::SnapshotChanged { path: path() },
        Loss::TooLarge {
            path: path(),
            size: 1,
        },
        Loss::Withheld { count: 1 },
        Loss::NotAnalysed {
            path: path(),
            reason: NotAnalysedReason::Oversize,
        },
        Loss::ManifestUnread {
            path: path(),
            reason: "sample".to_string(),
        },
        Loss::ParseError {
            path: path(),
            lines: vec![(1, 1)],
        },
        Loss::GrammarOutdated {
            path: path(),
            lines: vec![(1, 1)],
        },
        Loss::Unsupported { path: path() },
        Loss::Deadline {
            stage: Stage::Search,
        },
        Loss::ExtractorFailed { path: path() },
        Loss::AmbiguousMatch { path: path() },
    ]
}

// A kind of loss and whether it is about a file.
struct Kind {
    name: &'static str,
    of_a_file: bool,
}

fn kinds() -> Vec<Kind> {
    let all = samples();
    let mut seen = [false; VARIANTS];
    for loss in &all {
        let at = ordinal(loss);
        assert!(at < VARIANTS, "{loss}: ordinal {at} is past VARIANTS");
        assert!(!seen[at], "{loss}: two samples for one variant");
        seen[at] = true;
    }
    assert!(seen.iter().all(|s| *s), "a variant of `Loss` has no sample");
    all.iter()
        .map(|loss| Kind {
            name: loss.kind(),
            of_a_file: loss.path().is_some(),
        })
        .collect()
}

#[test]
fn every_variant_of_loss_has_a_sample_and_a_kind_name() {
    let kinds = kinds();
    let names: BTreeSet<&str> = kinds.iter().map(|k| k.name).collect();
    assert_eq!(names.len(), VARIANTS, "two variants share a kind name");
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    // The run ended with git's or the engine's words and a code.
    Error,
    // The answer says more than the clean one: a gap, a limit, a file.
    Disclosed,
    // The clean answer, claim for claim.
    Same,
}

struct Clean {
    doc: Value,
    claims: BTreeSet<String>,
    fingerprint: String,
}

fn claims_of(doc: &Value) -> BTreeSet<String> {
    let mut all = relations(doc);
    all.extend(changed_rows(doc));
    all
}

fn clean(planted: &Planted, query: Query, caller: &str) -> Result<Clean, String> {
    let doc = planted.ask(query, &[])?;
    if !names_link(&doc, caller) {
        return Err(format!(
            "{query:?}: the clean run does not name {caller}, so the sweep measures nothing\n{doc:#}"
        ));
    }
    Ok(Clean {
        claims: claims_of(&doc),
        fingerprint: fingerprint(&doc),
        doc,
    })
}

fn judge(clean: &Clean, caller: &str, out: &Output) -> Result<Verdict, String> {
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return match out.status.code() {
            Some(1..=4) if !stderr.trim().is_empty() => Ok(Verdict::Error),
            code => Err(format!(
                "exit {code:?} with stderr {stderr:?}: an error names what failed and exits with its code"
            )),
        };
    }
    let doc: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("exit 0 with stdout that is not JSON ({e})"))?;
    if says_empty(&doc) && !says_empty(&clean.doc) {
        return Err("the answer says the change reaches nothing".to_string());
    }
    if fingerprint(&doc) != clean.fingerprint {
        return Ok(Verdict::Disclosed);
    }
    let claims = claims_of(&doc);
    if claims == clean.claims && names_link(&doc, caller) {
        return Ok(Verdict::Same);
    }
    Err(format!(
        "a smaller answer that looks complete\nmissing: {:?}\nextra: {:?}",
        clean.claims.difference(&claims).collect::<Vec<_>>(),
        claims.difference(&clean.claims).collect::<Vec<_>>()
    ))
}

struct Cell {
    query: Query,
    kind: &'static str,
    of_a_file: bool,
    n: usize,
}

// Every cell, run a few at a time: one process each.
fn run_cells(planted: &Planted, cells: &[Cell]) -> Vec<Output> {
    let width = std::thread::available_parallelism().map_or(4, |n| n.get().clamp(2, 8));
    let mut outs = Vec::with_capacity(cells.len());
    for batch in cells.chunks(width) {
        let batch_outs: Vec<Output> = std::thread::scope(|scope| {
            let runs: Vec<_> = batch
                .iter()
                .map(|cell| {
                    scope.spawn(move || {
                        let knob = format!("{}:{}", cell.kind, cell.n);
                        planted.run(cell.query, &[("DIFFCTX_TEST_LOSS", knob.as_str())])
                    })
                })
                .collect();
            runs.into_iter()
                .map(|run| run.join().expect("a sweep run"))
                .collect()
        });
        outs.extend(batch_outs);
    }
    outs
}

fn sweep(family: Family) {
    let planted = Planted::new(families::with_caller(family, &Shape::default()));
    let caller = planted.repo.caller.clone().expect("a seed has a caller");
    let group = planted.repo.group_files();
    let name = planted.repo.symbol.to_lowercase();
    let spelling = group
        .iter()
        .filter(|(_, text)| text.to_lowercase().contains(&name))
        .count();
    assert!(
        spelling >= 1,
        "{}: no file of the group spells {name}",
        family.label()
    );
    let mut cleans = Vec::new();
    for query in queries(family) {
        match (clean(&planted, query, &caller), query) {
            (Ok(answer), _) => cleans.push((query, answer)),
            // The by-name question is the second one: where it does not name
            // the caller to begin with there is nothing to lose from it.
            (Err(_), Query::Symbol) => {}
            (Err(why), Query::Impact) => panic!("{why}"),
        }
    }
    let mut cells = Vec::new();
    for (query, _) in &cleans {
        for kind in kinds() {
            // A loss about no file is one and the same whichever file it is set on.
            let reach = if kind.of_a_file { group.len() + 2 } else { 1 };
            for n in 0..reach {
                cells.push(Cell {
                    query: *query,
                    kind: kind.name,
                    of_a_file: kind.of_a_file,
                    n,
                });
            }
        }
    }
    let outs = run_cells(&planted, &cells);
    let mut broken = Vec::new();
    let mut seen: Vec<(Query, &str, usize)> = Vec::new();
    for (cell, out) in cells.iter().zip(&outs) {
        let (_, clean) = cleans
            .iter()
            .find(|(query, _)| *query == cell.query)
            .expect("a clean answer");
        match judge(clean, &caller, out) {
            Ok(Verdict::Error | Verdict::Disclosed) if cell.of_a_file => {
                match seen
                    .iter_mut()
                    .find(|(query, kind, _)| *query == cell.query && *kind == cell.kind)
                {
                    Some(found) => found.2 += 1,
                    None => seen.push((cell.query, cell.kind, 1)),
                }
            }
            Ok(_) => {}
            Err(why) => broken.push(format!(
                "{:?} DIFFCTX_TEST_LOSS={}:{}: {why}",
                cell.query, cell.kind, cell.n
            )),
        }
    }
    assert!(
        broken.is_empty(),
        "{}: {} cell(s) left an answer that is neither an error nor a named gap nor the whole truth\n{}",
        family.label(),
        broken.len(),
        broken.join("\n")
    );
    let mut missing = Vec::new();
    for (query, _) in &cleans {
        for kind in kinds().iter().filter(|k| k.of_a_file) {
            let shown = seen
                .iter()
                .find(|(q, k, _)| q == query && *k == kind.name)
                .map_or(0, |found| found.2);
            if shown < spelling {
                missing.push(format!(
                    "{query:?} {}: {shown} of the {spelling} files that spell {name} show their loss in the answer",
                    kind.name
                ));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "{}: a loss in a file that spells the name stayed out of the answer\n{}",
        family.label(),
        missing.join("\n")
    );
}

macro_rules! sweeps {
    ($($test:ident: $family:ident;)+) => {
        mod every_kind_of_loss_in_every_file {
            use super::{Family, sweep};

            $(
                #[test]
                fn $test() {
                    sweep(Family::$family);
                }
            )+
        }
    };
}

sweeps! {
    python: Python;
    typescript: TypeScript;
    rust: Rust;
    kotlin: Kotlin;
    java: Java;
    go: Go;
    php: Php;
    c: C;
    config: Config;
    terraform: Terraform;
    dockerfile: Dockerfile;
    shell: Shell;
}
