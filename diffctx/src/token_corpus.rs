use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use rayon::prelude::*;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::config::bm25::BM25;
use crate::config::tokenization::TOKENIZATION;
use crate::discovery::DiscoveryContext;
use crate::git;

// Bump whenever identifier extraction changes (regex, lowercasing, length
// filtering): the epoch is part of the on-disk cache key, so stale entries
// are invalidated instead of silently poisoning results.
pub const TOKENIZER_EPOCH: u32 = 1;

pub struct DocTokens {
    pub term_counts: FxHashMap<String, u32>,
    pub total_len: u32,
}

pub struct TokenCorpus {
    pub docs: Vec<(PathBuf, DocTokens)>,
}

// The rare-identifier and BM25 strategies can share one tokenized corpus
// only because their tokenizers are identical: same identifier regex, same
// lowercasing, equal minimum lengths. The assert pins that assumption -
// filtering by post-lowercase length is NOT equivalent to the pre-lowercase
// length filter for non-ASCII identifiers, so if these configs ever diverge
// the corpus must be split back into per-strategy passes.
fn shared_min_token_length() -> usize {
    debug_assert_eq!(
        TOKENIZATION.query_min_identifier_length,
        BM25.min_query_token_length
    );
    TOKENIZATION
        .query_min_identifier_length
        .min(BM25.min_query_token_length)
}

impl TokenCorpus {
    pub fn build(ctx: &DiscoveryContext) -> Self {
        let t0 = Instant::now();
        let min_len = shared_min_token_length();
        let changed_set: FxHashSet<&Path> = ctx.changed_files.iter().map(|p| p.as_path()).collect();
        let store = TokenCacheStore::open(min_len);
        let oids = if store.is_some() {
            resolve_cacheable_oids(&ctx.root_dir)
        } else {
            FxHashMap::default()
        };

        let hits = AtomicUsize::new(0);
        let tokenized = AtomicUsize::new(0);
        let docs: Vec<(PathBuf, DocTokens)> = ctx
            .all_candidates
            .par_iter()
            .filter(|f| !changed_set.contains(f.as_path()))
            .filter_map(|f| {
                let oid = oids.get(f.as_path());
                if let (Some(store), Some(oid)) = (store.as_ref(), oid) {
                    if let Some(doc) = store.load(oid) {
                        hits.fetch_add(1, Ordering::Relaxed);
                        return Some((f.clone(), doc));
                    }
                }
                let content = ctx.read_file(f)?;
                let (term_counts, total_len) =
                    crate::types::extract_identifier_counts(&content, min_len);
                let doc = DocTokens {
                    term_counts,
                    total_len,
                };
                if let (Some(store), Some(oid)) = (store.as_ref(), oid) {
                    store.save(oid, &doc);
                }
                tokenized.fetch_add(1, Ordering::Relaxed);
                Some((f.clone(), doc))
            })
            .collect();

        tracing::debug!(
            "token corpus: {} docs ({} cache hits, {} tokenized) in {:.3}s",
            docs.len(),
            hits.load(Ordering::Relaxed),
            tokenized.load(Ordering::Relaxed),
            t0.elapsed().as_secs_f64(),
        );
        Self { docs }
    }
}

// Blob OIDs are only usable as cache keys for regular tracked files whose
// working-tree content matches the index: symlinks/gitlinks, conflicted
// stages, and files with unstaged modifications all bypass the cache and
// fall through to a direct read+tokenize, which keeps cold and warm runs
// bit-equivalent. Any git failure disables keying entirely for the run.
fn resolve_cacheable_oids(root_dir: &Path) -> FxHashMap<PathBuf, String> {
    let Ok(entries) = git::run_git_z(root_dir, &["ls-files", "-s", "-z"]) else {
        return FxHashMap::default();
    };
    let Ok(dirty_parts) = git::run_git_z(root_dir, &["diff-files", "--name-only", "-z"]) else {
        return FxHashMap::default();
    };
    let dirty: FxHashSet<PathBuf> = dirty_parts.into_iter().map(|p| root_dir.join(p)).collect();

    let mut oids: FxHashMap<PathBuf, String> = FxHashMap::default();
    for entry in entries {
        let Some((meta, rel)) = entry.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split_ascii_whitespace();
        let (Some(mode), Some(oid), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if stage != "0" || !(mode == "100644" || mode == "100755") {
            continue;
        }
        let path = root_dir.join(rel);
        if dirty.contains(&path) {
            continue;
        }
        oids.insert(path, oid.to_string());
    }
    oids
}

#[derive(Serialize, Deserialize)]
struct StoredDoc {
    len: u32,
    terms: Vec<(String, u32)>,
}

struct TokenCacheStore {
    dir: PathBuf,
}

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

impl TokenCacheStore {
    fn open(min_token_length: usize) -> Option<Self> {
        let root = std::env::var_os("DIFFCTX_TOKEN_CACHE_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(default_cache_root)?;
        let dir = root.join(format!("v{TOKENIZER_EPOCH}-l{min_token_length}"));
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    fn entry_path(&self, oid: &str) -> Option<PathBuf> {
        if oid.len() < 3 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(self.dir.join(&oid[..2]).join(&oid[2..]))
    }

    fn load(&self, oid: &str) -> Option<DocTokens> {
        let bytes = std::fs::read(self.entry_path(oid)?).ok()?;
        let stored: StoredDoc = serde_json::from_slice(&bytes).ok()?;
        Some(DocTokens {
            term_counts: stored.terms.into_iter().collect(),
            total_len: stored.len,
        })
    }

    fn save(&self, oid: &str, doc: &DocTokens) {
        let Some(path) = self.entry_path(oid) else {
            return;
        };
        let mut terms: Vec<(String, u32)> = doc
            .term_counts
            .iter()
            .map(|(t, c)| (t.clone(), *c))
            .collect();
        terms.sort();
        let stored = StoredDoc {
            len: doc.total_len,
            terms,
        };
        let Ok(bytes) = serde_json::to_vec(&stored) else {
            return;
        };
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let tmp = parent.join(format!(
            ".{}.{}.{}.tmp",
            &oid[2..],
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        if std::fs::write(&tmp, &bytes).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        if std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn default_cache_root() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Caches/diffctx/token-cache"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("diffctx/token-cache"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .map(|c| c.join("diffctx/token-cache"))
    }
}
