//! `git diff` and the untracked listing, asked the one safe way. Every
//! argument that reaches git's argv from a caller (a range, a pathspec) passes
//! its gate here, and the flags that pin the output's shape against a user's or
//! a repository's own configuration are not the caller's to forget.

use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;

use crate::snapshot::error::{Result, SnapshotError};
use crate::snapshot::repo::reject_option_like;
use crate::snapshot::run::{run_git, run_git_z};
use crate::snapshot::types::contains_lexically;

// The prefix and color flags are not cosmetic: the diff parser keys off the
// literal `--- a/` / `+++ b/` headers, so a user's `diff.noprefix`,
// `diff.mnemonicPrefix`, `diff.srcPrefix`/`dstPrefix` or `color.ui=always`
// silently reduced every run to zero fragments and an empty `changed_files`.
// `diff.interHunkContext` merges hunks across untouched lines, so functions
// between two edits were emitted as changed; pinned to zero for the same
// reason `--unified=0` is passed explicitly.
const SAFE_DIFF_FLAGS: &[&str] = &[
    "--no-textconv",
    "--no-ext-diff",
    "--no-color",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--inter-hunk-context=0",
];

// Neither side of a range may begin with `-`: a leading dash would be parsed
// by git as an option rather than a revision, so a caller-supplied range like
// `--ext-diff` or `--textconv` would re-enable the very filters SAFE_DIFF_FLAGS
// disables and run repo-configured commands. Refs that begin with a dash are
// unaddressable on a git command line anyway, so nothing legitimate is lost.
// `#`, `+`, `%`, `,` and `=` are legal in a ref name (`v1.2.3+build.7`,
// `release,2026`); the gate is the leading dash and the separators, not the
// alphabet.
static SAFE_RANGE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^[a-zA-Z0-9_.^~/@{}#+%,=][a-zA-Z0-9_.^~/@{}#+%,=\-]*(\.\.\.?([a-zA-Z0-9_.^~/@{}#+%,=][a-zA-Z0-9_.^~/@{}#+%,=\-]*)?)?$",
    )
    .expect("the range gate is valid")
});

/// Repo-relative pathspecs that narrow every git query of a run. Validated
/// once: a leading `-` would be an option, a leading `:` pathspec magic, and
/// anything escaping the root is not a path in this repository.
pub fn validate_pathspec(paths: &[String]) -> Result<()> {
    for p in paths {
        if p.is_empty()
            || p.starts_with('-')
            || p.starts_with(':')
            || p.chars().any(|c| c.is_control())
            || !contains_lexically(Path::new(p))
        {
            return Err(SnapshotError::InvalidPath(p.clone()));
        }
    }
    Ok(())
}

fn validate_diff_range(diff_range: &str) -> Result<()> {
    let trimmed = diff_range.trim();
    // Reject leading-dot ranges like `..origin/main` (audit X16): the regex
    // character class allows `.`, so a string of only dots/identifier-chars
    // passes as a "ref" before being rejected by git itself with a less
    // informative `fatal: ambiguous argument`. Surface the dedicated error.
    if trimmed.starts_with('.') || trimmed.starts_with('/') {
        return Err(SnapshotError::InvalidRange(diff_range.to_string()));
    }
    if !SAFE_RANGE_RE.is_match(trimmed) {
        return Err(SnapshotError::InvalidRange(diff_range.to_string()));
    }
    // The regex alone cannot enforce this: its leading character class is
    // greedy over `.`, so it swallows the separator and never enters the
    // second-side group, and `a..--ext-diff` matched. Split and check each
    // side, otherwise the documented "neither side may begin with a dash" gate
    // is dead code and only the later per-rev check stands between a crafted
    // range and an argv option.
    let separator = if trimmed.contains("...") { "..." } else { ".." };
    for side in trimmed.split(separator) {
        if !side.is_empty() {
            reject_option_like(side)
                .map_err(|_| SnapshotError::InvalidRange(diff_range.to_string()))?;
        }
    }
    Ok(())
}

// The argv for every `git diff` this crate runs. Both controls it carries are
// security-relevant (SAFE_DIFF_FLAGS pins the parser against hostile
// repo-local config, `validate_diff_range` is the argv-injection gate whose own
// comment records a bypass that already shipped).
fn diff_args<'a>(
    extra: &[&'a str],
    diff_range: Option<&'a str>,
    pathspec: &'a [String],
) -> Result<Vec<&'a str>> {
    let mut args: Vec<&str> = vec!["diff"];
    args.extend_from_slice(SAFE_DIFF_FLAGS);
    args.extend_from_slice(extra);
    if let Some(range) = diff_range {
        validate_diff_range(range)?;
        args.push(range);
    }
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    Ok(args)
}

/// `git diff <extra> [range] [-- pathspec]` as text. `range` is a revision,
/// `A..B`, or `None` for the working tree against the index.
pub fn diff_text(
    root: &Path,
    extra: &[&str],
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<String> {
    let args = diff_args(extra, diff_range, pathspec)?;
    run_git(root, &args)
}

/// The untracked, unignored files, as git names them (repo-relative, `/`
/// separated, a nested repository as one `dir/` entry); the caller decides
/// which of them are files of this repository.
pub fn untracked_names(root: &Path, pathspec: &[String]) -> Result<Vec<String>> {
    let mut args: Vec<&str> = vec!["ls-files", "--others", "--exclude-standard", "-z"];
    if !pathspec.is_empty() {
        validate_pathspec(pathspec)?;
        args.push("--");
        args.extend(pathspec.iter().map(String::as_str));
    }
    run_git_z(root, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_diff_range_rejects_option_smuggled_in_range() {
        for hostile in ["HEAD..--ext-diff", "a...-p", "..--upload-pack=x"] {
            assert!(
                validate_diff_range(hostile).is_err(),
                "expected {hostile:?} to be rejected"
            );
        }
    }

    #[test]
    fn validate_diff_range_accepts_legitimate_ranges() {
        for legit in [
            "HEAD~1..HEAD",
            "@{-1}..HEAD",
            "HEAD~2...origin/main",
            "main..feature/x",
            "v1.2.3+build.7",
            "release,2026..rc=1",
            "issue#42...fix%20",
        ] {
            assert!(
                validate_diff_range(legit).is_ok(),
                "expected {legit:?} to be accepted"
            );
        }
    }
}
