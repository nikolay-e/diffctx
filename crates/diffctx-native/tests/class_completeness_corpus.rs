// The two class-level properties of class_completeness.rs, over every case of
// the impact and symbol corpus. Each case's repository is the seed; its own
// expectations are not read except to find the subject (the changed
// definition and the files that call it).
//
// A  The subject's name is written into one more file. As a use the engine may
//    not read (`foo(x)` in an unchanged or a changed file with an unknown
//    extension, a string literal in a call of a parsed file) the answer names
//    the file or stays partial. As no use (a markdown file, a comment in the
//    definition's own file after the definition) the answer discloses what
//    the run without the insertion discloses. Class `H` is the second kind
//    of finding, a hedge over a spelling that is no use.
//
//   The regions a parsed file hides are written the same way, as a use: the
//   body of a macro, the text after a parse error, a word in command position
//   (a script, a Makefile, a CI step) and the text of a template. The answer
//   names the file, stays partial, or is the false absence.
//
// B  The files that call the subject are stripped of their imports, of their
//    type annotations and of their package clause, one transformation at a
//    time and then all three. The answer may weaken to a candidate or a gap,
//    never to a complete answer that names no caller.
//
// There is no resolver-off switch in the binary to drive instead: the
// transformations are spelling changes by file extension (support/classes.rs).
//
// Knobs:
//   DIFFCTX_CLASS_FILTER  only the cases whose name contains the text
//   DIFFCTX_CLASS_EVERY   every n-th case of a language, to sample
//   DIFFCTX_CLASS_REPORT  append every failing row to this file
//   DIFFCTX_CLASS_SITE    A only: run only the regions a parsed file hides
//                         (macro body, after a parse error, command position,
//                         template text) whose label contains the text
//   DIFFCTX_CLASS_ACCOUNT append, per region and language, how many runs the
//                         insertion made, named, hedged and called absent
//
// Rows read `class family case variant query complete_empty excerpt`, with
// the family being the case's language directory. Class `E` is a run that
// failed before it could be judged. No run is decided by a clock.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;
use walkdir::WalkDir;

#[path = "support/accounting.rs"]
mod accounting;
#[path = "support/fact_keys.rs"]
mod fact_keys;
#[path = "common/oracle/mod.rs"]
mod oracle;
mod support;

use oracle::case::{Case, Mode, Repo, parse};
use oracle::model::{Kind, Node};
use support::classes::{
    Finding, TRANSFORMS, comment_of, complete_empty, conclude, excerpt, extension, lists,
    literal_call, mention, proof_missing, regions, strip_everything, unmoved_by_no_use,
};
use support::{diffctx_unclocked, git, init_repo};

fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repository root")
        .join("tests")
        .join("cases")
}

struct Loaded {
    name: String,
    case: Case,
}

fn knob(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

// The cases of one language directory, in path order, as the corpus names
// them. A file that does not parse is the corpus gate's business.
fn load(language: &str) -> Vec<Loaded> {
    let filter = knob("DIFFCTX_CLASS_FILTER");
    let every: usize = knob("DIFFCTX_CLASS_EVERY")
        .map_or(1, |v| v.parse().expect("DIFFCTX_CLASS_EVERY is a number"));
    let mut loaded = Vec::new();
    for mode in [Mode::Impact, Mode::Symbol] {
        let dir = cases_root().join(mode.name()).join(language);
        if !dir.exists() {
            continue;
        }
        let mut paths: Vec<PathBuf> = WalkDir::new(&dir)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .map(walkdir::DirEntry::into_path)
            .filter(|p| p.extension().is_some_and(|x| x == "yaml" || x == "yml"))
            .collect();
        paths.sort();
        for path in paths {
            let rel = path.strip_prefix(&dir).unwrap_or(&path).with_extension("");
            let name = format!("{}/{language}/{}", mode.name(), rel.display());
            if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
                continue;
            }
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(case) = parse(&raw, stem, mode) {
                loaded.push(Loaded { name, case });
            }
        }
    }
    loaded
        .into_iter()
        .enumerate()
        .filter(|(at, _)| at % every == 0)
        .map(|(_, case)| case)
        .collect()
}

