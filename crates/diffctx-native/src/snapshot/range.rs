//! A question about a repository names its range once; every door resolves it
//! here, to object ids, and never re-reads the string. `A..B` and `A...B` are
//! what `B` changed since it forked from `A` (#485, #462), the push and
//! force-push deltas are values the caller states, and a single snapshot
//! (`--symbol`) is `At`.

use std::path::Path;

use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::git::{Exec, git_command};
use crate::snapshot::reader::Side;
use crate::snapshot::repo::{
    RepoRoot, empty_tree_id, empty_tree_oid, merge_base_with, reject_option_like, resolve_commit,
    resolve_object, rev_oid,
};
use crate::snapshot::types::Oid;

use once_cell::sync::Lazy;
use regex::Regex;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RangeSpec {
    /// `A..B` / `A...B`: `base` is `merge-base(A, B)`, resolved once, so a base
    /// that moved on after the fork is not read as reverts. `head` is a
    /// commit or a tree.
    Review { base: Oid, head: Oid },
    /// A push delta: `old` is an ancestor of `new`.
    Pushed { old: Oid, new: Oid },
    /// A force-push: the tree delta from the value the ref held, labelled
    /// "ref rewritten" by the caller. No ancestry between the two.
    Rewritten { old: Oid, new: Oid },
    /// `base` against the index.
    Staged { base: Oid },
    /// `base` against the working tree and its admitted untracked files.
    WorkingTree { base: Oid },
    /// One snapshot, no change.
    At(Side),
}

/// The one string parser.
///
/// Accepts `staged` / `--cached`, `A..B`, `A...B`,
/// `A..` (`A..HEAD`), a duration (`24h`, `8d`, `1h30m`: the last commit before
/// the window against the working tree) and a single revision (against the
/// working tree).
pub fn resolve(root: &RepoRoot, spec: &str) -> Result<RangeSpec> {
    let exec = Exec::ambient();
    let work = &root.work_tree;
    let spec = spec.trim();
    if matches!(spec, "staged" | "--cached") {
        return staged(root);
    }
    if spec.starts_with('.') || spec.starts_with('/') {
        return Err(SnapshotError::InvalidRange(spec.to_string()));
    }
    if let Some((base, head)) = split_range(spec) {
        return match (
            resolve_commit(&exec, work, base)?,
            resolve_commit(&exec, work, head)?,
        ) {
            (Some(base), Some(head)) => {
                let fork = merge_base_with(&exec, work, &base, &head)?;
                Ok(RangeSpec::Review {
                    base: fork.unwrap_or(base),
                    head,
                })
            }
            // Not two commits (a tree on the right is a snapshot): as written.
            _ => Ok(RangeSpec::Review {
                base: resolve_object(&exec, work, base)?,
                head: resolve_object(&exec, work, head)?,
            }),
        };
    }
    let parsed = parse_duration_seconds(spec);
    if parsed.is_none() && !looks_like_duration(spec) {
        return Ok(RangeSpec::WorkingTree {
            base: resolve_object(&exec, work, spec)?,
        });
    }
    // A ref that happens to look like a duration (a branch `24h`, an
    // abbreviated sha `24d`) keeps its git meaning.
    if let Some(base) = resolve_commit(&exec, work, spec)? {
        return Ok(RangeSpec::WorkingTree { base });
    }
    let Some(seconds) = parsed else {
        return Err(SnapshotError::InvalidDuration(spec.to_string()));
    };
    let base = match resolve_commit(&exec, work, "HEAD")? {
        None => empty_tree_oid(&exec, work)?,
        Some(_) => {
            let before = format!("--before={seconds} seconds ago");
            let out = exec.text(work, &["rev-list", "-1", &before, "HEAD", "--"])?;
            // Nothing before the window: every file is new within it.
            match out.trim() {
                "" => empty_tree_oid(&exec, work)?,
                oid => Oid::parse(oid)?,
            }
        }
    };
    Ok(RangeSpec::WorkingTree { base })
}

/// `HEAD` against the index; the empty tree on an unborn branch.
pub fn staged(root: &RepoRoot) -> Result<RangeSpec> {
    Ok(RangeSpec::Staged {
        base: head_or_empty(&Exec::ambient(), root)?,
    })
}

/// `HEAD` against the working tree; the empty tree on an unborn branch.
pub fn working_tree(root: &RepoRoot) -> Result<RangeSpec> {
    Ok(RangeSpec::WorkingTree {
        base: head_or_empty(&Exec::ambient(), root)?,
    })
}

fn head_or_empty(exec: &Exec, root: &RepoRoot) -> Result<Oid> {
    match resolve_commit(exec, &root.work_tree, "HEAD")? {
        Some(head) => Ok(head),
        None => empty_tree_oid(exec, &root.work_tree),
    }
}

/// `(base, head)` of a two-sided range. An open right side (`A..`, `A...`) is
/// `A..HEAD` in git's own reading: both sides committed.
fn split_range(range: &str) -> Option<(&str, &str)> {
    if range.chars().any(char::is_whitespace) {
        return None;
    }
    // The base is at least one character: a leading `..` is no range.
    let at = range.get(1..)?.find("..")? + 1;
    let sep = if range[at..].starts_with("...") { 3 } else { 2 };
    let head = &range[at + sep..];
    Some((&range[..at], if head.is_empty() { "HEAD" } else { head }))
}

