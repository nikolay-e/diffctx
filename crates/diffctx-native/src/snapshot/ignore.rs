//! Which paths the ignore rules exclude, by git's own engine so nesting,
//! negation and `**` are right, plus `.diffctx/ignore` fed to the same engine
//! as a lowest-precedence excludes file. Ported from the facade's
//! `find_ignored_paths_inner`, with two changes: a failure is an `Err` (the
//! old code fell back to "nothing ignored" or "everything ignored"), and a
//! tree is judged by the rules *in that tree*, evaluated in a scratch
//! repository that holds only its `.gitignore` files, never by whatever the
//! working tree holds today.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::snapshot::admission::IgnoreSource;
use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::git::{Exec, Scratch, git_command};
use crate::snapshot::types::RepoPath;

pub(crate) struct Hit {
    pub source: IgnoreSource,
    /// The same rule excludes an ancestor directory: the path is caught only
    /// transitively.
    pub inherited: bool,
}

pub(crate) enum Basis<'a> {
    /// The rules git finds in the checkout at `root`.
    WorkingTree { root: &'a Path },
    /// The `.gitignore` and `.diffctx/ignore` files a tree holds, by path.
    Tree { files: &'a [(RepoPath, Vec<u8>)] },
}

const DIFFCTX_IGNORE: &str = ".diffctx/ignore";

pub(crate) fn is_rule_file(path: &RepoPath) -> bool {
    path.file_name() == ".gitignore"
        || path.as_str() == DIFFCTX_IGNORE
        || path.as_str().ends_with("/.diffctx/ignore")
}

/// Rewrites one `.diffctx/ignore` pattern line to be anchored to the
/// directory that contains the `.diffctx/` folder (`rel`, repo-root-relative,
/// "" for the repo root itself), so a pattern declared in `sub/.diffctx/ignore`
/// only ever matches within `sub/`. Held to
/// `tests/fixtures/ignore_anchor_cases.json` with the facade's copy.
pub(crate) fn anchor_line(line: &str, rel: &str) -> String {
    let (neg, pat) = match line.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let pat_no_trailing_slash = pat.trim_end_matches('/');
    let full = if pat_no_trailing_slash.starts_with('/') || pat_no_trailing_slash.contains('/') {
        let anchored = pat.trim_start_matches('/');
        if rel.is_empty() {
            format!("/{anchored}")
        } else {
            format!("/{rel}/{anchored}")
        }
    } else if rel.is_empty() {
        pat.to_string()
    } else {
        format!("{rel}/**/{pat}")
    };
    if neg { format!("!{full}") } else { full }
}

fn patterns_of(rule_file: &str, content: &str) -> Vec<String> {
    let rel_dir = rule_file
        .strip_suffix(DIFFCTX_IGNORE)
        .unwrap_or("")
        .trim_end_matches('/');
    content
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| anchor_line(line, rel_dir))
        .collect()
}

pub(crate) fn check(
    exec: &Exec,
    basis: &Basis<'_>,
    paths: &[&str],
) -> Result<FxHashMap<String, Hit>> {
    if paths.is_empty() {
        return Ok(FxHashMap::default());
    }
    let scratch = Scratch::new()?;
    let (dir, patterns, own_excludes) = match basis {
        Basis::WorkingTree { root } => (
            root.to_path_buf(),
            working_tree_patterns(exec, root)?,
            false,
        ),
        Basis::Tree { files } => {
            let work = scratch.path().join("work");
            let init = {
                let mut cmd = git_command(scratch.path());
                cmd.args(["init", "-q", "--template=", "work"]);
                cmd
            };
            exec.finish(init, scratch.path(), &["init"], &[])?;
            let mut patterns = Vec::new();
            for (path, bytes) in *files {
                if path.file_name() == ".gitignore" {
                    let at = path.under(&work);
                    if let Some(parent) = at.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(at, bytes)?;
                } else {
                    let text = std::str::from_utf8(bytes).map_err(|_| {
                        SnapshotError::Git(format!("cannot read {path}: not UTF-8"))
                    })?;
                    patterns.extend(patterns_of(path.as_str(), text));
                }
            }
            // The scratch checkout has the directories the queried paths name,
            // empty: a rule that matches only directories (`build/`) is asked
            // about `build`, and git tells a directory from a file by looking.
            let mut made = FxHashSet::default();
            for path in paths {
                if let Some((parent, _)) = path.rsplit_once('/') {
                    if made.insert(parent) {
                        std::fs::create_dir_all(RepoPath::new(parent)?.under(&work))?;
                    }
                }
            }
            (work, patterns, true)
        }
    };
    run(exec, &dir, &scratch, &patterns, own_excludes, paths)
}

