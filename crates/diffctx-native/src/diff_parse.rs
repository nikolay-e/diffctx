//! The legacy diff text, read into the hunks and names the pack pipeline
//! works from. The text comes from the snapshot layer (`diff_text`,
//! `untracked`), which owns the flags and the gates; this module only
//! parses it, and refuses a header that does not stay inside the repository.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashSet;

use crate::snapshot::{Result, diff_text, untracked};
use crate::types::DiffHunk;

static HUNK_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap());

/// The diff with the context lines that anchor a run and tell layout from
/// content; a `diff.context` in the user's config must not move them.
pub fn get_diff_text(
    repo_root: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<String> {
    diff_text(repo_root, &["--unified=3"], diff_range, pathspec)
}

pub(crate) fn unquote_c_style(quoted: &str) -> String {
    if !(quoted.starts_with('"') && quoted.ends_with('"')) {
        return quoted.to_string();
    }

    let raw = &quoted[1..quoted.len() - 1];
    let bytes = raw.as_bytes();
    let mut result: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let nxt = bytes[i + 1];
            match nxt {
                b't' => {
                    result.push(b'\t');
                    i += 2;
                }
                b'n' => {
                    result.push(b'\n');
                    i += 2;
                }
                b'r' => {
                    result.push(b'\r');
                    i += 2;
                }
                b'b' => {
                    result.push(0x08);
                    i += 2;
                }
                b'f' => {
                    result.push(0x0C);
                    i += 2;
                }
                b'v' => {
                    result.push(0x0B);
                    i += 2;
                }
                b'a' => {
                    result.push(0x07);
                    i += 2;
                }
                b'\\' => {
                    result.push(b'\\');
                    i += 2;
                }
                b'"' => {
                    result.push(b'"');
                    i += 2;
                }
                b'0'..=b'7'
                    if i + 3 < bytes.len()
                        && bytes[i + 2].is_ascii_digit()
                        && bytes[i + 2] <= b'7'
                        && bytes[i + 3].is_ascii_digit()
                        && bytes[i + 3] <= b'7' =>
                {
                    let val = (nxt - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
                    result.push(val);
                    i += 4;
                }
                _ => {
                    result.push(b'\\');
                    i += 1;
                }
            }
        } else {
            result.push(bytes[i]);
            i += 1;
        }
    }

    String::from_utf8(result).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Resolves a diff-header path against the repository root, or `None` if it
/// does not stay inside it.
///
/// `Path::starts_with` compares components, not locations, and
/// `canonicalize` fails for any path that does not exist — which is the
/// normal case for the old side of a deletion, and for a crafted header
/// naming a file that was never there. The previous guard fell back to the
/// lexically joined path in that case, and `<root>/../../escape.py` starts
/// with `<root>` component-wise, so the check passed and the header was
/// accepted. It only ever failed on macOS, where the temp root canonicalizes
/// through `/var -> /private/var` and the two spellings stop matching.
///
/// A `..` component is therefore rejected outright, before any of this:
/// git does not emit one for a tracked path, so nothing legitimate needs it,
/// and downstream `strip_prefix` guards are lexical for exactly the same
/// reason. Absolute and rooted paths are refused for the same reason —
/// `Path::join` with such an argument discards the root entirely.
pub(crate) fn resolve_in_repo(repo_root: &Path, rel_path: &str) -> Option<PathBuf> {
    let rel = Path::new(rel_path);
    // `has_root` too: on Windows `\etc\x` is rooted but not absolute, and
    // `Path::join` still replaces everything after the drive with it.
    if rel.has_root()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }

    let joined = crate::paths::repo_join(repo_root, rel_path);
    // Compare like for like. Falling back to the lexical spelling as an
    // *alternative* to the canonical check (rather than only when
    // canonicalization is impossible) would make the canonical check dead: with
    // `..` already excluded, `joined` always starts with `repo_root`, so an
    // in-repo symlink pointing outside the tree would resolve outside and still
    // be accepted.
    match dunce::canonicalize(&joined) {
        Ok(resolved) => {
            let resolved_root =
                dunce::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
            if !resolved.starts_with(&resolved_root) {
                return None;
            }
        }
        // Unresolvable: the old side of a deletion, or a path that never
        // existed. `..` and absolute components are rejected above, so the
        // lexical join cannot leave the root and needs no further check —
        // which is also why the canonical root's spelling (`/var` vs
        // `/private/var`) cannot cause a false rejection here.
        Err(_) => {}
    }
    Some(joined)
}