/// The commits a range adds, newest first. Empty where the range has no
/// history of its own: a snapshot, the index, a tree on the right.
pub fn commits(root: &RepoRoot, range: &RangeSpec) -> Result<Vec<Oid>> {
    let exec = Exec::ambient();
    let work = &root.work_tree;
    let (from, to) = match range {
        RangeSpec::Review { base, head } => (base.clone(), head.clone()),
        RangeSpec::Pushed { old, new } | RangeSpec::Rewritten { old, new } => {
            if old.is_zero() {
                return Err(SnapshotError::Range(
                    "a created ref has no old value to count commits from".into(),
                ));
            }
            (old.clone(), new.clone())
        }
        RangeSpec::WorkingTree { base } => match resolve_commit(&exec, work, "HEAD")? {
            Some(head) => (base.clone(), head),
            None => return Ok(Vec::new()),
        },
        RangeSpec::Staged { .. } | RangeSpec::At(_) => return Ok(Vec::new()),
    };
    let is_commit = |oid: &Oid| resolve_commit(&exec, work, oid.as_str()).map(|c| c.is_some());
    if !is_commit(&from)? || !is_commit(&to)? || from == to {
        return Ok(Vec::new());
    }
    let span = format!("{from}..{to}");
    let mut cmd = git_command(work);
    cmd.args(["rev-list", &span, "--"]);
    let out = exec.finish(cmd, &["rev-list"], &[])?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(Oid::parse)
        .collect()
}

const UNITS: &[(&str, u64)] = &[
    ("weeks", 7 * 24 * 3600),
    ("week", 7 * 24 * 3600),
    ("w", 7 * 24 * 3600),
    ("days", 24 * 3600),
    ("day", 24 * 3600),
    ("d", 24 * 3600),
    ("hours", 3600),
    ("hour", 3600),
    ("hrs", 3600),
    ("hr", 3600),
    ("h", 3600),
    ("minutes", 60),
    ("minute", 60),
    ("mins", 60),
    ("min", 60),
    ("m", 60),
    ("seconds", 1),
    ("second", 1),
    ("secs", 1),
    ("sec", 1),
    ("s", 1),
];

fn strip_unit<'a>(text: &'a str, unit: &str) -> Option<&'a str> {
    let head = text.get(..unit.len())?;
    head.eq_ignore_ascii_case(unit).then(|| &text[unit.len()..])
}

/// `24h` / `8d` / `1h30m`: whole `<number><unit>` components, nothing left
/// over. Anything left over makes the whole string a revision again, so `8dd`
/// still reaches git as a ref.
fn parse_duration_seconds(spec: &str) -> Option<u64> {
    let mut rest = spec.trim();
    if rest.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    while !rest.is_empty() {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || digits > 9 {
            return None;
        }
        let Ok(count) = rest[..digits].parse::<u64>() else {
            return None;
        };
        let after = rest[digits..].trim_start();
        let (seconds, tail) = UNITS
            .iter()
            .find_map(|(name, seconds)| strip_unit(after, name).map(|tail| (*seconds, tail)))?;
        total = total.checked_add(count.checked_mul(seconds)?)?;
        rest = tail.trim_start();
    }
    Some(total)
}

/// Every component is `<number>[.fraction]<known unit>`. A spec of this shape
/// that `parse_duration_seconds` still rejects (a fraction, a count too large)
/// is a typo of the duration grammar, and "unknown git revision '1.5h'" would
/// send the user to `git log` for a problem it cannot show.
fn looks_like_duration(spec: &str) -> bool {
    shape(spec.trim())
}

fn shape(text: &str) -> bool {
    let int = text.bytes().take_while(u8::is_ascii_digit).count();
    if int == 0 {
        return false;
    }
    let mut rest = &text[int..];
    if let Some(frac) = rest.strip_prefix('.') {
        let n = frac.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return false;
        }
        rest = &frac[n..];
    }
    let rest = rest.trim_start();
    UNITS.iter().any(|(name, _)| {
        strip_unit(rest, name).is_some_and(|tail| {
            let tail = tail.trim_start();
            tail.is_empty() || shape(tail)
        })
    })
}

static RANGE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\s*(\S+?)(\.\.\.?)(\S*?)\s*$").expect("the range pattern is valid"));

/// `(base, head)` of a two-sided range as the engine's text readers spell it;
/// `(None, None)` for a single revision, which git compares with the working
/// tree. An open right side (`A..`, `A...`) is `A..HEAD` in git's own reading:
/// both sides committed, so the head is HEAD, not the working tree.
pub fn split_diff_range(range: &str) -> (Option<String>, Option<String>) {
    match RANGE_RE.captures(range) {
        None => (None, None),
        Some(caps) => {
            let base = caps
                .get(1)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty());
            let head = caps
                .get(3)
                .map(|m| m.as_str().trim().to_string())
                .filter(|s| !s.is_empty())
                .or_else(|| Some("HEAD".to_string()));
            (base, head)
        }
    }
}