/// The `.diffctx/ignore` files of a checkout, tracked or present, as
/// anchored patterns. `Err` when the declared policy could not be read in
/// full: "no policy" is the one answer that publishes what was declared
/// private.
fn working_tree_patterns(exec: &Exec, root: &Path) -> Result<Vec<String>> {
    let files = exec.records(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
            "--",
            ":(glob)**/.diffctx/ignore",
        ],
    )?;
    let mut patterns = Vec::new();
    for raw in files {
        let rel = String::from_utf8_lossy(&raw).into_owned();
        if rel != DIFFCTX_IGNORE && !rel.ends_with("/.diffctx/ignore") {
            continue;
        }
        let content = std::fs::read_to_string(RepoPath::new(&rel)?.under(root))
            .map_err(|e| SnapshotError::Git(format!("cannot read {rel}: {e}")))?;
        patterns.extend(patterns_of(&rel, &content));
    }
    Ok(patterns)
}

fn run(
    exec: &Exec,
    dir: &Path,
    scratch: &Scratch,
    patterns: &[String],
    own_excludes: bool,
    paths: &[&str],
) -> Result<FxHashMap<String, Hit>> {
    let excludes = scratch.path().join("excludes");
    let set_excludes = own_excludes || !patterns.is_empty();
    if set_excludes {
        std::fs::write(&excludes, patterns.join("\n"))?;
    }

    // Ancestors are queried alongside the paths themselves so an exclusion can
    // be attributed: the same winning rule on a parent directory means the
    // file was only caught transitively.
    let mut queries: Vec<String> = paths.iter().map(|p| (*p).to_string()).collect();
    let mut seen_ancestors: FxHashSet<String> = FxHashSet::default();
    for rel in paths {
        for ancestor in ancestor_dirs(rel) {
            if seen_ancestors.insert(ancestor.clone()) {
                queries.push(ancestor);
            }
        }
    }
    // Paths reach `--stdin` through a file, NUL-separated: a pipe written
    // before stdout is drained deadlocks once the payload outgrows its buffer,
    // and line-delimited input splits a path holding a newline into two
    // phantom queries.
    let query_file = scratch.path().join("queries");
    std::fs::write(&query_file, format!("{}\0", queries.join("\0")))?;

    let mut cmd = git_command(dir);
    if set_excludes {
        cmd.arg("-c")
            .arg(format!("core.excludesFile={}", excludes.display()));
    }
    cmd.args(["check-ignore", "--no-index", "-v", "-z", "--stdin"])
        .stdin(std::fs::File::open(&query_file)?);
    // Exit 1 means "none of the given paths are ignored": an answer.
    let output = exec.finish(cmd, dir, &["check-ignore"], &[1])?;

    let rules = parse_verbose_records(&String::from_utf8_lossy(&output.stdout));
    let excludes_source = set_excludes.then(|| format!("{}:", excludes.display()));
    Ok(paths
        .iter()
        .filter_map(|rel| {
            let rule = rules.get(*rel)?;
            let hit = if excludes_source
                .as_deref()
                .is_some_and(|src| rule.starts_with(src))
            {
                Hit {
                    source: IgnoreSource::DiffctxPolicy,
                    inherited: false,
                }
            } else {
                Hit {
                    source: IgnoreSource::Gitignore,
                    inherited: ancestor_dirs(rel)
                        .iter()
                        .any(|dir| rules.get(dir) == Some(rule)),
                }
            };
            Some(((*rel).to_string(), hit))
        })
        .collect())
}

/// `check-ignore -v -z` emits four NUL-separated fields per match:
/// `<source>\0<line>\0<pattern>\0<path>\0`. The path is never quoted, because
/// NUL cannot appear in one. A record whose pattern is a negation means the
/// path is explicitly NOT ignored (#193).
fn parse_verbose_records(stdout: &str) -> FxHashMap<String, String> {
    let mut rules = FxHashMap::default();
    let fields: Vec<&str> = stdout.split('\0').collect();
    for record in fields.chunks(4) {
        if record.len() < 4 {
            break;
        }
        let (source, line, pattern, path) = (record[0], record[1], record[2], record[3]);
        if path.is_empty() || pattern.starts_with('!') {
            continue;
        }
        rules.insert(path.to_string(), format!("{source}:{line}:{pattern}"));
    }
    rules
}

fn ancestor_dirs(rel: &str) -> Vec<String> {
    let mut dirs = Vec::new();
    let mut remainder = rel;
    while let Some((parent, _)) = remainder.rsplit_once('/') {
        dirs.push(parent.to_string());
        remainder = parent;
    }
    dirs
}
