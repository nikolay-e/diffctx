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

// Size cap for the on-disk token cache, overridable with
// DIFFCTX_TOKEN_CACHE_MAX_BYTES (0 = unlimited). The cache is a pure speedup,
// so the default trades a cold tokenization pass for bounded disk use.
const DEFAULT_CACHE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const CACHE_SHARDS: u64 = 256;
const SHARD_EVICTION_TARGET_FRACTION: f64 = 0.8;

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

        if let Some(store) = store.as_ref() {
            store.evict_one_shard();
        }

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

    // Entries are written once and never rewritten, so nothing ages out on its
    // own and the cache grows with every repository ever analyzed. Walking all
    // 256 shards per run would cost more than the cache saves, so each run
    // enforces the per-shard share of the size cap on ONE shard; every shard is
    // reached within a few hundred runs.
    fn evict_one_shard(&self) {
        let Some(max_bytes) = cache_max_bytes() else {
            return;
        };
        let shard = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 % CACHE_SHARDS)
            .unwrap_or(0);
        evict_shard(
            &self.dir.join(format!("{shard:02x}")),
            max_bytes / CACHE_SHARDS,
        );
    }
}

fn cache_max_bytes() -> Option<u64> {
    match std::env::var("DIFFCTX_TOKEN_CACHE_MAX_BYTES") {
        Ok(raw) => cache_max_bytes_from(&raw),
        Err(_) => Some(DEFAULT_CACHE_MAX_BYTES),
    }
}

fn cache_max_bytes_from(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(DEFAULT_CACHE_MAX_BYTES);
    }
    raw.parse::<u64>()
        .map_or(Some(DEFAULT_CACHE_MAX_BYTES), |b| (b > 0).then_some(b))
}

