//! Facts of a blob, kept by what produced them. The disk layer holds one file per
//! key: a checksum line and the facts as JSON. JSON because an entry that does
//! not parse is a miss, never a misread, and because a cache can be read with
//! `cat`. A cache is an optimisation: an entry that is corrupt, unreadable or
//! unwritable costs a re-extraction and is counted in [`CacheStats`], and never
//! fails the answer.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::facts::extractor::Registry;
use crate::facts::hash::checksum;
use crate::facts::lang;
use crate::facts::language::Language;
use crate::facts::model::{BlobFacts, Fact, Lexical};
use crate::snapshot::{ContentId, Listing, Oid, Result, SnapshotReader};

/// The stored layout, named in the key and in the file name. This file is under
/// `src/`, so a change to the layout moves [`FINGERPRINT`] whether or not this is
/// bumped.
pub const SCHEMA: u32 = 2;

/// The digest of the sources under `src/` and of the lock file the crate was
/// built with (`build.rs`): what an extractor emits is a function of them, so an
/// entry written by another build is a miss and nobody has to remember to say so.
pub const FINGERPRINT: u64 = include!(concat!(env!("OUT_DIR"), "/facts_fingerprint.in"));
pub const DEFAULT_CAP_BYTES: u64 = 256 * 1024 * 1024;
const ENTRY_SUFFIX: &str = ".json";
const TEMP_PREFIX: &str = ".tmp-";
const LISTINGS_KEPT: usize = 4;

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// What an entry was produced by: the blob, the language, the build's fingerprint,
/// the digest of the grammar and the stored layout. Any of them changing is a miss.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey {
    pub blob: Oid,
    pub language: Language,
    pub build: u64,
    pub grammar: u64,
    pub schema: u32,
}

impl CacheKey {
    fn file_name(&self) -> String {
        format!(
            "{}-{}-{:016x}-{:016x}-{}{ENTRY_SUFFIX}",
            self.blob, self.language, self.build, self.grammar, self.schema
        )
    }
}

#[derive(Serialize)]
struct EntryRef<'a> {
    language: Language,
    parse_errors: &'a [&'a [(u32, u32)]],
    facts: &'a [Fact],
    lexical: &'a [(Range<u32>, Lexical)],
}

#[derive(Deserialize)]
struct Entry {
    language: Language,
    parse_errors: Vec<Vec<(u32, u32)>>,
    facts: Vec<Fact>,
    lexical: Vec<(Range<u32>, Lexical)>,
}

/// What the cache did since it was built.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CacheStats {
    pub memory_hits: u64,
    pub disk_hits: u64,
    pub misses: u64,
    pub writes: u64,
    pub write_failures: u64,
    /// Entries found corrupt, unreadable or of another language.
    pub discarded: u64,
    pub evicted: u64,
}

#[derive(Default)]
struct Counters {
    memory_hits: AtomicU64,
    disk_hits: AtomicU64,
    misses: AtomicU64,
    writes: AtomicU64,
    write_failures: AtomicU64,
    discarded: AtomicU64,
    evicted: AtomicU64,
}

fn bump(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::Relaxed);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Stored {
    path: PathBuf,
    name: OsString,
    len: u64,
    modified: SystemTime,
}

struct Store {
    dir: PathBuf,
    cap: u64,
    /// The bytes the directory holds, as far as this process knows; `None` until
    /// the first write measures it.
    used: Mutex<Option<u64>>,
}

impl Store {
    #[allow(
        clippy::disallowed_methods,
        reason = "the facts cache is the tool's own store under the user cache directory, not repository bytes; an entry whose metadata cannot be read is left out of the size count"
    )]
    fn scan(&self) -> Vec<Stored> {
        let Ok(dir) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.ends_with(ENTRY_SUFFIX) || name.starts_with(TEMP_PREFIX)
            })
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                Some(Stored {
                    path: entry.path(),
                    name: entry.file_name(),
                    len: meta.len(),
                    modified: meta.modified().unwrap_or(UNIX_EPOCH),
                })
            })
            .collect()
    }

    /// Notes `added` bytes written and, past the cap, drops the least recently
    /// used entries down to nine tenths of it.
    fn account(&self, added: u64, counters: &Counters) {
        let mut used = lock(&self.used);
        let total = match *used {
            Some(total) => total + added,
            None => self.scan().iter().map(|file| file.len).sum(),
        };
        *used = Some(if total > self.cap {
            self.evict(counters)
        } else {
            total
        });
    }

    fn evict(&self, counters: &Counters) -> u64 {
        let mut files = self.scan();
        files.sort_by(|a, b| (a.modified, &a.name).cmp(&(b.modified, &b.name)));
        let mut total: u64 = files.iter().map(|file| file.len).sum();
        let target = self.cap - self.cap / 10;
        for file in files {
            if total <= target {
                break;
            }
            if fs::remove_file(&file.path).is_ok() {
                total = total.saturating_sub(file.len);
                bump(&counters.evicted);
            }
        }
        total
    }
}