// ---- the subject of a case ----

struct Subject {
    // The bare name of the changed definition.
    name: String,
    // The file that defines it, where the case says so.
    definition: Option<String>,
    // The files that call it.
    callers: Vec<String>,
}

fn has_file(repo: &Repo, path: &str) -> bool {
    repo.initial_files.contains_key(path)
        || repo.changed_files.contains_key(path)
        || repo.untracked_files.contains_key(path)
}

fn bare(symbol: &str) -> Option<String> {
    let name = symbol.rsplit(['.', ':']).next()?;
    let identifier = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    identifier.then(|| name.to_string())
}

fn subject(case: &Case) -> Option<Subject> {
    let repo = &case.repo;
    let expect = &case.expect;
    let mut targets: Vec<Node> = Vec::new();
    targets.extend(
        expect
            .changed
            .iter()
            .flatten()
            .map(|text| Node::parse(text)),
    );
    targets.extend(expect.relations.iter().map(|r| Node::parse(&r.to)));
    targets.extend(expect.callers.keys().map(|t| Node::target(t)));
    if let Some(query) = &case.query.symbol {
        targets.push(match query.split_once(':') {
            Some((path, symbol)) => Node::new(path, Some(symbol)),
            None => Node::new("", Some(query)),
        });
    }
    let target = targets.into_iter().find(|n| n.symbol.is_some())?;
    let name = bare(target.symbol.as_deref()?)?;
    let definition = if has_file(repo, &target.path) {
        Some(target.path)
    } else {
        // The file of the change that spells the name.
        repo.changed_files
            .iter()
            .find(|(_, text)| text.contains(&name))
            .map(|(path, _)| path.clone())
    };
    let mut callers: Vec<String> = expect
        .relations
        .iter()
        .filter(|r| r.kind != Kind::TestsReach)
        .map(|r| Node::parse(&r.from).path)
        .chain(
            expect
                .callers
                .values()
                .flatten()
                .map(|from| Node::parse(from).path),
        )
        .filter(|path| has_file(repo, path))
        .collect();
    callers.sort();
    callers.dedup();
    Some(Subject {
        name,
        definition,
        callers,
    })
}

// ---- running a variant ----

struct Planted {
    tmp: TempDir,
    mode: Mode,
    base: String,
    worktree: bool,
}

fn write_files(root: &Path, files: &BTreeMap<String, String>) {
    for (rel, content) in files {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("mkdir");
        std::fs::write(path, content).expect("write");
    }
}

