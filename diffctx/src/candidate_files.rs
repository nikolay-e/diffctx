use std::path::{Path, PathBuf};

use rayon::prelude::*;
use rustc_hash::FxHashSet;
use walkdir::WalkDir;

use crate::config::graph_filtering::GRAPH_FILTERING;
use crate::config::limits::LIMITS;
use crate::git;
use crate::languages::get_language_for_file;

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
    for entry in WalkDir::new(root_dir)
        .sort_by_file_name()
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 || !e.file_type().is_dir() {
                return true;
            }
            match e.file_name().to_str() {
                Some(name) => {
                    !name.starts_with('.') && name != "node_modules" && name != "__pycache__"
                }
                None => true,
            }
        })
        .filter_map(|e| e.ok())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        if fallback.len() >= GRAPH_FILTERING.fallback_max_files {
            break;
        }
        let path = entry.into_path();
        if is_candidate_file(&path, root_dir, included_set) {
            fallback.push(path);
        }
    }
    fallback
}

pub fn normalize_path(path: &Path, root_dir: &Path) -> PathBuf {
    if path.is_absolute() {
        path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
    } else {
        let joined = root_dir.join(path);
        joined.canonicalize().unwrap_or(joined)
    }
}