/// Facts by [`CacheKey`], in memory and on disk. Hands out the same `Arc` to every
/// path and snapshot holding the blob.
pub struct FactsCache {
    registry: Registry,
    /// What its entries are keyed by: the build's, unless a test says otherwise.
    fingerprint: u64,
    store: Option<Store>,
    /// Off for a cache that must not remember anything (`disabled`).
    remember: bool,
    memory: Mutex<HashMap<String, Arc<BlobFacts>>>,
    listings: Mutex<Vec<(ContentId, Arc<Listing>)>>,
    counters: Counters,
}

impl FactsCache {
    /// A cache that persists under `dir`, created on the first write.
    pub fn new(dir: PathBuf) -> Self {
        Self {
            store: Some(Store {
                dir,
                cap: DEFAULT_CAP_BYTES,
                used: Mutex::new(None),
            }),
            remember: true,
            ..Self::disabled()
        }
    }

    /// The user's cache directory ([`default_dir`]); in memory for the process
    /// when the environment names none.
    pub fn user() -> Self {
        default_dir(|name| std::env::var_os(name)).map_or_else(Self::in_memory, Self::new)
    }

    /// Remembers within the process and writes nothing.
    pub fn in_memory() -> Self {
        Self {
            remember: true,
            ..Self::disabled()
        }
    }

    /// Remembers nothing and writes nothing: every file is extracted every time.
    pub fn disabled() -> Self {
        Self {
            registry: lang::builtin(),
            fingerprint: FINGERPRINT,
            store: None,
            remember: false,
            memory: Mutex::default(),
            listings: Mutex::default(),
            counters: Counters::default(),
        }
    }

    /// The bytes the directory may hold before the least recently used entries go.
    #[must_use]
    pub fn with_cap(mut self, bytes: u64) -> Self {
        if let Some(store) = &mut self.store {
            store.cap = bytes;
        }
        self
    }

    /// Serves these extractors instead of the shipped ones.
    #[must_use]
    pub fn with_registry(mut self, registry: Registry) -> Self {
        self.registry = registry;
        self
    }

    /// Keys its entries on this fingerprint instead of the build's: what a test
    /// uses to stand for another build.
    #[must_use]
    pub fn with_fingerprint(mut self, fingerprint: u64) -> Self {
        self.fingerprint = fingerprint;
        self
    }

    pub fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn dir(&self) -> Option<&Path> {
        self.store.as_ref().map(|store| store.dir.as_path())
    }

    pub fn stats(&self) -> CacheStats {
        let c = &self.counters;
        let read = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        CacheStats {
            memory_hits: read(&c.memory_hits),
            disk_hits: read(&c.disk_hits),
            misses: read(&c.misses),
            writes: read(&c.writes),
            write_failures: read(&c.write_failures),
            discarded: read(&c.discarded),
            evicted: read(&c.evicted),
        }
    }

