//! The accounting identity, in debug builds under `DIFFCTX_DEBUG_IDENTITY`: in every
//! file that may name a changed definition, each text match of each of its names is
//! exactly one record of the account — a use, a declaration, a not-ours with its
//! witness, a gap. No form is listed: the matches are counted from the bytes, the
//! records from the account, and a match left without a record is a violation
//! whatever region it lies in. `=<file>` appends one JSON line per account and per
//! violation; `=assert` panics on the first violation.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;

use serde_json::{Value, json};

use super::scanner::{Standing, matches, standing_at};
use super::{AliasScope, Case, Entry, Outcome};
use crate::decide::Gap;
use crate::facts::{FileFacts, Language, Name};
use crate::snapshot::{Bytes, RepoPath};

const ENV: &str = "DIFFCTX_DEBUG_IDENTITY";

/// What the account's reach was cut by, for the files it did not visit.
pub struct Bounds<'a> {
    /// The run's pathspecs.
    pub pathspec: &'a dyn Fn(&str) -> bool,
    /// A local's own file: its name elsewhere is another name.
    pub own_file: Option<&'a str>,
}

/// The files a name may be matched in, besides the ones the account visited.
pub struct Universe<'a> {
    /// The group's files of the head listing, read or not.
    pub group: &'a [RepoPath],
    /// Files the resolver read that no extension puts in the group.
    pub outside: &'a [RepoPath],
    /// Every file of the head listing: the matches outside the group are counted apart.
    pub listing: Vec<RepoPath>,
    pub read: &'a dyn Fn(&RepoPath) -> Option<Bytes>,
    pub bounds: &'a Bounds<'a>,
    /// The gaps of the run that stand against every relation.
    pub gaps: &'a [Gap],
}

enum Sink {
    File(OsString),
    Assert,
}

#[derive(Clone, Copy)]
enum Record {
    Use,
    Declaration,
    NotOurs,
    Gap,
}

fn record_of(outcome: &Outcome) -> Record {
    match outcome {
        Outcome::Ours | Outcome::Alias(_) | Outcome::Leak(_) => Record::Use,
        Outcome::Declaration => Record::Declaration,
        Outcome::NotOurs(_) => Record::NotOurs,
        Outcome::Candidate(_) => Record::Gap,
    }
}

#[derive(Default)]
struct Tally {
    matches: i64,
    uses: i64,
    declarations: i64,
    not_ours: i64,
    gaps: i64,
    unread_unchecked: usize,
    other_group_files: usize,
    other_group_matches: usize,
}

impl Tally {
    fn add(&mut self, record: Record, n: i64) {
        match record {
            Record::Use => self.uses += n,
            Record::Declaration => self.declarations += n,
            Record::NotOurs => self.not_ours += n,
            Record::Gap => self.gaps += n,
        }
    }
}

/// The identity of one definition read at one side.
pub struct Ledger {
    sink: Sink,
    def: String,
    side: String,
    tally: Tally,
    names: Vec<(Name, Option<AliasScope>)>,
    visited: HashSet<(Name, RepoPath)>,
    lines: String,
    violations: usize,
}

fn line_of(text: &[u8], at: u32) -> usize {
    1 + text[..at as usize].iter().filter(|b| **b == b'\n').count()
}

/// The matches of `name` in a file no facts were read for, by the language its path
/// and text name, else by the widest rule (no case, the fewest bytes that join a name).
fn count(path: &RepoPath, text: &[u8], name: &str) -> usize {
    let (language, case) = Language::detect(path, text)
        .map_or((Language::Config, Case::Insensitive), |l| (l, Case::of(l)));
    matches(text, name, language, case).len()
}

impl Ledger {
    /// The ledger of `def` at `side`, when the knob is set.
    pub fn open(def: &str, side: &str) -> Option<Self> {
        let value = std::env::var_os(ENV).filter(|value| !value.is_empty())?;
        let sink = if value == "assert" {
            Sink::Assert
        } else {
            Sink::File(value)
        };
        Some(Self {
            sink,
            def: def.to_string(),
            side: side.to_string(),
            tally: Tally::default(),
            names: Vec::new(),
            visited: HashSet::new(),
            lines: String::new(),
            violations: 0,
        })
    }

    /// The account looks for `name`, in `scope` when it is an alias.
    pub fn name(&mut self, name: &Name, scope: Option<&AliasScope>) {
        let entry = (name.clone(), scope.cloned());
        if !self.names.contains(&entry) {
            self.names.push(entry);
        }
    }

    fn violate(&mut self, kind: &str, name: &Name, file: &RepoPath, detail: Value) {
        self.violations += 1;
        let line = json!({
            "identity": "violation", "kind": kind, "def": self.def, "side": self.side,
            "name": name.as_str(), "file": file.as_str(), "detail": detail,
        });
        self.lines.push_str(&format!("{line}\n"));
    }

