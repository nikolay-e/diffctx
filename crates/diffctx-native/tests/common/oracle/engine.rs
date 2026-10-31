//! Builds a case's repository, runs the shipped binary on it, and reads the
//! JSON answer into the relations the judge compares.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

use super::case::{Case, Mode, Repo};
use super::model::{Kind, Node, Relation, Resolution, UnknownClass};

/// What the engine said, as relations and lists.
#[derive(Default)]
pub struct Answer {
    pub relations: Vec<Relation>,
    /// Definitions under `changed[]`: for `--symbol`, the ones it resolved.
    pub definitions: Vec<Node>,
    /// Every changed definition the answer names anywhere, listed or not.
    pub touched: Vec<Node>,
    pub removed: Vec<Node>,
    pub untested: Vec<Node>,
    /// A definition whose callers the engine could not resolve, with the
    /// class of its sentence (`None` when no class matches it).
    pub unknowns: Vec<(Node, Option<UnknownClass>, String)>,
    /// A definition the answer discloses a gap for: an unknown, files that
    /// spell the name but were not analysed.
    pub gapped: Vec<Node>,
    pub stale: Vec<String>,
    pub named_by: Vec<String>,
    pub contracts: Vec<Node>,
    pub empty: bool,
    pub tests_known: bool,
    /// A limit, an unanalysed or withheld file, or a name no file defines.
    pub run_disclosure: bool,
    pub markdown: Option<String>,
}

/// Phrases the text form uses to claim an absence.
const ABSENCE_CLAIMS: &[&str] = &[
    "nothing calls them",
    "No resolved static callers",
    "No changes to analyse",
    "no callers",
    "no definition found",
    "untested",
];

const ZERO_CALLERS: &str = "0 caller(s)";

/// A header count of zero is a claim only where the same answer says callers
/// were not traced; otherwise it is a count of what was found.
const NOT_TRACED: &[&str] = &[
    "not traced",
    "unresolved",
    "not resolved",
    "not analysed",
    "only in part",
    "not followed",
    "(partial",
];

pub fn absence_claims(markdown: &str) -> Vec<String> {
    let mut claims: Vec<String> = ABSENCE_CLAIMS
        .iter()
        .filter(|phrase| markdown.contains(*phrase))
        .map(|phrase| (*phrase).to_string())
        .collect();
    let counted_zero = markdown.match_indices(ZERO_CALLERS).any(|(at, _)| {
        let digit_before = markdown[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_digit());
        let after = &markdown[at + ZERO_CALLERS.len()..];
        !digit_before && !after.starts_with(" without a static test link")
    });
    if counted_zero && NOT_TRACED.iter().any(|p| markdown.contains(p)) {
        claims.push(ZERO_CALLERS.to_string());
    }
    claims
}

pub fn run(case: &Case, name: &str) -> Result<Answer, String> {
    let repo = materialise(case.mode, &case.repo)?;
    dump(name, "repo.txt", repo_summary(&repo).as_bytes());
    let json = ask(&repo, case, "json")?;
    let mut answer = match json {
        Some(output) => {
            dump(name, "json", &output.stdout);
            let doc: Value = serde_json::from_slice(&output.stdout)
                .map_err(|e| format!("the answer is not JSON: {e}"))?;
            extract(&doc)
        }
        None => Answer {
            empty: true,
            ..Answer::default()
        },
    };
    if case.expect.no_negative_claims {
        let output = ask(&repo, case, "md")?;
        if let Some(output) = &output {
            dump(name, "md", &output.stdout);
        }
        answer.markdown = Some(output.map_or_else(String::new, |o| {
            String::from_utf8_lossy(&o.stdout).into_owned()
        }));
    }
    Ok(answer)
}

/// `DIFFCTX_ORACLE_DUMP=<dir>`: the raw answers and the repository of every
/// case, as `<dir>/<mode>/<language>/<name>.<ext>`, for reading a red case.
fn dump(name: &str, ext: &str, bytes: &[u8]) {
    let Some(dir) = std::env::var_os("DIFFCTX_ORACLE_DUMP") else {
        return;
    };
    let path = Path::new(&dir).join(format!("{name}.{ext}"));
    let written = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&path, bytes));
    if let Err(e) = written {
        eprintln!("yaml_cases: could not write {path:?}: {e}");
    }
}