/// The public names for the staged index: `staged`, and git's own `--cached`,
/// accepted literally and nowhere else as an option.
pub fn is_staged_keyword(spec: &str) -> bool {
    matches!(spec.trim(), "staged" | "--cached")
}

/// The last commit before a `--diff 24h` window, the empty tree when the
/// repository is younger: `None` when `spec` is no window (a revision, a range,
/// the staged keyword). A ref that happens to look like a duration (a branch
/// `24h`, an abbreviated sha `24d`) keeps its git meaning. Only the phrasing of
/// messages and the legacy spelling of the analysed range need it; the range
/// itself is resolved once by [`resolve`].
pub fn duration_base(root: &Path, spec: &str) -> Result<Option<String>> {
    let trimmed = spec.trim();
    if is_staged_keyword(trimmed) || split_diff_range(trimmed).0.is_some() {
        return Ok(None);
    }
    let parsed = parse_duration_seconds(trimmed);
    if parsed.is_none() && !looks_like_duration(trimmed) {
        return Ok(None);
    }
    if reject_option_like(trimmed).is_ok() && rev_oid(root, trimmed)?.is_some() {
        return Ok(None);
    }
    let Some(seconds) = parsed else {
        return Err(SnapshotError::InvalidDuration(trimmed.to_string()));
    };
    // A repository with no commit has none before the window: the empty tree.
    // Any other failure of the read is the caller's to see.
    let base = if rev_oid(root, "HEAD")?.is_some() {
        // git owns the calendar arithmetic (local timezone, DST) via approxidate.
        let before = format!("--before={seconds} seconds ago");
        Exec::current()
            .text(root, &["rev-list", "-1", &before, "HEAD", "--"])?
            .trim()
            .to_string()
    } else {
        String::new()
    };
    Ok(Some(if base.is_empty() {
        empty_tree_id(root)?
    } else {
        base
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::testing::{commit_all, git, init_git_repo, write_file};
    use std::fs;
    use tempfile::TempDir;

    /// One `--name-status` listing answers what the three listings it
    /// replaced answered, by git's own word on each: renames, deletions and

    #[test]
    fn duration_specs_cover_the_standard_units_and_compose() {
        for (spec, expected) in [
            ("5s", 5),
            ("90 sec", 90),
            ("10min", 600),
            ("45m", 2700),
            ("24h", 86_400),
            ("3hrs", 10_800),
            ("8d", 691_200),
            ("2 weeks", 1_209_600),
            ("1h30m", 5400),
            ("1D", 86_400),
        ] {
            assert_eq!(
                parse_duration_seconds(spec),
                Some(expected),
                "spec {spec:?}"
            );
        }
    }

    #[test]
    fn anything_that_is_not_wholly_a_duration_stays_a_revision() {
        for spec in [
            "HEAD",
            "HEAD~1..HEAD",
            "main",
            "8dd",
            "24",
            "h",
            "v1.2",
            "",
            "1h-",
            "deadbeef",
        ] {
            assert_eq!(parse_duration_seconds(spec), None, "spec {spec:?}");
        }
    }

    #[test]
    fn a_duration_resolves_to_the_last_commit_before_the_window() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "old.txt", "old\n");
        // `--before` filters on the committer date, so backdating the author
        // date alone would leave this commit inside the window.
        git(root, &["add", "-A"]);
        let status = git_command(root)
            .args(["commit", "-q", "-m", "old"])
            .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00+00:00")
            .status()
            .expect("commit");
        assert!(status.success());
        let old_head = crate::snapshot::run_git(root, &["rev-parse", "HEAD"])
            .expect("rev-parse")
            .trim()
            .to_string();
        write_file(root, "new.txt", "new\n");
        commit_all(root, "new");

        let base = duration_base(root, "24h").expect("resolve");
        assert_eq!(base.as_deref(), Some(old_head.as_str()));

        let diff =
            crate::snapshot::diff_text(root, &["--unified=3"], base.as_deref(), &[]).expect("diff");
        assert!(diff.contains("new.txt"), "window must cover the new commit");
        assert!(
            !diff.contains("old.txt"),
            "window must exclude the commit before it"
        );
    }

    #[test]
    fn a_window_older_than_the_repo_falls_back_to_the_empty_tree() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "only.txt", "only\n");
        commit_all(root, "only");

        let base = duration_base(root, "1w").expect("resolve");
        assert!(base.is_some());
        let diff =
            crate::snapshot::diff_text(root, &["--unified=3"], base.as_deref(), &[]).expect("diff");
        assert!(
            diff.contains("only.txt"),
            "a repo younger than the window is entirely new within it"
        );
    }

    #[test]
    fn a_ref_that_looks_like_a_duration_keeps_its_git_meaning() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "a.txt", "a\n");
        commit_all(root, "a");
        git(root, &["branch", "24h"]);

        assert_eq!(duration_base(root, "24h").expect("resolve"), None);
    }
}
