use std::path::{Path, PathBuf};

use rustc_hash::FxHashSet;

use crate::config::limits::LIMITS;
use crate::languages::get_language_for_file;

const SHEBANG_SNIFF_BYTES: usize = 128;

/// `is_candidate_file` for a blob of the analysed snapshot: its language
/// comes from its name, its first bytes only when it has no extension.
fn is_candidate(
    path: &Path,
    included_set: &FxHashSet<PathBuf>,
    source: &crate::source::Source,
) -> bool {
    if included_set.contains(path) {
        return false;
    }
    let name = path.to_string_lossy();
    get_language_for_file(&name).is_some()
        || (path.extension().is_none()
            && source.read_bytes(path).is_some_and(|bytes| {
                let head = &bytes[..bytes.len().min(SHEBANG_SNIFF_BYTES)];
                crate::languages::sniff_language(&name, &String::from_utf8_lossy(head)).is_some()
            }))
}

/// The discovery universe under `scope` (repo-relative pathspecs; empty is
/// the whole repository), read from the run's snapshot alone, with every
/// size exclusion recorded on `ctx`. A listing that failed leaves the
/// universe empty and says so: a walk of the disk in its place answered for
/// a state nobody asked about (#443).
pub fn collect_candidate_files(
    root_dir: &Path,
    included_set: &FxHashSet<PathBuf>,
    scope: &[String],
    ctx: &crate::resource::RunContext,
) -> Vec<PathBuf> {
    let source = ctx.source();
    let listing = match source.list() {
        Ok(listing) => listing,
        Err(e) => {
            tracing::warn!("discovery: the snapshot's listing failed ({e})");
            ctx.note(crate::resource::reason_for(&e));
            return Vec::new();
        }
    };
    // A source file the universe will not read is a hole in the context, and
    // the artifact says so rather than rendering as complete (#T4.2: a 237 KB
    // helper importing the changed file vanished with no coverage block).
    for path in ctx
        .bounded(&listing.too_large)
        .filter(|p| crate::source::in_scope(root_dir, p, scope))
        .filter(|p| !included_set.contains(*p))
        .filter(|p| get_language_for_file(&p.to_string_lossy()).is_some())
    {
        ctx.note_file(
            crate::resource::LimitReason::FileTooLarge,
            crate::paths::display_rel_or_abs(root_dir, path),
        );
    }
    ctx.bounded(listing.within(root_dir, scope))
        .filter(|b| is_candidate(&b.path, included_set, &source))
        .filter(|b| {
            if b.size as usize <= LIMITS.max_file_size {
                return true;
            }
            ctx.note_file(
                crate::resource::LimitReason::FileTooLarge,
                crate::paths::display_rel_or_abs(root_dir, &b.path),
            );
            false
        })
        .map(|b| b.path.clone())
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