    pub(crate) fn get(&self, key: &CacheKey) -> Option<Arc<BlobFacts>> {
        let name = key.file_name();
        let remembered = lock(&self.memory).get(&name).cloned();
        if remembered.is_some() {
            bump(&self.counters.memory_hits);
            return remembered;
        }
        let loaded = self
            .store
            .as_ref()
            .and_then(|store| self.read_entry(store, key, &name));
        if let Some(blob) = loaded {
            bump(&self.counters.disk_hits);
            if self.remember {
                lock(&self.memory).insert(name, Arc::clone(&blob));
            }
            Some(blob)
        } else {
            bump(&self.counters.misses);
            None
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the facts cache is the tool's own store under the user cache directory, not repository bytes; an entry that does not decode is a miss, counted as discarded"
    )]
    fn read_entry(&self, store: &Store, key: &CacheKey, name: &str) -> Option<Arc<BlobFacts>> {
        let path = store.dir.join(name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    bump(&self.counters.discarded);
                }
                return None;
            }
        };
        let entry = split_checked(&bytes)
            .and_then(|body| serde_json::from_slice::<Entry>(body).ok())
            .filter(|entry| entry.language == key.language);
        let Some(entry) = entry else {
            bump(&self.counters.discarded);
            return None;
        };
        refresh(&path);
        Some(Arc::new(BlobFacts::from_stored(
            entry.language,
            entry.facts,
            entry.lexical,
            entry.parse_errors,
        )))
    }

    /// Keeps `blob` under `key` when it is the file's whole answer, and returns it
    /// shared. A blob cut short by the budget or a panic is returned and not kept.
    pub(crate) fn put(&self, key: &CacheKey, blob: BlobFacts) -> Arc<BlobFacts> {
        let kept = blob.is_complete();
        let name = key.file_name();
        let blob = Arc::new(blob);
        if !kept {
            return blob;
        }
        if self.remember {
            lock(&self.memory).insert(name.clone(), Arc::clone(&blob));
        }
        if let Some(store) = &self.store {
            self.write_entry(store, &name, &blob);
        }
        blob
    }

    /// Written beside the entry and renamed over it, so a reader sees the old
    /// entry or the new one and never half of either.
    fn write_entry(&self, store: &Store, name: &str, blob: &BlobFacts) {
        let Some(errors) = blob.parse_error_lines() else {
            return;
        };
        let entry = EntryRef {
            language: blob.language,
            parse_errors: &errors,
            facts: &blob.facts,
            lexical: &blob.lexical,
        };
        let Ok(body) = serde_json::to_vec(&entry) else {
            bump(&self.counters.write_failures);
            return;
        };
        let mut bytes = checksum(&body).into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(&body);
        let temp = store.dir.join(format!(
            "{TEMP_PREFIX}{}-{}",
            process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let written = fs::create_dir_all(&store.dir)
            .and_then(|()| fs::write(&temp, &bytes))
            .and_then(|()| fs::rename(&temp, store.dir.join(name)));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
            bump(&self.counters.write_failures);
            return;
        }
        bump(&self.counters.writes);
        store.account(bytes.len() as u64, &self.counters);
    }

    /// The listing of a snapshot, taken once per snapshot: it names each file's
    /// blob, which is what a lookup keys on.
    pub(crate) fn listing(&self, reader: &SnapshotReader) -> Result<Arc<Listing>> {
        let known = lock(&self.listings)
            .iter()
            .find(|(id, _)| id == reader.id())
            .map(|(_, listing)| Arc::clone(listing));
        if let Some(listing) = known {
            return Ok(listing);
        }
        let listing = Arc::new(reader.list()?);
        let mut listings = lock(&self.listings);
        listings.push((reader.id().clone(), Arc::clone(&listing)));
        if listings.len() > LISTINGS_KEPT {
            listings.remove(0);
        }
        Ok(listing)
    }
}

/// The body when the checksum line above it matches.
fn split_checked(bytes: &[u8]) -> Option<&[u8]> {
    let line_end = bytes.iter().position(|byte| *byte == b'\n')?;
    let (line, rest) = bytes.split_at(line_end);
    let body = &rest[1..];
    (line == checksum(body).as_bytes()).then_some(body)
}

/// `$DIFFCTX_CACHE_DIR`, else `$XDG_CACHE_HOME`, else `~/.cache` (`%LOCALAPPDATA%`
/// on Windows), then `diffctx/facts`. `env` is the lookup, so a test never
/// touches the process environment.
pub fn default_dir(env: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let get = |name: &str| env(name).filter(|value| !value.is_empty());
    let base = get("DIFFCTX_CACHE_DIR")
        .or_else(|| get("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| {
            if cfg!(windows) {
                get("LOCALAPPDATA").map(PathBuf::from)
            } else {
                get("HOME").map(|home| PathBuf::from(home).join(".cache"))
            }
        })?;
    Some(base.join("diffctx").join("facts"))
}

/// A hit marks the entry as recently used for the eviction that follows.
fn refresh(path: &Path) {
    if let Ok(file) = fs::OpenOptions::new().write(true).open(path) {
        let _ = file.set_modified(SystemTime::now());
    }
}
