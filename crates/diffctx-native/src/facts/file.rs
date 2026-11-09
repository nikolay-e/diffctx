//! The facts of one file of one snapshot: the blob's facts, found in the cache or
//! extracted, bound to the path, project and module the workspace gives it.

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::facts::blob::blob_oid;
use crate::facts::cache::{CacheKey, FactsCache, SCHEMA};
use crate::facts::grammar;
use crate::facts::language::Language;
use crate::facts::model::{BlobFacts, FileFacts};
use crate::facts::module;
use crate::snapshot::{
    AnalysisContext, Bytes, Decision, NotAnalysedReason, RepoPath, Result, SideTag, SnapshotReader,
};

/// An extensionless file is read for a shebang only up to this size.
const SHEBANG_FILE_BYTES: u64 = 1 << 20;

/// The facts of `path` on one side of the range.
///
/// `Ok(None)`: the snapshot has no such file, withholds it, lists it without
/// reading it as source, or no extractor reads its language. An unreadable or
/// oversize file is an `Err`, as in the reader. A hit on a tree side is served
/// from the blob's id without reading the blob. A panic of the extractor is
/// caught here and is `Loss::ExtractorFailed` for this file, never for the run.
pub fn facts_of(
    ctx: &AnalysisContext,
    side: SideTag,
    cache: &FactsCache,
    path: &RepoPath,
) -> Result<Option<FileFacts>> {
    let reader = match side {
        SideTag::Base => &ctx.base,
        SideTag::Head => &ctx.head,
    };
    let listing = cache.listing(reader)?;
    let Ok(at) = listing
        .entries
        .binary_search_by(|entry| entry.path.cmp(path))
    else {
        let oversize =
            reader.decision(path)? == Some(Decision::NotAnalysed(NotAnalysedReason::Oversize));
        let readable = Language::from_path(path)
            .is_some_and(|language| cache.registry().get(language).is_some());
        if oversize && readable {
            reader.read(path)?;
        }
        return Ok(None);
    };
    let entry = &listing.entries[at];

    let mut bytes: Option<Bytes> = None;
    let language = match Language::from_path(path) {
        Some(language) => language,
        None if Language::needs_shebang(path) && entry.size <= SHEBANG_FILE_BYTES => {
            let Some(read) = reader.read(path)? else {
                return Ok(None);
            };
            let Some(language) = Language::from_shebang(&read) else {
                return Ok(None);
            };
            bytes = Some(read);
            language
        }
        None => return Ok(None),
    };
    let Some(extractor) = cache.registry().get(language) else {
        return Ok(None);
    };

    let blob = match &entry.oid {
        Some(oid) => oid.clone(),
        None => {
            let Some(read) = load(reader, path, &mut bytes)? else {
                return Ok(None);
            };
            blob_oid(&read)?
        }
    };
    let key = CacheKey {
        blob,
        language,
        build: cache.fingerprint(),
        grammar: extractor.grammar(),
        schema: SCHEMA,
    };
    let facts = match cache.get(&key) {
        Some(hit) => hit,
        None => {
            let Some(read) = load(reader, path, &mut bytes)? else {
                return Ok(None);
            };
            let mut extracted =
                catch_unwind(AssertUnwindSafe(|| extractor.extract(&read, &ctx.run)))
                    .unwrap_or_else(|_| BlobFacts::failed(language));
            extracted.in_source_order();
            cache.put(&key, extracted)
        }
    };
    let (project, module) = module::locate(&ctx.workspace, language, path.as_str());
    let outdated_grammar = !facts.losses.is_empty()
        && facts.parse_error_lines().is_some()
        && grammar::newer_grammar_reads(language, &key.blob);
    Ok(Some(FileFacts {
        path: path.clone(),
        project,
        module,
        blob: facts,
        outdated_grammar,
    }))
}

fn load(
    reader: &SnapshotReader,
    path: &RepoPath,
    slot: &mut Option<Bytes>,
) -> Result<Option<Bytes>> {
    if slot.is_none() {
        *slot = reader.read(path)?;
    }
    Ok(slot.clone())
}