pub(crate) fn parse_path_line(line: &str, repo_root: &Path) -> (&'static str, Option<PathBuf>) {
    if line.starts_with("--- /dev/null") {
        return ("old", None);
    }
    if line.starts_with("+++ /dev/null") {
        return ("new", None);
    }

    let (kind, rel_path) = if let Some(rest) = line.strip_prefix("--- a/") {
        ("old", rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("+++ b/") {
        ("new", rest.trim().to_string())
    } else if let Some(rest) = line.strip_prefix("--- ").filter(|r| r.starts_with("\"a/")) {
        let unquoted = unquote_c_style(rest.trim());
        (
            "old",
            unquoted.strip_prefix("a/").unwrap_or(&unquoted).to_string(),
        )
    } else if let Some(rest) = line.strip_prefix("+++ ").filter(|r| r.starts_with("\"b/")) {
        let unquoted = unquote_c_style(rest.trim());
        (
            "new",
            unquoted.strip_prefix("b/").unwrap_or(&unquoted).to_string(),
        )
    } else {
        return ("", None);
    };

    match resolve_in_repo(repo_root, &rel_path) {
        Some(path) => (kind, Some(path)),
        None => ("", None),
    }
}

fn parse_hunk_header(caps: &regex::Captures, path: &Path) -> Option<DiffHunk> {
    // Adversarial-diff hardening: integers parsed from the hunk regex are
    // small ASCII digits matched by `\d+`, so `parse::<u32>` can only fail
    // on overflow (e.g. lines > 2^32). Skip such hunks instead of crashing
    // the host process — they are degenerate and have no useful semantics.
    let old_start: u32 = caps[1].parse().ok()?;
    let old_len: u32 = match caps.get(2) {
        Some(m) => m.as_str().parse().ok()?,
        None => 1,
    };
    let new_start: u32 = caps[3].parse().ok()?;
    let new_len: u32 = match caps.get(4) {
        Some(m) => m.as_str().parse().ok()?,
        None => 1,
    };

    Some(DiffHunk {
        path: Arc::from(path.to_string_lossy().as_ref()),
        new_start,
        new_len,
        old_start,
        old_len,
    })
}

pub fn parse_diff(
    repo_root: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<Vec<DiffHunk>> {
    let output = diff_text(repo_root, &["--unified=0", "-M"], diff_range, pathspec)?;
    Ok(parse_hunks_from_diff_output(&output, repo_root))
}

pub(crate) fn parse_hunks_from_diff_output(output: &str, repo_root: &Path) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut old_path: Option<PathBuf> = None;
    let mut new_path: Option<PathBuf> = None;

    for line in output.lines() {
        // `diff --git` is the per-file boundary git always emits, so clearing
        // here is what keeps one file's hunks from ever being charged to the
        // previous one. Relying on the `---`/`+++` pair alone leaves the
        // previous file's paths live whenever this file's header is not
        // understood or is rejected — a path escaping the repo root returns
        // `("", None)`, which is a refusal, not "keep the last path".
        if line.starts_with("diff --git ") {
            old_path = None;
            new_path = None;
            continue;
        }

        let (path_type, path) = parse_path_line(line, repo_root);
        match path_type {
            "old" => {
                old_path = path;
                continue;
            }
            "new" => {
                new_path = path;
                continue;
            }
            _ => {}
        }

        if let Some(caps) = HUNK_RE.captures(line) {
            let current_path = new_path.as_deref().or(old_path.as_deref());
            if let Some(p) = current_path {
                if let Some(hunk) = parse_hunk_header(&caps, p) {
                    hunks.push(hunk);
                }
            }
        }
    }

    hunks
}

/// The names a diff touches, read from one `--name-status -M` listing: every
/// changed path (a rename by its destination), the deletions, and the rename
/// pairs. They were three listings, two of them run twice per run.
#[derive(Default)]
pub struct DiffNames {
    pub changed: Vec<PathBuf>,
    pub deleted: FxHashSet<PathBuf>,
    renamed: Vec<(String, String)>,
}

impl DiffNames {
    /// Rename *source* paths, canonicalized. These no longer exist on disk and
    /// cannot be fragmented, so the pipeline excludes them from the changed set.
    /// The rename destinations need no special handling: they exist on HEAD and
    /// reach the universe through the ordinary changed-file path.
    pub fn renamed_sources(&self, repo_root: &Path) -> FxHashSet<PathBuf> {
        self.renamed
            .iter()
            .map(|(old, _)| {
                let joined = crate::paths::repo_join(repo_root, old);
                dunce::canonicalize(&joined).unwrap_or(joined)
            })
            .collect()
    }

    /// Rename pairs as repo-relative display paths (`old -> new`), for the
    /// output header: the pairing kept, nothing canonicalized (the old path no
    /// longer exists on disk).
    pub fn rename_pairs(&self) -> Vec<(String, String)> {
        self.renamed
            .iter()
            .map(|(old, new)| {
                (
                    crate::paths::to_posix_display(std::borrow::Cow::Borrowed(old)),
                    crate::paths::to_posix_display(std::borrow::Cow::Borrowed(new)),
                )
            })
            .collect()
    }
}

pub fn diff_names(
    repo_root: &Path,
    diff_range: Option<&str>,
    pathspec: &[String],
) -> Result<DiffNames> {
    let output = diff_text(
        repo_root,
        &["--name-status", "-M", "-z"],
        diff_range,
        pathspec,
    )?;
    let parts: Vec<&str> = output.split('\0').collect();
    // Lexical `root.join(p)`, never the canonical target: `canonicalize()` on a
    // symlink returns what it POINTS AT, so an untracked `evil -> /etc/shadow`
    // entered the changed-file list under an absolute out-of-root path. That
    // breaks three things at once — the path stops naming an object in the
    // repository, the ignore and secret policies are then applied to the wrong
    // name, and the working-tree line counter reads outside the checkout.
    //
    // Containment is checked lexically and NOT by canonicalising: these names
    // need not exist on disk. A deleted file is gone by definition and a bare
    // clone has no working tree, so an existence test empties both lists. The
    // symlink itself is caught where the file is actually read.
    let inside = |p: &str| {
        crate::paths::contains_lexically(Path::new(p))
            .then(|| crate::paths::repo_join(repo_root, p))
    };
    let mut names = DiffNames::default();
    let mut i = 0;
    while i < parts.len() {
        let status = parts[i];
        if status.is_empty() {
            i += 1;
            continue;
        }
        // `R<similarity>\0old\0new` and `C…` carry two paths, every other
        // status one; a rename without a destination is no rename.
        if status.starts_with(['R', 'C']) {
            let (old, new) = (
                parts.get(i + 1).copied().unwrap_or(""),
                parts.get(i + 2).copied().unwrap_or(""),
            );
            if !new.is_empty() {
                names.changed.extend(inside(new));
            }
            if status.starts_with('R') && !old.is_empty() && !new.is_empty() {
                names.renamed.push((old.to_string(), new.to_string()));
            }
            i += 3;
        } else {
            if let Some(path) = parts.get(i + 1).filter(|p| !p.is_empty()) {
                let joined = inside(path);
                if status.starts_with('D') {
                    names.deleted.extend(joined.clone());
                }
                names.changed.extend(joined);
            }
            i += 2;
        }
    }
    Ok(names)
}

pub fn get_untracked_files(repo_root: &Path, pathspec: &[String]) -> Result<Vec<PathBuf>> {
    let parts = untracked(repo_root, pathspec)?.names;
    // Lexical `root.join(p)`, never the canonical target: `canonicalize()` on a
    // symlink returns what it POINTS AT, so an untracked `evil -> /etc/shadow`
    // entered the changed-file list under an absolute out-of-root path. That
    // breaks three things at once — the path stops naming an object in the
    // repository, the ignore and secret policies are then applied to the wrong
    // name, and the working-tree line counter reads outside the checkout.
    //
    // Containment is checked lexically and NOT by canonicalising: these names
    // need not exist on disk. A deleted file is gone by definition and a bare
    // clone has no working tree, so an existence test empties both lists. The
    // symlink itself is caught where the file is actually read.
    //
    // An untracked nested repository is reported as one directory entry
    // (`vendor/nested/`, trailing slash): it is not a file of this
    // repository and has no content to fragment, so it is not a change.
    Ok(parts
        .iter()
        .filter(|p| !p.ends_with('/'))
        .filter(|p| crate::paths::contains_lexically(std::path::Path::new(p)))
        .map(|p| crate::paths::repo_join(repo_root, p))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::git_command;
    use crate::snapshot::testing::{commit_all, git, init_git_repo, write_file};
    use std::fs;
    use tempfile::TempDir;

    /// One `--name-status` listing answers what the three listings it
    /// replaced answered, by git's own word on each: renames, deletions and

    /// One `--name-status` listing answers what the three listings it
    /// replaced answered, by git's own word on each: renames, deletions and
    /// names a shell or a header would split.
    #[test]
    fn diff_names_answers_what_the_three_listings_did() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        init_git_repo(root);
        let body = "line one\nline two\nline three\nline four\n";
        let mut names = vec![
            "kept.txt",
            "moved.txt",
            "gone.txt",
            "a b.txt",
            "ünï.txt",
            "-dash.txt",
        ];
        // Windows refuses a tab in a file name.
        if cfg!(unix) {
            names.push("tab\there.txt");
        }
        for name in names {
            fs::write(root.join(name), format!("{name}\n{body}")).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "base"]);
        fs::write(root.join("kept.txt"), "changed\n").unwrap();
        git(root, &["mv", "moved.txt", "renamed.txt"]);
        git(root, &["rm", "-q", "gone.txt", "--", "-dash.txt"]);
        fs::write(root.join("a b.txt"), "changed too\n").unwrap();
        fs::write(root.join("added.txt"), "new\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "change"]);

        let range = Some("HEAD~1..HEAD");
        let listed = |extra: &[&str]| diff_text(root, extra, range, &[]).unwrap();
        let names = diff_names(root, range, &[]).unwrap();
        let joined = |s: &str| crate::paths::repo_join(root, s);

        let name_only: Vec<PathBuf> = listed(&["--name-only", "-M", "-z"])
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(joined)
            .collect();
        assert_eq!(names.changed, name_only);

        let deleted: FxHashSet<PathBuf> = listed(&["--diff-filter=D", "--name-only", "-M", "-z"])
            .split('\0')
            .filter(|p| !p.is_empty())
            .map(joined)
            .collect();
        assert_eq!(names.deleted, deleted);
        assert_eq!(deleted.len(), 2, "{deleted:?}");

        let renames = listed(&["--diff-filter=R", "--name-status", "-M", "-z"]);
        let fields: Vec<&str> = renames.split('\0').filter(|p| !p.is_empty()).collect();
        let pairs: Vec<(String, String)> = fields
            .chunks(3)
            .map(|r| (r[1].to_string(), r[2].to_string()))
            .collect();
        assert_eq!(names.rename_pairs(), pairs);
        assert_eq!(
            pairs,
            vec![("moved.txt".to_string(), "renamed.txt".to_string())]
        );
    }

    // --- SAFE_DIFF_FLAGS pins the parser against hostile repo-local config ---

    struct HunkShape {
        old_start: u32,
        old_len: u32,
        new_start: u32,
        new_len: u32,
    }

    fn hunk_shapes(hunks: &[DiffHunk]) -> Vec<HunkShape> {
        hunks
            .iter()
            .map(|h| HunkShape {
                old_start: h.old_start,
                old_len: h.old_len,
                new_start: h.new_start,
                new_len: h.new_len,
            })
            .collect()
    }

    fn basenames(paths: &[PathBuf]) -> Vec<String> {
        let mut names: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn assert_diff_survives_hostile_config(hostile_config: &[&[&str]]) {
        let tmp = TempDir::new().expect("tempdir");
        let clean_root = tmp.path().join("clean");
        let hostile_root = tmp.path().join("hostile");
        fs::create_dir_all(&clean_root).expect("mkdir clean");
        fs::create_dir_all(&hostile_root).expect("mkdir hostile");

        let body = |a: u32, c: u32| {
            format!(
                "def f():\n    return {a}\n\n\ndef g():\n    return 0\n\n\ndef h():\n    return {c}\n"
            )
        };
        for root in [&clean_root, &hostile_root] {
            init_git_repo(root);
            write_file(root, "app.py", &body(1, 1));
            commit_all(root, "initial");
            write_file(root, "app.py", &body(2, 2));
            commit_all(root, "change");
        }
        for args in hostile_config {
            git(&hostile_root, args);
        }

        let clean_hunks =
            parse_diff(&clean_root, Some("HEAD~1..HEAD"), &[]).expect("clean parse_diff");
        let hostile_hunks =
            parse_diff(&hostile_root, Some("HEAD~1..HEAD"), &[]).expect("hostile parse_diff");
        assert!(
            !hostile_hunks.is_empty(),
            "hostile git config reduced the diff to zero hunks"
        );
        assert_eq!(
            hunk_shapes(&hostile_hunks)
                .iter()
                .map(|s| (s.old_start, s.old_len, s.new_start, s.new_len))
                .collect::<Vec<_>>(),
            hunk_shapes(&clean_hunks)
                .iter()
                .map(|s| (s.old_start, s.old_len, s.new_start, s.new_len))
                .collect::<Vec<_>>(),
            "hostile config changed the parsed hunk shape vs a clean-config repo"
        );

        let clean_files = diff_names(&clean_root, Some("HEAD~1..HEAD"), &[])
            .expect("clean changed files")
            .changed;
        let hostile_files = diff_names(&hostile_root, Some("HEAD~1..HEAD"), &[])
            .expect("hostile changed files")
            .changed;
        assert!(
            !hostile_files.is_empty(),
            "hostile git config reduced changed_files to empty"
        );
        assert_eq!(
            basenames(&hostile_files),
            basenames(&clean_files),
            "hostile config changed the changed_files set vs a clean-config repo"
        );
    }

    #[test]
    fn diff_survives_diff_noprefix() {
        assert_diff_survives_hostile_config(&[&["config", "diff.noprefix", "true"]]);
    }

    #[test]
    fn diff_survives_diff_mnemonic_prefix() {
        assert_diff_survives_hostile_config(&[&["config", "diff.mnemonicPrefix", "true"]]);
    }

    #[test]
    fn diff_survives_custom_src_dst_prefix() {
        assert_diff_survives_hostile_config(&[
            &["config", "diff.srcPrefix", "x/"],
            &["config", "diff.dstPrefix", "y/"],
        ]);
    }

    #[test]
    fn diff_survives_color_ui_always() {
        assert_diff_survives_hostile_config(&[&["config", "color.ui", "always"]]);
    }

    /// Merged across the untouched `g`, the two edits became one hunk and
    /// `g` was emitted as changed.
    #[test]
    fn diff_survives_inter_hunk_context() {
        assert_diff_survives_hostile_config(&[&["config", "diff.interHunkContext", "20"]]);
    }

    /// `GIT_DIFF_OPTS` overrides `--unified` inside git, so it must never
    /// reach a child; the end-to-end run with the variable set lives in
    /// `tests/test_git_config_resilience.py`.
    #[test]
    fn git_diff_opts_is_scrubbed_from_every_child() {
        let cmd = git_command(Path::new("."));
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "GIT_DIFF_OPTS" && v.is_none()),
            "GIT_DIFF_OPTS is not removed from the child environment"
        );
    }

    // --- unquote_c_style + the quoted diff-header branch ---

    #[test]
    fn unquote_c_style_decodes_octal_utf8_escapes() {
        // Exactly what git emits for `café.py` under the default
        // core.quotePath=true: é is UTF-8 0xC3 0xA9, i.e. octal 303 251.
        let quoted = r#""a/caf\303\251.py""#;
        assert_eq!(unquote_c_style(quoted), "a/café.py");
    }

    #[test]
    fn unquote_c_style_leaves_unquoted_input_untouched() {
        assert_eq!(unquote_c_style("a/plain.py"), "a/plain.py");
    }

    #[test]
    fn parse_path_line_takes_quoted_branch_for_old_and_new_headers() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        // The path must exist on disk: parse_path_line canonicalizes the
        // joined path to guard against traversal, and a nonexistent target
        // can fail to canonicalize while the (existing) root does, tripping
        // the containment check on platforms where the temp dir sits behind
        // a symlink (e.g. macOS /var -> /private/var) for reasons unrelated
        // to the quoted-header parsing this test targets.
        write_file(root, "café.py", "value = 1\n");

        let old_line = r#"--- "a/caf\303\251.py""#;
        let (kind, path) = parse_path_line(old_line, root);
        assert_eq!(kind, "old");
        assert_eq!(
            path.expect("old path")
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "café.py"
        );

        let new_line = r#"+++ "b/caf\303\251.py""#;
        let (kind, path) = parse_path_line(new_line, root);
        assert_eq!(kind, "new");
        assert_eq!(
            path.expect("new path")
                .file_name()
                .unwrap()
                .to_string_lossy(),
            "café.py"
        );
    }

    /// The containment check is lexical (`Path::starts_with` compares
    /// components) and `canonicalize` cannot resolve a path that does not
    /// exist, so the fallback kept `..` in place and `<root>/../x` "started
    /// with" `<root>`. This only failed on macOS, where the temp root
    /// canonicalizes through `/var -> /private/var` and the two spellings stop
    /// matching — so the guard was passing for an accident of layout. Both the
    /// existing and non-existing target are covered: the first is the one that
    /// actually reads a file outside the repository.
    #[test]
    fn a_header_escaping_the_repo_root_is_refused_whether_or_not_the_target_exists() {
        let tmp = TempDir::new().expect("tempdir");
        // Canonicalized on purpose. With the root spelled the same way
        // `canonicalize` would spell it, the lexical fallback prefix matches and
        // the hole reproduces here exactly as it did on Linux CI; spelled via a
        // symlinked temp dir (`/var` on macOS) the mismatch hid it.
        let base = dunce::canonicalize(tmp.path()).expect("canonical tempdir");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("mkdir repo");
        std::fs::write(base.join("outside.py"), "secret = 1\n").expect("write outside");

        for rel in ["../outside.py", "../missing.py", "sub/../../outside.py"] {
            for line in [format!("--- a/{rel}"), format!("+++ b/{rel}")] {
                let (kind, path) = parse_path_line(&line, &root);
                assert_eq!(
                    (kind, path.as_ref()),
                    ("", None),
                    "escaping header accepted: {line}"
                );
            }
        }

        // An ordinary in-repo header still resolves, including one whose file
        // does not exist yet (the old side of a deletion).
        std::fs::write(root.join("real.py"), "x = 1\n").expect("write real");
        for rel in ["real.py", "gone.py", "nested/deep.py"] {
            let (kind, path) = parse_path_line(&format!("--- a/{rel}"), &root);
            assert_eq!(kind, "old", "in-repo header refused: {rel}");
            assert!(path.expect("path").ends_with(rel));
        }
    }

    /// A symlink inside the repository needs no `..` to point outside it, so
    /// rejecting `..` is not on its own enough — the canonical containment check
    /// has to stay reachable rather than being short-circuited by a lexical
    /// fallback that is always true once `..` is gone.
    #[cfg(unix)]
    #[test]
    fn an_in_repo_symlink_pointing_outside_the_root_is_refused() {
        let tmp = TempDir::new().expect("tempdir");
        let base = dunce::canonicalize(tmp.path()).expect("canonical tempdir");
        let root = base.join("repo");
        std::fs::create_dir_all(&root).expect("mkdir repo");

        let outside_dir = base.join("outside");
        std::fs::create_dir_all(&outside_dir).expect("mkdir outside");
        std::fs::write(outside_dir.join("secret.py"), "token = 1\n").expect("write secret");
        std::os::unix::fs::symlink(&outside_dir, root.join("escape"))
            .expect("symlink into the repo");

        let (kind, path) = parse_path_line("--- a/escape/secret.py", &root);
        assert_eq!(
            (kind, path.as_ref()),
            ("", None),
            "a header reaching outside the repo through an in-repo symlink was accepted"
        );

        // A symlink that stays inside the repository is still fine.
        std::fs::create_dir_all(root.join("real")).expect("mkdir real");
        std::fs::write(root.join("real/mod.py"), "y = 1\n").expect("write real");
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).expect("inner symlink");
        let (kind, _) = parse_path_line("--- a/alias/mod.py", &root);
        assert_eq!(kind, "old", "an in-repo symlink was wrongly refused");
    }

    /// The traversal guard in `parse_path_line` returns `("", None)`, which the
    /// hunk loop cannot distinguish from "this line is not a path header". With
    /// no per-file reset the previous file's paths stay live, so the rejected
    /// entry's hunks are charged to the last legitimate file — the guard drops
    /// the path but keeps its line ranges, and those ranges decide which
    /// fragments are marked as changed.
    #[test]
    fn a_rejected_path_header_does_not_charge_its_hunks_to_the_previous_file() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_file(root, "real.py", "a = 1\nb = 2\nc = 3\n");

        let output = concat!(
            "diff --git a/real.py b/real.py\n",
            "--- a/real.py\n",
            "+++ b/real.py\n",
            "@@ -1,1 +1,1 @@\n",
            "-a = 1\n",
            "+a = 9\n",
            "diff --git a/../../escape.py b/../../escape.py\n",
            "--- a/../../escape.py\n",
            "+++ b/../../escape.py\n",
            "@@ -500,20 +500,20 @@\n",
            "-gone\n",
            "+new\n",
        );

        let hunks = parse_hunks_from_diff_output(output, root);
        assert_eq!(
            hunks.len(),
            1,
            "expected only the in-repo file's hunk, got {:?}",
            hunks
                .iter()
                .map(|h| (h.path.as_ref().to_string(), h.new_start))
                .collect::<Vec<_>>()
        );
        assert_eq!(hunks[0].new_start, 1);
        assert!(hunks[0].path.ends_with("real.py"));
    }

    /// A deletion emits `+++ /dev/null`, and a creation emits `--- /dev/null`;
    /// both must still resolve to the side that names a real file.
    #[test]
    fn deletions_and_creations_attribute_their_hunks_to_the_named_side() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        write_file(root, "kept.py", "x = 1\n");
        write_file(root, "added.py", "y = 1\n");
        write_file(root, "removed.py", "z = 1\n");

        let output = concat!(
            "diff --git a/kept.py b/kept.py\n",
            "--- a/kept.py\n",
            "+++ b/kept.py\n",
            "@@ -1,1 +1,1 @@\n",
            "diff --git a/removed.py b/removed.py\n",
            "--- a/removed.py\n",
            "+++ /dev/null\n",
            "@@ -1,1 +0,0 @@\n",
            "diff --git a/added.py b/added.py\n",
            "--- /dev/null\n",
            "+++ b/added.py\n",
            "@@ -0,0 +1,1 @@\n",
        );

        let paths: Vec<String> = parse_hunks_from_diff_output(output, root)
            .iter()
            .map(|h| {
                Path::new(h.path.as_ref())
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(paths, vec!["kept.py", "removed.py", "added.py"]);
    }

    #[test]
    fn parse_diff_handles_real_repo_with_default_quoted_unicode_filename() {
        // core.quotePath defaults to true, so a real git diff over a renamed
        // non-ASCII file exercises the quoted branch end-to-end, not just the
        // helper in isolation.
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_git_repo(root);
        write_file(root, "café.py", "value = 1\n");
        commit_all(root, "initial");
        write_file(root, "café.py", "value = 2\n");
        commit_all(root, "change");

        let hunks = parse_diff(root, Some("HEAD~1..HEAD"), &[]).expect("parse_diff");
        assert!(
            !hunks.is_empty(),
            "quoted unicode diff header was not parsed into any hunk"
        );
        assert!(
            hunks.iter().any(|h| h.path.contains("café")),
            "no hunk carried the decoded unicode path, got: {:?}",
            hunks.iter().map(|h| h.path.as_ref()).collect::<Vec<_>>()
        );
    }
}