fn evict_shard(shard_dir: &Path, shard_max_bytes: u64) {
    let Ok(entries) = std::fs::read_dir(shard_dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, u64, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            Some((
                meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                meta.len(),
                e.path(),
            ))
        })
        .collect();

    let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
    if total <= shard_max_bytes {
        return;
    }

    // Oldest first: entries are never touched after their single write, so
    // mtime orders them by insertion, not by use.
    files.sort_by_key(|(modified, _, _)| *modified);
    let target = (shard_max_bytes as f64 * SHARD_EVICTION_TARGET_FRACTION) as u64;
    let mut removed = 0usize;
    for (_, len, path) in &files {
        if total <= target {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= len;
            removed += 1;
        }
    }
    tracing::debug!(
        "token cache: evicted {} entries from {}",
        removed,
        shard_dir.display()
    );
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // mtime is stamped explicitly: filesystems whose timestamp granularity is
    // coarser than the writes (CI runners) would otherwise give all entries the
    // same mtime, leaving eviction order up to readdir.
    fn write_entry(dir: &Path, name: &str, bytes: usize, age_secs: u64) {
        let path = dir.join(name);
        std::fs::write(&path, vec![b'x'; bytes]).expect("write entry");
        let stamp = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open entry")
            .set_modified(stamp)
            .expect("set mtime");
    }

    #[test]
    fn evict_shard_drops_oldest_entries_until_under_target() {
        let tmp = TempDir::new().expect("tempdir");
        let shard = tmp.path().join("ab");
        std::fs::create_dir_all(&shard).expect("shard dir");
        for (age_secs, name) in [(3600, "oldest"), (1800, "middle"), (60, "newest")] {
            write_entry(&shard, name, 1000, age_secs);
        }

        evict_shard(&shard, 2000);

        let survivors: Vec<String> = std::fs::read_dir(&shard)
            .expect("read shard")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(survivors, vec!["newest".to_string()]);
    }

    #[test]
    fn evict_shard_keeps_everything_under_the_cap() {
        let tmp = TempDir::new().expect("tempdir");
        let shard = tmp.path().join("cd");
        std::fs::create_dir_all(&shard).expect("shard dir");
        write_entry(&shard, "kept", 1000, 86_400);

        evict_shard(&shard, 4096);

        assert!(shard.join("kept").exists());
    }

    #[test]
    fn cache_max_bytes_honors_the_unlimited_and_override_settings() {
        assert_eq!(cache_max_bytes_from("0"), None);
        assert_eq!(cache_max_bytes_from("4096"), Some(4096));
        assert_eq!(
            cache_max_bytes_from("not-a-number"),
            Some(DEFAULT_CACHE_MAX_BYTES)
        );
        assert_eq!(cache_max_bytes_from(""), Some(DEFAULT_CACHE_MAX_BYTES));
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = git::git_command(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(root: &Path) {
        git(root, &["init", "--quiet"]);
        git(root, &["config", "user.email", "test@example.com"]);
        git(root, &["config", "user.name", "test"]);
        git(root, &["config", "commit.gpgsign", "false"]);
    }

    /// A blob OID is only a valid cache key while the working tree matches the
    /// index. The three bypass branches are what keep cold and warm runs
    /// bit-equivalent, and the determinism fixture cannot reach any of them:
    /// every entry there is mode 100644, stage 0 and clean. A regression here
    /// surfaces only on the *second* run against a given repo, i.e. never in a
    /// fresh CI checkout.
    #[test]
    fn only_clean_regular_tracked_files_are_cache_keyed() {
        let tmp = TempDir::new().expect("tempdir");
        let root = tmp.path();
        init_repo(root);

        std::fs::write(root.join("clean.py"), "x = 1\n").expect("write clean");
        std::fs::write(root.join("exec.sh"), "echo hi\n").expect("write exec");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(root.join("exec.sh"), std::fs::Permissions::from_mode(0o755))
                .expect("chmod exec");
        }
        std::fs::write(root.join("dirty.py"), "y = 1\n").expect("write dirty");
        #[cfg(unix)]
        std::os::unix::fs::symlink("clean.py", root.join("link.py")).expect("symlink");

        git(root, &["add", "-A"]);
        git(root, &["commit", "--quiet", "-m", "base"]);

        // Unstaged modification: the index OID no longer describes the content.
        std::fs::write(root.join("dirty.py"), "y = 2\n").expect("modify dirty");

        let oids = resolve_cacheable_oids(root);
        let keyed = |name: &str| oids.contains_key(&root.join(name));

        assert!(keyed("clean.py"), "a clean 100644 file must be cache-keyed");
        assert!(keyed("exec.sh"), "a clean 100755 file must be cache-keyed");
        assert!(
            !keyed("dirty.py"),
            "a file with unstaged modifications must bypass the cache"
        );
        #[cfg(unix)]
        assert!(
            !keyed("link.py"),
            "a symlink (mode 120000) must bypass the cache"
        );

        for oid in oids.values() {
            assert!(
                oid.len() >= 3 && oid.bytes().all(|b| b.is_ascii_hexdigit()),
                "unusable oid as a cache key: {oid:?}"
            );
        }
    }

    #[test]
    fn resolve_cacheable_oids_is_empty_outside_a_repository() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(resolve_cacheable_oids(tmp.path()).is_empty());
    }

    #[test]
    fn cache_entries_round_trip_and_reject_unusable_oids() {
        let tmp = TempDir::new().expect("tempdir");
        let store = TokenCacheStore {
            dir: tmp.path().to_path_buf(),
        };

        let mut term_counts: FxHashMap<String, u32> = FxHashMap::default();
        term_counts.insert("alpha".into(), 3);
        term_counts.insert("beta".into(), 1);
        let doc = DocTokens {
            term_counts: term_counts.clone(),
            total_len: 4,
        };

        let oid = "abcdef0123456789";
        store.save(oid, &doc);
        let loaded = store.load(oid).expect("entry round-trips");
        assert_eq!(loaded.total_len, doc.total_len);
        assert_eq!(loaded.term_counts, term_counts);

        assert!(store.entry_path("ab").is_none(), "too-short oid accepted");
        assert!(
            store.entry_path("../../etc/passwd").is_none(),
            "non-hex oid accepted as a path component"
        );
        assert!(store.load("zzzz").is_none());
    }
}
