//! Builds a case's repository, runs the shipped binary on it, and reads the
//! JSON answer into the relations the judge compares.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use _diffctx::snapshot::git_command;
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
    /// Definitions whose entry role names a test that reaches them, and those
    /// whose role names none.
    pub entry_tested: Vec<Node>,
    pub entry_not_tested: Vec<Node>,
    /// A definition whose callers the engine could not resolve, with the
    /// class of its sentence (`None` when no class matches it).
    pub unknowns: Vec<(Node, Option<UnknownClass>, String)>,
    /// A definition the answer discloses a gap for: an unknown, files that
    /// spell the name but were not analysed.
    pub gapped: Vec<Node>,
    pub stale: Vec<String>,
    pub named_by: Vec<String>,
    /// (path, class) of every row of `stale_references` and `named_by`.
    pub mention_classes: Vec<(String, String)>,
    pub contracts: Vec<Node>,
    pub empty: bool,
    pub tests_known: bool,
    /// Every caller row of the JSON, word matches included: (path, evidence).
    pub caller_rows: Vec<(String, String)>,
    /// Callers the engine shed to fit its cap; the text header counts them.
    pub truncated: u64,
    /// Calls the answer states as a count, not as rows, which the text header
    /// counts too: what a fold stands for, and the call sites a change that only
    /// added defaulted fields leaves working.
    pub counted_not_listed: u64,
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
    "not determined",
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
            let repeated = crate::fact_keys::repeated_facts(&doc);
            if !repeated.is_empty() {
                return Err(format!(
                    "{}\nthe answer states one fact in more than one row:\n  {}",
                    super::ASSERTION_FAILED,
                    repeated.join("\n  ")
                ));
            }
            if let Some(lost) = crate::accounting::lost_definitions(&doc) {
                return Err(format!(
                    "{}\nthe answer leaves a changed definition it traced unnamed:\n  {lost}",
                    super::ASSERTION_FAILED
                ));
            }
            extract(&doc)?
        }
        None => Answer {
            empty: true,
            ..Answer::default()
        },
    };
    if case.expect.no_negative_claims || case.mode == Mode::Impact {
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
    let out = git_command(root)
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

/// The binary under test: `DIFFCTX_BIN` when set, else this build's.
fn binary() -> Command {
    match std::env::var_os("DIFFCTX_BIN") {
        Some(bin) => Command::new(bin),
        None => crate::support::diffctx(),
    }
}

/// Whether the binary lists `--unfold` in its help, asked once per run: the
/// capability decides, not where the binary came from.
fn supports_unfold() -> bool {
    static SUPPORTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SUPPORTED.get_or_init(|| {
        binary()
            .arg("--help")
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains("--unfold"))
    })
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
    // The corpus asserts relations; what the answer folds has its own tests. A
    // binary that predates the flag (the class test's base) is asked without it.
    if supports_unfold() {
        args.push("--unfold".into());
    }
    args.extend(case.query.args.iter().cloned());

    let mut command = binary();
    let output = command
        .current_dir(repo.dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(&case.query.env)
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

fn kind_of(relation: &str) -> Kind {
    match relation {
        "reference" => Kind::Reads,
        "implementation" => Kind::Overrides,
        "reexports" => Kind::Reexports,
        _ => Kind::Calls,
    }
}

fn resolution_of(evidence: &str) -> Resolution {
    match evidence {
        // A static re-export binds: the module offers the definition on.
        "resolved" | "reexport" => Resolution::Resolved,
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
        kind_stated: false,
    }
}

/// The kind a caller row states, when it does: a precise cell's rows carry
/// one, and a name that is no schema v1 kind is an engine defect the harness
/// cannot evaluate around.
fn stated_kind(caller: &Value) -> Result<Option<Kind>, String> {
    match text(caller, "kind") {
        None => Ok(None),
        Some(name) => Kind::parse(name).map(Some).ok_or_else(|| {
            format!(
                "a caller row of {} states the kind `{name}`, which is no relation kind",
                text(caller, "path").unwrap_or("?")
            )
        }),
    }
}

/// Lines of a name a gap may count before the name is "too common to trace".
const COMMON_WORD_LINES: u64 = 500;

/// A listing's items: `{total, items}` in the answer.
fn listing<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    items(&value[key], "items")
}

/// The class a gap of the answer belongs to; `None` for a gap that is a stop, a
/// withheld file or a route nothing traces, which have no class of their own.
fn class_of_gap(gap: &Value) -> Option<UnknownClass> {
    match text(gap, "kind")? {
        "dispatched" => Some(UnknownClass::Dispatched),
        "ambiguous" => Some(UnknownClass::Ambiguous),
        "overridable" => Some(UnknownClass::Overridable),
        "not_modelled" => Some(UnknownClass::NotModelled),
        "unlanded" if count(gap, "uses") > COMMON_WORD_LINES => Some(UnknownClass::CommonWord),
        "unlanded" => Some(UnknownClass::UnresolvedReads),
        _ => None,
    }
}

pub fn extract(doc: &Value) -> Result<Answer, String> {
    let mut answer = Answer {
        empty: doc.get("empty").and_then(Value::as_bool).unwrap_or(false),
        truncated: count(&doc["summary"], "shed"),
        tests_known: doc["tests"]
            .get("known")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        run_disclosure: !items(&doc["coverage"], "limits").is_empty()
            || count(&doc["coverage"]["unanalysed"], "total") > 0
            || count(&doc["inventory"], "withheld") > 0
            || doc.get("spelled").is_some_and(|v| !v.is_null()),
        ..Answer::default()
    };

    for definition_doc in items(doc, "definitions") {
        let definition = node_of(definition_doc);
        answer.definitions.push(definition.clone());
        answer.touched.push(definition.clone());
        if text(&definition_doc["change"], "kind") == Some("removed") {
            answer.removed.push(definition.clone());
        }
        if definition_doc.get("guard").is_some_and(|g| !g.is_null()) {
            answer.untested.push(definition.clone());
        }
        if let Some(entry) = text(definition_doc, "entry") {
            if entry.contains("reachable from tests:") {
                answer.entry_tested.push(definition.clone());
            } else {
                answer.entry_not_tested.push(definition.clone());
            }
        }
        answer.counted_not_listed += items(definition_doc, "folded")
            .iter()
            .filter(|fold| text(fold, "rule") != Some("untyped-receiver"))
            .map(|fold| count(&fold["count"], "n"))
            .sum::<u64>();
        for row in items(definition_doc, "rows") {
            let standing = text(row, "standing").unwrap_or("");
            let class = text(row, "class").unwrap_or("call");
            // A re-export is a row of its own class, counted apart in the header.
            let evidence = if class == "reexports" {
                "reexport"
            } else {
                standing
            };
            answer.caller_rows.push((
                text(row, "path").unwrap_or("").to_string(),
                evidence.to_string(),
            ));
            // A word match is the textual section of the answer, not a caller.
            if standing == "name" {
                continue;
            }
            let from = node_of(row);
            let stated = stated_kind(row)?;
            answer.relations.push(Relation {
                from: from.clone(),
                to: definition.clone(),
                kind: stated.unwrap_or_else(|| kind_of(class)),
                resolution: resolution_of(evidence),
                sites: lines_of(row),
                loose_kind: false,
                kind_stated: stated.is_some(),
            });
            if text(&row["guard"], "state") == Some("linked") {
                // The first test the guard names, then the others it lists.
                for test in
                    std::iter::once(&row["guard"]).chain(items(&row["guard"], "also").iter())
                {
                    if let Some(file) = text(test, "test") {
                        answer.relations.push(Relation {
                            from: Node::new(file, text(test, "name")),
                            to: from.clone(),
                            kind: Kind::TestsReach,
                            resolution: Resolution::Resolved,
                            sites: Vec::new(),
                            loose_kind: false,
                            kind_stated: false,
                        });
                    }
                }
            }
        }
        for gap in items(definition_doc, "gaps") {
            answer.gapped.push(definition.clone());
            // Files that spell the name but were not read are a claim about those
            // files (below), not an unknown of the definition.
            if text(gap, "kind") == Some("unaccounted") {
                continue;
            }
            answer
                .unknowns
                .push((definition.clone(), class_of_gap(gap), gap.to_string()));
        }
        // A file that spells the name but was not fully analysed is named only
        // textually: a claim about that file, and a gap in the answer.
        let missed = items(definition_doc, "missed_in");
        for file in missed {
            answer.relations.push(Relation {
                from: Node::new(text(file, "path").unwrap_or(""), None),
                to: definition.clone(),
                kind: Kind::Calls,
                resolution: Resolution::Unresolved,
                sites: lines_in(file),
                loose_kind: false,
                kind_stated: false,
            });
        }
        if !missed.is_empty() {
            answer.gapped.push(definition);
        }
    }
    // What the cap shed whole is a gap the answer discloses, never a caller list.
    if doc.get("omitted").is_some_and(|o| !o.is_null()) {
        let shed = Node::new("(changed definitions)", None);
        answer.unknowns.push((
            shed.clone(),
            None,
            "changed definitions omitted: the answer cap".to_string(),
        ));
        answer.gapped.push(shed);
    }

    let unlisted = &doc["unlisted"];
    for key in ["no_caller", "called_in_diff"] {
        for def in listing(unlisted, key) {
            let node = node_of(def);
            answer.touched.push(node.clone());
            answer.untested.push(node);
        }
    }
    for def in listing(unlisted, "entered_by") {
        let node = node_of(&def["def"]);
        if text(def, "role").is_some_and(|role| role.contains("reachable from tests:")) {
            answer.entry_tested.push(node.clone());
        } else {
            answer.entry_not_tested.push(node.clone());
        }
        answer.touched.push(node);
    }
    for def in listing(unlisted, "layout_only") {
        answer.touched.push(node_of(def));
    }

    for reference in listing(doc, "stale") {
        if let Some(target) = text(reference, "target") {
            answer
                .relations
                .push(path_relation(reference, target, Resolution::Resolved));
            answer
                .stale
                .push(text(reference, "path").unwrap_or("").to_string());
            answer.mention_classes.push((
                text(reference, "path").unwrap_or("").to_string(),
                text(reference, "class").unwrap_or("").to_string(),
            ));
        }
    }
    for (rows, resolution) in [
        (listing(&doc["named"], "by_path"), Resolution::Resolved),
        (listing(&doc["named"], "by_key"), Resolution::Candidate),
    ] {
        for reference in rows {
            if let Some(target) = text(reference, "target") {
                answer
                    .relations
                    .push(path_relation(reference, target, resolution));
                answer
                    .named_by
                    .push(text(reference, "path").unwrap_or("").to_string());
                answer.mention_classes.push((
                    text(reference, "path").unwrap_or("").to_string(),
                    text(reference, "class").unwrap_or("").to_string(),
                ));
            }
        }
    }
    // A name no file defines, spelled in files: the rows are candidates for
    // the queried name itself, which has no path.
    if let (Some(symbol), Some(spelled)) = (text(doc, "symbol"), doc.get("spelled")) {
        for row in items(spelled, "rows") {
            answer.relations.push(Relation {
                from: Node::new(text(row, "path").unwrap_or(""), None),
                to: Node::new(symbol, None),
                kind: Kind::Calls,
                resolution: Resolution::Candidate,
                sites: u32::try_from(count(row, "line")).into_iter().collect(),
                loose_kind: false,
                kind_stated: false,
            });
        }
    }
    answer.contracts = listing(doc, "contracts").iter().map(node_of).collect();
    // A removed public API is a removal, whether or not anything refers to it.
    for contract in listing(doc, "contracts") {
        if text(contract, "kind") == Some("removed_api") {
            let node = node_of(contract);
            answer.removed.push(node.clone());
            answer.touched.push(node);
        }
    }
    // `getCollection('blog')` reads every entry under `content/blog/`.
    let changed_files: Vec<&str> = items(&doc["inventory"]["changed"], "items")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for read in listing(&doc["named"], "collections") {
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
                kind_stated: false,
            });
        }
    }
    Ok(answer)
}
