//! Reads every file under the given roots with the grammars this build pins and
//! prints `<Language> <blob id> ok|error`, one line per file a grammar reads. The
//! bump job runs it before and after the grammars move: a blob that was `error` and
//! is `ok` is a witness (`scripts/bump-grammars.sh witnesses`).

use std::path::Path;

use _diffctx::facts::blob_oid;
use _diffctx::facts::lang::builtin;
use _diffctx::facts::{Extractor, Language};
use _diffctx::snapshot::{Loss, RepoPath, RunBudget};
use walkdir::WalkDir;

const SKIPPED: [&str; 4] = [".git", "target", "node_modules", ".venv"];

fn main() {
    let registry = builtin();
    let budget = RunBudget::default();
    for root in std::env::args().skip(1) {
        let walk = WalkDir::new(&root)
            .into_iter()
            .filter_entry(|e| !SKIPPED.iter().any(|s| e.file_name() == *s));
        for entry in walk
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
        {
            let Ok(relative) = entry.path().strip_prefix(Path::new(&root)) else {
                continue;
            };
            let Ok(path) = RepoPath::new(&relative.to_string_lossy()) else {
                continue;
            };
            let Some(language) = Language::from_path(&path) else {
                continue;
            };
            let Some(extractor) = registry.get(language) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            let Ok(oid) = blob_oid(&bytes) else {
                continue;
            };
            let facts = extractor.extract(&bytes, &budget);
            let broken = facts
                .losses
                .iter()
                .any(|loss| matches!(loss, Loss::ParseError { .. }));
            println!("{language:?} {oid} {}", if broken { "error" } else { "ok" });
        }
    }
}
