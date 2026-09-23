use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use walkdir::WalkDir;

use crate::config::graph_filtering::GRAPH_FILTERING;
use crate::config::limits::LIMITS;
use crate::git;
use crate::languages::get_language_for_file;

const SHEBANG_SNIFF_BYTES: usize = 128;

fn is_allowed_file(path: &Path) -> bool {
    let name = path.to_string_lossy();
    get_language_for_file(&name).is_some()
        || (path.extension().is_none() && has_known_shebang(path, &name))
}

fn has_known_shebang(path: &Path, name: &str) -> bool {
    use std::io::Read;
    let mut head = [0u8; SHEBANG_SNIFF_BYTES];
    let Ok(read) = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) else {
        return false;
    };
    crate::languages::sniff_language(name, &String::from_utf8_lossy(&head[..read])).is_some()
}

fn is_candidate_file(
    file_path: &Path,
    root_dir: &Path,
    included_set: &FxHashSet<PathBuf>,
    ctx: &crate::resource::RunContext,
) -> bool {
    // `is_file()` and `metadata()` both follow symlinks, and every path here is
    // only LEXICALLY under the root — `git ls-files` reports the link, not its
    // target. Without this the universe admits `repo/evil -> /etc/shadow`,
    // discovery reads it with a bare `read_to_string`, and the content is a
    // context fragment. The boundary is checked once, here, so the readers
    // downstream stay simple; a link pointing INSIDE the repo still resolves.
    if crate::paths::resolve_within(root_dir, file_path).is_none() {
        return false;
    }
    if !file_path.is_file() {
        return false;
    }
    if !is_allowed_file(file_path) {
        return false;
    }
    if included_set.contains(file_path) {
        return false;
    }
    match file_path.metadata() {
        Ok(meta) if meta.len() as usize > LIMITS.max_file_size => {
            // A source file the universe will not read is a hole in the
            // context, and the artifact says so rather than rendering as
            // complete (#T4.2: a 237 KB helper importing the changed file
            // vanished with no coverage block).
            ctx.note(crate::resource::LimitReason::FileTooLarge);
            return false;
        }
        Err(_) => return false,
        _ => {}
    }
    true
}

/// The discovery universe under `scope` (repo-relative pathspecs; empty is
/// the whole repository), with every size exclusion recorded on `ctx`.
pub fn collect_candidate_files(
    root_dir: &Path,
    included_set: &FxHashSet<PathBuf>,
    scope: &[String],
    ctx: &crate::resource::RunContext,
) -> Vec<PathBuf> {
    match tracked_candidates(root_dir, included_set, scope, ctx) {
        Some(files) => filter_ignored_and_secret(root_dir, files),
        // The walk sees untracked paths, so ancestor-inherited rules must count
        // (`.venv/x.py` is ignored BY `.venv/`); the attribution variant drops them.
        None => filter_ignored_and_secret_walked(
            root_dir,
            walked_candidates(root_dir, included_set, scope, ctx),
        ),
    }
}

fn tracked_candidates(
    root_dir: &Path,
    included_set: &FxHashSet<PathBuf>,
    scope: &[String],
    ctx: &crate::resource::RunContext,
) -> Option<Vec<PathBuf>> {
    let mut args: Vec<&str> = vec!["ls-files", "-z"];
    if !scope.is_empty() {
        args.push("--");
        args.extend(scope.iter().map(String::as_str));
    }
    let parts = git::run_git_z(root_dir, &args).ok()?;
    Some(
        parts
            .into_iter()
            .map(|f| crate::paths::repo_join(root_dir, &f))
            .collect::<Vec<PathBuf>>()
            .into_par_iter()
            .filter(|f| is_candidate_file(f, root_dir, included_set, ctx))
            .collect(),
    )
}

fn is_walk_skipped_dir(entry: &walkdir::DirEntry) -> bool {
    entry.depth() > 0
        && entry.file_type().is_dir()
        && entry.file_name().to_str().is_some_and(|name| {
            name.starts_with('.') || name == "node_modules" || name == "__pycache__"
        })
}

fn walked_candidates(
    root_dir: &Path,
    included_set: &FxHashSet<PathBuf>,
    scope: &[String],
    ctx: &crate::resource::RunContext,
) -> Vec<PathBuf> {
    let walk_roots: Vec<PathBuf> = if scope.is_empty() {
        vec![root_dir.to_path_buf()]
    } else {
        scope
            .iter()
            .map(|rel| crate::paths::repo_join(root_dir, rel))
            .collect()
    };
    let mut admitted: Vec<PathBuf> = Vec::new();
    for walk_root in walk_roots {
        let files = WalkDir::new(walk_root)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| !is_walk_skipped_dir(e))
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file());
        for entry in files {
            let path = entry.into_path();
            if is_candidate_file(&path, root_dir, included_set, ctx) {
                admitted.push(path);
            }
            // Counted AFTER admission: capping raw entries meant a repository
            // whose first N walked files are ignored (a vendored tree, a build
            // directory) produced a nearly empty universe while the useful files
            // sat just past the cut.
            if admitted.len() >= GRAPH_FILTERING.fallback_max_files {
                return admitted;
            }
        }
    }
    admitted
}

fn filter_ignored_and_secret_walked(root_dir: &Path, files: Vec<PathBuf>) -> Vec<PathBuf> {
    let rel_paths: Vec<String> = files
        .iter()
        .filter_map(|f| crate::pipeline::rel_path_string(root_dir, f))
        .collect();
    let ignored: FxHashMap<String, git::IgnoreSource> =
        git::find_ignored_paths_any(root_dir, &rel_paths)
            .into_iter()
            .map(|p| (p, git::IgnoreSource::Gitignore))
            .collect();
    files
        .into_par_iter()
        .filter(|f| !crate::pipeline::is_withheld(root_dir, f, &ignored))
        .collect()
}

/// Discovery's candidate universe otherwise skips straight from a language
/// check to the graph: an unchanged file a changed file imports would render
/// as neighbour context even when `.diffctx/ignore` explicitly excludes it,
/// or when it is secret-like (`id_rsa`, `*.pem`, ...) — `changed_files` is
/// filtered this way already (`pipeline::compute_scored_state`), the
/// discovery universe was not. One batched `git check-ignore` call covers
/// every survivor of the language filter, so cost stays O(1) subprocess
/// invocations regardless of repo size, and the final filter still runs in
/// parallel via rayon.
fn filter_ignored_and_secret(root_dir: &Path, files: Vec<PathBuf>) -> Vec<PathBuf> {
    let rel_paths: Vec<String> = files
        .iter()
        .filter_map(|f| crate::pipeline::rel_path_string(root_dir, f))
        .collect();
    let ignored_rel_paths = git::find_ignored_paths_with_source(root_dir, &rel_paths);
    files
        .into_par_iter()
        .filter(|f| !crate::pipeline::is_withheld(root_dir, f, &ignored_rel_paths))
        .collect()
}

pub fn normalize_path(path: &Path, root_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        dunce::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        let joined = crate::paths::repo_join(root_dir, &path.to_string_lossy());
        dunce::canonicalize(&joined).unwrap_or(joined)
    }
}