    /// One file the account read for `name`: `records` are the entries it made there,
    /// `mentions` and `comments` the matches it counted in strings and comments.
    pub fn file(
        &mut self,
        name: &Name,
        file: &FileFacts,
        text: &[u8],
        records: &[Entry],
        mentions: usize,
        comments: usize,
    ) {
        self.visited.insert((name.clone(), file.path.clone()));
        let language = file.language();
        let found = matches(text, name.as_str(), language, Case::of(language));
        let lossy = !file.blob.losses.is_empty();
        let mut at: BTreeMap<(u32, u32), usize> = BTreeMap::new();
        for entry in records {
            let bytes = &entry.occurrence.bytes;
            *at.entry((bytes.start, bytes.end)).or_default() += 1;
            self.tally.add(record_of(&entry.outcome), 1);
        }
        self.tally.matches += found.len() as i64;
        self.tally.not_ours += (mentions + comments) as i64;
        let (mut unrecorded, mut doubled) = (Vec::new(), Vec::new());
        let (mut in_strings, mut in_comments) = (0, 0);
        for range in &found {
            let recorded = at.remove(&(range.start, range.end)).unwrap_or(0);
            if recorded > 1 {
                doubled.push(line_of(text, range.start));
            }
            let standing = if lossy {
                Standing::Code
            } else {
                standing_at(&file.blob, range)
            };
            match (standing, recorded) {
                (Standing::Code, 0) => unrecorded.push(line_of(text, range.start)),
                (Standing::Mention, 0) => in_strings += 1,
                (Standing::Comment, 0) => in_comments += 1,
                (Standing::Code | Standing::Mention | Standing::Comment, _) => {}
            }
        }
        let path = &file.path;
        if !unrecorded.is_empty() {
            self.violate(
                "code_without_record",
                name,
                path,
                json!({ "lines": unrecorded }),
            );
        }
        if !doubled.is_empty() {
            self.violate("double_record", name, path, json!({ "lines": doubled }));
        }
        if !at.is_empty() {
            let lines: Vec<usize> = at.keys().map(|(start, _)| line_of(text, *start)).collect();
            self.violate(
                "record_without_match",
                name,
                path,
                json!({ "lines": lines }),
            );
        }
        if in_strings != mentions {
            let detail = json!({ "matches": in_strings, "recorded": mentions });
            self.violate("mentions_miscounted", name, path, detail);
        }
        if in_comments != comments {
            let detail = json!({ "matches": in_comments, "recorded": comments });
            self.violate("comments_unrecorded", name, path, detail);
        }
    }

    /// A file the account holds as unread: one gap covering every match of it.
    pub fn unread(&mut self, name: &Name, path: &RepoPath, text: Option<&[u8]>) {
        self.visited.insert((name.clone(), path.clone()));
        match text {
            Some(text) => {
                let n = count(path, text, name.as_str()) as i64;
                self.tally.matches += n;
                self.tally.gaps += n;
            }
            None => self.tally.unread_unchecked += 1,
        }
    }

    /// The files the account did not visit for each name: a match there is a gap of the
    /// run, a not-ours of a local's scope, or a violation. Files of other groups are
    /// counted apart.
    pub fn universe(&mut self, universe: &Universe<'_>) {
        let covered = (universe.gaps.iter()).any(|gap| matches!(gap, Gap::OutsideScope { .. }));
        let bounds = universe.bounds;
        let own: HashSet<&RepoPath> = universe.group.iter().chain(universe.outside).collect();
        let mut other_group: HashSet<&RepoPath> = HashSet::new();
        for (name, scope) in self.names.clone() {
            for path in universe.group.iter().chain(universe.outside) {
                let narrowed = matches!(&scope, Some(AliasScope::File(only)) if only != path);
                if narrowed || self.visited.contains(&(name.clone(), path.clone())) {
                    continue;
                }
                let Some(text) = (universe.read)(path) else {
                    self.tally.unread_unchecked += 1;
                    continue;
                };
                let n = count(path, &text, name.as_str());
                if n == 0 {
                    continue;
                }
                self.tally.matches += n as i64;
                if bounds.own_file.is_some_and(|own| own != path.as_str()) {
                    self.tally.not_ours += n as i64;
                } else if !(bounds.pathspec)(path.as_str()) {
                    if covered {
                        self.tally.gaps += n as i64;
                    } else {
                        let detail = json!({ "matches": n });
                        self.violate("outside_scope_without_gap", &name, path, detail);
                    }
                } else {
                    self.violate("not_accounted", &name, path, json!({ "matches": n }));
                }
            }
            if bounds.own_file.is_some() || scope.is_some() {
                continue;
            }
            for path in universe.listing.iter().filter(|path| !own.contains(path)) {
                let Some(text) = (universe.read)(path) else {
                    continue;
                };
                let n = count(path, &text, name.as_str());
                if n > 0 {
                    other_group.insert(path);
                    self.tally.other_group_matches += n;
                }
            }
        }
        self.tally.other_group_files = other_group.len();
    }

    /// Writes the account's line and its violations; `extra` are the gaps that stand on
    /// no match (a use that spells no name, code another runtime reads).
    pub fn finish(mut self, extra: usize) {
        let tally = &self.tally;
        let recorded = tally.uses + tally.declarations + tally.not_ours + tally.gaps;
        let names: Vec<&str> = self.names.iter().map(|(name, _)| name.as_str()).collect();
        let line = json!({
            "identity": "account", "def": self.def, "side": self.side, "names": names,
            "matches": tally.matches, "use": tally.uses, "declaration": tally.declarations,
            "not_ours": tally.not_ours, "gap": tally.gaps,
            "remainder": tally.matches - recorded,
            "unread_unchecked": tally.unread_unchecked, "extra_gaps": extra,
            "other_group": { "files": tally.other_group_files, "matches": tally.other_group_matches },
            "violations": self.violations,
        });
        self.lines.push_str(&format!("{line}\n"));
        match &self.sink {
            Sink::Assert => assert!(
                self.violations == 0,
                "{ENV}: the accounting identity does not hold:\n{}",
                self.lines
            ),
            Sink::File(path) => {
                let written = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
                    .and_then(|mut out| out.write_all(self.lines.as_bytes()));
                if let Err(error) = written {
                    eprintln!("{ENV}: {error}");
                }
            }
        }
    }
}
