//! A question about a repository names its range once; every door resolves it
//! here, to object ids, and never re-reads the string. `A..B` and `A...B` are
//! what `B` changed since it forked from `A` (#485, #462), the push and
//! force-push deltas are values the caller states, and a single snapshot
//! (`--symbol`) is `At`.

use crate::error::{Result, SnapshotError};
use crate::git::{Exec, git_command};
use crate::reader::Side;
use crate::repo::{RepoRoot, empty_tree_oid, merge_base_with, resolve_commit, resolve_object};
use crate::types::Oid;

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
    let out = exec.finish(cmd, work, &["rev-list"], &[])?;
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
