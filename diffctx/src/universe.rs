use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustc_hash::FxHashSet;

use crate::config::limits::LIMITS;
use crate::git;
use crate::languages::get_language_for_file;

const FALLBACK_MAX_FILES: usize = 10_000;

fn is_allowed_file(path: &Path) -> bool {
    get_language_for_file(&path.to_string_lossy()).is_some()
}

fn is_candidate_file(
    file_path: &Path,
    _root_dir: &Path,
    included_set: &FxHashSet<PathBuf>,
) -> bool {
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
        Ok(meta) if meta.len() as usize > LIMITS.max_file_size => return false,
        Err(_) => return false,
        _ => {}
    }
    true
}

pub fn collect_candidate_files(root_dir: &Path, included_set: &FxHashSet<PathBuf>) -> Vec<PathBuf> {
    if let Ok(parts) = git::run_git_z(root_dir, &["ls-files", "-z"]) {
        let all_paths: Vec<PathBuf> = parts.into_iter().map(|f| root_dir.join(f)).collect();
        let files: Vec<PathBuf> = all_paths
            .into_par_iter()
            .filter(|f| is_candidate_file(f, root_dir, included_set))
            .collect();
        return files;
    }

    let mut fallback: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = walkdir(root_dir) {
        for f in entries {
            if fallback.len() >= FALLBACK_MAX_FILES {
                break;
            }
            if is_candidate_file(&f, root_dir, included_set) {
                fallback.push(f);
            }
        }
    }
    fallback
}

fn walkdir(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    walk_recursive(root, &mut result)?;
    Ok(result)
}

fn walk_recursive(dir: &Path, result: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.starts_with('.') || name == "node_modules" || name == "__pycache__" {
                continue;
            }
            walk_recursive(&path, result)?;
        } else {
            result.push(path);
        }
    }
    Ok(())
}

pub fn normalize_path(path: &Path, root_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        let joined = root_dir.join(path);
        joined.canonicalize().unwrap_or(joined)
    }
}