fn repo_summary(repo: &Materialised) -> String {
    let root = repo.dir.path();
    let log = git(root, &["log", "--stat", "--format=%h %s"]).unwrap_or_else(|e| e);
    let status = git(root, &["status", "--short"]).unwrap_or_else(|e| e);
    format!("{log}\n\nstatus --short:\n{status}\n")
}

fn safe_relative(rel: &str) -> Result<(), String> {
    let escapes = rel.is_empty()
        || rel.starts_with('/')
        || rel.split('/').any(|part| part == "..")
        || rel.contains('\\');
    if escapes {
        Err(format!("`{rel}` is not a path inside the repository"))
    } else {
        Ok(())
    }
}

fn write_files(root: &Path, files: &BTreeMap<String, String>) -> Result<(), String> {
    for (rel, content) in files {
        safe_relative(rel)?;
        let full = root.join(rel);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
        }
        std::fs::write(&full, content).map_err(|e| format!("write {full:?}: {e}"))?;
    }
    Ok(())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .args(args)
        .output()
        .map_err(|e| format!("spawn git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A repository holding `initial_files` in one commit and, unless the case is
/// a lookup without a change, the change on top: committed, or left in the
/// working tree for `worktree`. The base commit is recorded in `base`.
struct Materialised {
    dir: TempDir,
    base: String,
}

fn materialise(mode: Mode, spec: &Repo) -> Result<Materialised, String> {
    let dir = TempDir::new().map_err(|e| format!("tempdir: {e}"))?;
    let root = dir.path();
    git(root, &["init", "--quiet", "-b", "main"])?;
    git(root, &["config", "user.email", "test@example.com"])?;
    git(root, &["config", "user.name", "test"])?;
    git(root, &["config", "commit.gpgsign", "false"])?;
    write_files(root, &spec.initial_files)?;
    git(root, &["add", "-A"])?;
    git(
        root,
        &["commit", "--quiet", "-m", "Initial commit", "--allow-empty"],
    )?;
    let base = git(root, &["rev-parse", "HEAD"])?;

    let changes = !spec.changed_files.is_empty()
        || !spec.deleted_files.is_empty()
        || !spec.renamed_files.is_empty();
    if changes || mode == Mode::Impact {
        // Renames first, so a `changed_files` entry for the destination
        // overwrites the moved content; deletions last, so a case cannot
        // resurrect a path it deletes.
        for (from, to) in &spec.renamed_files {
            safe_relative(from)?;
            safe_relative(to)?;
            let dst = root.join(to);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
            }
            std::fs::rename(root.join(from), &dst)
                .map_err(|e| format!("rename {from} -> {to}: {e}"))?;
        }
        write_files(root, &spec.changed_files)?;
        for rel in &spec.deleted_files {
            safe_relative(rel)?;
            std::fs::remove_file(root.join(rel)).map_err(|e| format!("delete {rel}: {e}"))?;
        }
        if !spec.worktree {
            git(root, &["add", "-A"])?;
            git(
                root,
                &[
                    "commit",
                    "--quiet",
                    "-m",
                    &spec.commit_message,
                    "--allow-empty",
                ],
            )?;
        }
    }
    write_files(root, &spec.untracked_files)?;
    Ok(Materialised { dir, base })
}

/// The binary's answer in `format`, or `None` when a `--symbol` lookup names
/// a symbol nothing defines (the binary exits nonzero saying so).
fn ask(repo: &Materialised, case: &Case, format: &str) -> Result<Option<Output>, String> {
    let mut args: Vec<String> = Vec::new();
    match case.mode {
        Mode::Impact => {
            let range = if case.repo.worktree {
                "HEAD".to_string()
            } else {
                format!("{}..HEAD", repo.base)
            };
            args.extend(["--diff".into(), range, "--mode".into(), "impact".into()]);
        }
        Mode::Symbol => {
            let symbol = case.query.symbol.clone().unwrap_or_default();
            args.extend(["--symbol".into(), symbol]);
        }
    }
    args.extend(["-f".into(), format.into(), "-q".into()]);
    args.extend(case.query.args.iter().cloned());

    let mut command = match std::env::var_os("DIFFCTX_BIN") {
        Some(bin) => Command::new(bin),
        None => crate::support::diffctx(),
    };
    let output = command
        .current_dir(repo.dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(&args)
        .output()
        .map_err(|e| format!("run diffctx: {e}"))?;
    if output.status.success() {
        return Ok(Some(output));
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    if case.mode == Mode::Symbol && stderr.contains("no definition of") {
        return Ok(None);
    }
    Err(format!(
        "diffctx {} exited with {}: {}",
        args.join(" "),
        output.status,
        stderr.trim()
    ))
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn items<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn count(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn node_of(value: &Value) -> Node {
    Node::new(text(value, "path").unwrap_or(""), text(value, "symbol"))
}

/// The definition a `path::symbol (lines)` label names; a symbol-less
/// definition is labelled `path:lines`.
fn node_of_label(label: &str) -> Node {
    let head = label.split(" \u{2014} ").next().unwrap_or(label);
    let head = head.rsplit_once(" (").map_or(head, |(name, _)| name);
    if head.contains("::") {
        Node::parse(head)
    } else {
        Node::parse(head.rsplit_once(':').map_or(head, |(path, _)| path))
    }
}

fn kind_of(relation: &str) -> Kind {
    match relation {
        "reference" => Kind::Reads,
        "implementation" => Kind::Overrides,
        _ => Kind::Calls,
    }
}

fn resolution_of(evidence: &str) -> Resolution {
    match evidence {
        "resolved" => Resolution::Resolved,
        "candidate" => Resolution::Candidate,
        _ => Resolution::Unresolved,
    }
}

fn lines_of(value: &Value) -> Vec<u32> {
    numbers(value, "sites")
}

fn lines_in(value: &Value) -> Vec<u32> {
    numbers(value, "lines")
}

fn numbers(value: &Value, key: &str) -> Vec<u32> {
    items(value, key)
        .iter()
        .filter_map(Value::as_u64)
        .filter_map(|n| u32::try_from(n).ok())
        .collect()
}

fn path_relation(from: &Value, target: &str, resolution: Resolution) -> Relation {
    Relation {
        from: Node::new(text(from, "path").unwrap_or(""), None),
        to: Node::new(target, None),
        kind: Kind::ReferencesPath,
        resolution,
        sites: u32::try_from(count(from, "line")).into_iter().collect(),
        loose_kind: false,
    }
}

pub fn extract(doc: &Value) -> Answer {
    let mut answer = Answer {
        empty: doc.get("empty").and_then(Value::as_bool).unwrap_or(false),
        tests_known: doc
            .get("tests_known")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        run_disclosure: !items(doc, "limits").is_empty()
            || !items(doc, "unanalysed_files").is_empty()
            || count(doc, "unanalysed_files_omitted") > 0
            || count(doc, "withheld_files") > 0
            || doc.get("symbol_named").is_some_and(|v| !v.is_null()),
        ..Answer::default()
    };

    for changed in items(doc, "changed") {
        let definition = node_of(changed);
        answer.definitions.push(definition.clone());
        answer.touched.push(definition.clone());
        if text(changed, "kind") == Some("removed") {
            answer.removed.push(definition.clone());
        }
        if changed
            .get("untested")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            answer.untested.push(definition.clone());
        }
        for caller in items(changed, "callers") {
            // A word match is the textual section of the answer, not a caller.
            if text(caller, "evidence") == Some("name") {
                continue;
            }
            let from = node_of(caller);
            answer.relations.push(Relation {
                from: from.clone(),
                to: definition.clone(),
                kind: kind_of(text(caller, "relation").unwrap_or("call")),
                resolution: resolution_of(text(caller, "evidence").unwrap_or("name")),
                sites: lines_of(caller),
                loose_kind: false,
            });
            if let Some(test) = text(caller, "tested_by") {
                answer.relations.push(Relation {
                    from: Node::new(test, None),
                    to: from,
                    kind: Kind::TestsReach,
                    resolution: Resolution::Resolved,
                    sites: Vec::new(),
                    loose_kind: false,
                });
            }
        }
        if let Some(sentence) = text(changed, "unresolved") {
            answer.unknowns.push((
                definition.clone(),
                UnknownClass::of_sentence(sentence),
                sentence.to_string(),
            ));
            answer.gapped.push(definition.clone());
        }
        // A file that spells the name but was not fully analysed is named only
        // textually: a claim about that file, and a gap in the answer.
        let missed = items(changed, "missed_in");
        for file in missed {
            answer.relations.push(Relation {
                from: Node::new(text(file, "path").unwrap_or(""), None),
                to: definition.clone(),
                kind: Kind::Calls,
                resolution: Resolution::Unresolved,
                sites: lines_in(file),
                loose_kind: false,
            });
        }
        if !missed.is_empty() {
            answer.gapped.push(definition);
        }
    }

    for key in ["untested_unlisted", "untested_called_in_diff"] {
        for label in items(doc, key).iter().filter_map(Value::as_str) {
            let node = node_of_label(label);
            answer.touched.push(node.clone());
            answer.untested.push(node);
        }
    }
    for key in ["entry_unlisted", "layout_only"] {
        for label in items(doc, key).iter().filter_map(Value::as_str) {
            answer.touched.push(node_of_label(label));
        }
    }

    for reference in items(doc, "stale_references") {
        if let Some(target) = text(reference, "target") {
            answer
                .relations
                .push(path_relation(reference, target, Resolution::Resolved));
            answer
                .stale
                .push(text(reference, "path").unwrap_or("").to_string());
        }
    }
    for reference in items(doc, "named_by") {
        if let Some(target) = text(reference, "target") {
            let resolution = if reference.get("key").is_some_and(|k| !k.is_null()) {
                Resolution::Candidate
            } else {
                Resolution::Resolved
            };
            answer
                .relations
                .push(path_relation(reference, target, resolution));
            answer
                .named_by
                .push(text(reference, "path").unwrap_or("").to_string());
        }
    }
    // A name no file defines, spelled in files: the rows are candidates for
    // the queried name itself, which has no path.
    if let (Some(symbol), Some(named)) = (text(doc, "symbol"), doc.get("symbol_named")) {
        for row in items(named, "rows") {
            answer.relations.push(Relation {
                from: Node::new(text(row, "path").unwrap_or(""), None),
                to: Node::new(symbol, None),
                kind: Kind::Calls,
                resolution: Resolution::Candidate,
                sites: u32::try_from(count(row, "line")).into_iter().collect(),
                loose_kind: false,
            });
        }
    }
    answer.contracts = items(doc, "contracts").iter().map(node_of).collect();
    // A removed public API is a removal, whether or not anything refers to it.
    for contract in items(doc, "contracts") {
        if text(contract, "kind") == Some("removed_api") {
            let node = node_of(contract);
            answer.removed.push(node.clone());
            answer.touched.push(node);
        }
    }
    // `getCollection('blog')` reads every entry under `content/blog/`.
    let changed_files: Vec<&str> = items(doc, "changed_files")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for read in items(doc, "collection_reads") {
        let Some(collection) = text(read, "collection") else {
            continue;
        };
        let under = format!("content/{collection}/");
        for file in changed_files
            .iter()
            .filter(|f| f.starts_with(&under) || f.contains(&format!("/{under}")))
        {
            answer.relations.push(Relation {
                from: Node::new(text(read, "path").unwrap_or(""), None),
                to: Node::new(file, None),
                kind: Kind::Reads,
                resolution: Resolution::Resolved,
                sites: u32::try_from(count(read, "line")).into_iter().collect(),
                loose_kind: false,
            });
        }
    }
    answer
}