fn head(root: &Path) -> String {
    let out = support::scrubbed_sh(root, "git rev-parse HEAD")
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn materialise(mode: Mode, spec: &Repo) -> Planted {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path().join("repo");
    std::fs::create_dir_all(&root).expect("mkdir");
    init_repo(&root);
    git(&root, &["config", "commit.gpgsign", "false"]);
    write_files(&root, &spec.initial_files);
    git(&root, &["add", "-A"]);
    git(
        &root,
        &["commit", "--quiet", "-m", "Initial commit", "--allow-empty"],
    );
    let base = head(&root);
    let changes = !spec.changed_files.is_empty()
        || !spec.deleted_files.is_empty()
        || !spec.renamed_files.is_empty();
    if changes || mode == Mode::Impact {
        for (from, to) in &spec.renamed_files {
            let dst = root.join(to);
            std::fs::create_dir_all(dst.parent().expect("a parent")).expect("mkdir");
            std::fs::rename(root.join(from), &dst).expect("rename");
        }
        write_files(&root, &spec.changed_files);
        for rel in &spec.deleted_files {
            std::fs::remove_file(root.join(rel)).expect("delete");
        }
        if !spec.worktree {
            git(&root, &["add", "-A"]);
            git(
                &root,
                &[
                    "commit",
                    "--quiet",
                    "-m",
                    &spec.commit_message,
                    "--allow-empty",
                ],
            );
        }
    }
    write_files(&root, &spec.untracked_files);
    Planted {
        tmp,
        mode,
        base,
        worktree: spec.worktree,
    }
}

// The binary's JSON answer; None where a `--symbol` lookup names a symbol
// nothing defines.
fn ask(planted: &Planted, case: &Case) -> Result<Option<Value>, String> {
    let mut args: Vec<String> = Vec::new();
    match planted.mode {
        Mode::Impact => {
            let range = if planted.worktree {
                "HEAD".to_string()
            } else {
                format!("{}..HEAD", planted.base)
            };
            args.extend(["--diff".into(), range, "--mode".into(), "impact".into()]);
        }
        Mode::Symbol => {
            args.extend([
                "--symbol".into(),
                case.query.symbol.clone().unwrap_or_default(),
            ]);
        }
    }
    args.extend(["-f".into(), "json".into(), "-q".into(), "--unfold".into()]);
    // A cap that sheds rows would hide a listing the property reads.
    let mut own = case.query.args.iter();
    while let Some(arg) = own.next() {
        if arg == "--budget" {
            own.next();
        } else {
            args.push(arg.clone());
        }
    }
    args.extend(["--budget".into(), "1000000".into()]);
    let output: Output = diffctx_unclocked()
        .current_dir(planted.tmp.path().join("repo"))
        .args(&args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("DIFFCTX_NO_MARKER", "1")
        .env("DIFFCTX_CACHE_DIR", planted.tmp.path().join("cache"))
        .envs(&case.query.env)
        .output()
        .map_err(|e| format!("spawn: {e}"))?;
    if output.status.success() {
        return serde_json::from_slice(&output.stdout)
            .map(Some)
            .map_err(|e| format!("not JSON: {e}"));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if planted.mode == Mode::Symbol && stderr.contains("no definition of") {
        return Ok(None);
    }
    Err(format!(
        "exit {:?}: {}",
        output.status.code(),
        stderr.trim()
    ))
}

fn language_of(name: &str) -> String {
    name.split('/').nth(1).unwrap_or("").to_string()
}

fn finding(class: char, loaded: &Loaded, variant: &str, doc: &Value) -> Finding {
    Finding {
        class,
        family: language_of(&loaded.name),
        case: loaded.name.clone(),
        variant: variant.to_string(),
        query: loaded.case.mode.name().to_string(),
        complete_empty: complete_empty(doc),
        excerpt: excerpt(doc),
    }
}

fn error(class: char, loaded: &Loaded, variant: &str, why: &str) -> Finding {
    Finding {
        class: if class == 'B' { 'F' } else { 'E' },
        family: language_of(&loaded.name),
        case: loaded.name.clone(),
        variant: variant.to_string(),
        query: loaded.case.mode.name().to_string(),
        complete_empty: false,
        excerpt: why.chars().take(300).collect(),
    }
}

// ---- A ----

#[derive(Clone, Copy, PartialEq, Eq)]
enum Spelling {
    // The name may be a use: the answer may not call it complete and empty.
    Use,
    // The name is a comment or prose: the answer does not move.
    NoUse,
}

// A word nobody defines or calls, spelled in place of the subject's name in the
// run that a no-use variant is compared with: the file is as new there as in
// the variant, so what differs is the name alone.
const NEUTRAL: &str = "zzneutral";

// The variants of one case that spell `name`: the label, whether it is a use,
// the repository, the file that holds the spelling and, for a region a parsed
// file hides, its site.
type Variant = (&'static str, Spelling, Repo, String, Option<&'static str>);

fn spellings(loaded: &Loaded, subject: &Subject, name: &str) -> Vec<Variant> {
    let line = |path: &str| mention(path, name);
    let mut variants: Vec<Variant> = Vec::new();
    let mut elsewhere = |label, spelling, extra: &str, text: String, changed: bool| {
        let mut repo = loaded.case.repo.clone();
        let files = if changed {
            &mut repo.changed_files
        } else {
            &mut repo.initial_files
        };
        files.insert(extra.to_string(), text);
        variants.push((label, spelling, repo, extra.to_string(), None));
    };
    elsewhere(
        "unchanged .xyz",
        Spelling::Use,
        "zz_class/notes.xyz",
        line("zz_class/notes.xyz"),
        false,
    );
    elsewhere(
        "changed .xyz",
        Spelling::Use,
        "zz_class/changed.xyz",
        line("zz_class/changed.xyz"),
        true,
    );
    elsewhere(
        "unchanged .md",
        Spelling::NoUse,
        "zz_class/notes.md",
        line("zz_class/notes.md"),
        false,
    );
    if let Some(own) = &subject.definition {
        let literal = format!("zz_class/literal.{}", extension(own));
        if let Some(text) = literal_call(&literal, name) {
            elsewhere(
                "unchanged, string literal in a call",
                Spelling::Use,
                &literal,
                text,
                false,
            );
        }
        let untouched = !loaded.case.repo.deleted_files.contains(own)
            && !loaded.case.repo.renamed_files.contains_key(own);
        if comment_of(own).is_some() && untouched && extension(own) != "php" {
            let mut repo = loaded.case.repo.clone();
            let mut wrote = false;
            for files in [&mut repo.initial_files, &mut repo.changed_files] {
                if let Some(text) = files.get_mut(own) {
                    text.push_str(&line(own));
                    wrote = true;
                }
            }
            if wrote {
                variants.push((
                    "definition's file, after",
                    Spelling::NoUse,
                    repo,
                    own.clone(),
                    None,
                ));
            }
        }
    }
    for region in regions(subject.definition.as_deref(), name) {
        let mut repo = loaded.case.repo.clone();
        repo.initial_files.insert(region.path.clone(), region.text);
        variants.push((
            region.label,
            Spelling::Use,
            repo,
            region.path,
            Some(region.site),
        ));
    }
    variants
}

// What the insertions of one region made of the answers, in one language.
#[derive(Default)]
struct Tally {
    inserted: usize,
    // The answer lists the inserted file.
    named: usize,
    // The answer names no such file and is no complete "nobody uses it": it
    // stays partial. `unmoved` of them were as partial without the name.
    hedged: usize,
    unmoved: usize,
    // A complete answer that nobody uses the subject: the finding.
    absent: usize,
    errored: usize,
    unanswered: usize,
}

fn account(test: &str, tallies: &BTreeMap<(&'static str, &'static str), Tally>) {
    let Some(path) = knob("DIFFCTX_CLASS_ACCOUNT") else {
        return;
    };
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    for ((site, label), t) in tallies {
        writeln!(
            file,
            "{test}\t{site}\t{label}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            t.inserted, t.named, t.hedged, t.unmoved, t.absent, t.errored, t.unanswered
        )
        .expect("write the account");
    }
}

fn enumeration(language: &str) {
    let mut findings = Vec::new();
    let mut tallies: BTreeMap<(&'static str, &'static str), Tally> = BTreeMap::new();
    let only_sites = knob("DIFFCTX_CLASS_SITE");
    let mut checked = 0;
    for loaded in load(language) {
        let Some(subject) = subject(&loaded.case) else {
            continue;
        };
        let variants = spellings(&loaded, &subject, &subject.name);
        let neutral = spellings(&loaded, &subject, NEUTRAL);
        for ((label, spelling, repo, path, site), (_, _, neutral_repo, _, _)) in
            variants.into_iter().zip(neutral)
        {
            if let Some(only) = &only_sites
                && !(site.is_some() && label.contains(only.as_str()))
            {
                continue;
            }
            let planted = materialise(loaded.case.mode, &repo);
            checked += 1;
            let tally = site.map(|site| tallies.entry((site, label)).or_default());
            let doc = match ask(&planted, &loaded.case) {
                Err(why) => {
                    if let Some(tally) = tally {
                        tally.inserted += 1;
                        tally.errored += 1;
                    }
                    findings.push(error('A', &loaded, label, &why));
                    continue;
                }
                Ok(None) => {
                    if let Some(tally) = tally {
                        tally.unanswered += 1;
                    }
                    continue;
                }
                Ok(Some(doc)) => doc,
            };
            if spelling == Spelling::Use {
                let named = lists(&doc, &path);
                let absent = !named && complete_empty(&doc);
                if let Some(tally) = tally {
                    tally.inserted += 1;
                    if named {
                        tally.named += 1;
                    } else if absent {
                        tally.absent += 1;
                    } else {
                        tally.hedged += 1;
                        let neutral = materialise(loaded.case.mode, &neutral_repo);
                        if matches!(
                            ask(&neutral, &loaded.case),
                            Ok(Some(baseline)) if unmoved_by_no_use(&doc, &baseline)
                        ) {
                            tally.unmoved += 1;
                        }
                    }
                }
                if absent {
                    findings.push(finding('A', &loaded, label, &doc));
                }
                continue;
            }
            let neutral = materialise(loaded.case.mode, &neutral_repo);
            match ask(&neutral, &loaded.case) {
                Ok(Some(baseline)) if !unmoved_by_no_use(&doc, &baseline) => {
                    findings.push(finding('H', &loaded, label, &doc));
                }
                Err(why) => findings.push(error('A', &loaded, label, &why)),
                _ => {}
            }
        }
    }
    account(&format!("corpus A/{language}"), &tallies);
    conclude(&format!("corpus A/{language}"), checked, &findings);
}

// ---- B ----

fn proof(language: &str) {
    let mut findings = Vec::new();
    let mut checked = 0;
    for loaded in load(language) {
        let Some(subject) = subject(&loaded.case) else {
            continue;
        };
        if subject.callers.is_empty() {
            continue;
        }
        let mut variants: Vec<(&str, fn(&str, &str) -> Option<String>)> = TRANSFORMS.to_vec();
        variants.push(("strip_everything", strip_everything));
        for (label, transform) in variants {
            let mut repo = loaded.case.repo.clone();
            let mut touched = false;
            for caller in &subject.callers {
                for files in [
                    &mut repo.initial_files,
                    &mut repo.changed_files,
                    &mut repo.untracked_files,
                ] {
                    let Some(text) = files.get(caller) else {
                        continue;
                    };
                    if let Some(stripped) = transform(caller, text) {
                        // A caller that no longer spells the subject proves
                        // nothing.
                        if stripped.contains(&subject.name) {
                            files.insert(caller.clone(), stripped);
                            touched = true;
                        }
                    }
                }
            }
            if !touched {
                continue;
            }
            let planted = materialise(loaded.case.mode, &repo);
            checked += 1;
            match ask(&planted, &loaded.case) {
                Err(why) => findings.push(error('B', &loaded, label, &why)),
                Ok(None) => {}
                Ok(Some(doc)) => {
                    let names_none = subject
                        .callers
                        .iter()
                        .all(|caller| proof_missing(&doc, caller));
                    if names_none {
                        findings.push(finding('B', &loaded, label, &doc));
                    }
                }
            }
        }
    }
    conclude(&format!("corpus B/{language}"), checked, &findings);
}

macro_rules! per_language {
    ($($name:ident),* $(,)?) => {
        mod a_enumeration {
            use super::*;
            $(#[test] fn $name() { enumeration(stringify!($name)); })*
        }
        mod b_proof {
            use super::*;
            $(#[test] fn $name() { proof(stringify!($name)); })*
        }
    };
}

per_language! {
    python, typescript, rust, kotlin, java, php, c, cpp, swift, go, shell, config, terraform,
    dockerfile, web, sql, other,
}

// The language directories the macro names are the directories the corpus has.
#[test]
fn every_corpus_directory_is_swept() {
    let mut on_disk: Vec<String> = Vec::new();
    for mode in [Mode::Impact, Mode::Symbol] {
        let Ok(entries) = std::fs::read_dir(cases_root().join(mode.name())) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            if entry.path().is_dir() {
                on_disk.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    on_disk.sort();
    on_disk.dedup();
    let swept = [
        "python",
        "typescript",
        "rust",
        "kotlin",
        "java",
        "php",
        "c",
        "cpp",
        "swift",
        "go",
        "shell",
        "config",
        "terraform",
        "dockerfile",
        "web",
        "sql",
        "other",
    ];
    for dir in on_disk {
        assert!(swept.contains(&dir.as_str()), "{dir} is not swept");
    }
}
