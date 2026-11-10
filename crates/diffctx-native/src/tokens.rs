//! The one source of "does this file spell this name": a token index over the
//! raw identifiers of the head snapshot, built once per run.
//!
//! A claim that nothing names a definition rests on files that cannot name it.
//! A file cannot when it holds no token the definition could be spelled by, and
//! that is decided here and nowhere else, over what the file says whole: no
//! stop-word drop, no length floor, no frequency floor (a one-letter package, a
//! name every file spells, are the cases a floor would hide). A file that could
//! not be tokenised (unreadable, past the size cap, out of time) is not "without
//! the name": the index says it was not read, and the caller accounts for it.
//!
//! The unit is the blob: a blob is tokenised once however many paths hold it, and
//! its token set is kept in the facts cache directory under its own namespace,
//! keyed by the blob and the tokenizer's version (not the build's fingerprint: a
//! new binary does not strand it).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use crate::decide::Group;
use crate::facts::{FactsCache, expired};
use crate::snapshot::{AnalysisContext, Listing, Oid, RepoPath, Result, RunBudget, SnapshotReader};

/// Moves when the tokenizer's rules do: a stored token set made by another
/// version is not read.
const TOKENIZER_VERSION: u32 = 1;

/// The bytes the token namespace may hold before the least recently used sets
/// go.
const CACHE_CAP_BYTES: u64 = 128 * 1024 * 1024;

/// The identifiers of `bytes`, ASCII-lowercased (PHP is case-insensitive, and a
/// collision between cases only over-hedges), each once, in order. An identifier
/// is a run of ASCII letters, digits and `_`, and of non-ASCII bytes; `$`, `-`,
/// `.` and `:` separate, so `$name` is `name` and a kebab-case key is its words.
pub fn identifiers(bytes: &[u8]) -> Vec<Box<str>> {
    let lower = bytes.to_ascii_lowercase();
    let mut runs: Vec<&[u8]> = Vec::new();
    let mut start = None;
    for (at, byte) in lower.iter().enumerate() {
        let inside = byte.is_ascii_alphanumeric() || *byte == b'_' || *byte >= 0x80;
        match (inside, start) {
            (true, None) => start = Some(at),
            (false, Some(from)) => {
                runs.push(&lower[from..at]);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        runs.push(&lower[from..]);
    }
    runs.sort_unstable();
    runs.dedup();
    runs.into_iter()
        .map(|run| String::from_utf8_lossy(run).into())
        .collect()
}

/// The names a definition can be spelled by, as the index asks for them. A name
/// is the identifiers it is made of: a file spells it when it holds all of them.
/// A name that has none (`*`, `.`) cannot be looked for: every file may spell it.
pub struct Names {
    alternatives: Vec<Vec<Box<str>>>,
    everywhere: bool,
}

impl Names {
    pub fn new<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut alternatives: Vec<Vec<Box<str>>> = Vec::new();
        let mut everywhere = false;
        for name in names {
            let words = identifiers(name.as_bytes());
            if words.is_empty() {
                everywhere = true;
            } else if !alternatives.contains(&words) {
                alternatives.push(words);
            }
        }
        Self {
            alternatives,
            everywhere,
        }
    }
}

/// The files of one interaction group and what they spell.
pub struct GroupTokens {
    /// The group's files of the head snapshot, by path, tokenised or not.
    files: Vec<RepoPath>,
    /// The indices of the files that have no token set, ascending.
    unread: Vec<u32>,
    /// Token to the indices of the files that hold it, ascending.
    postings: HashMap<Box<str>, Vec<u32>>,
}

impl GroupTokens {
    /// A group nothing could be read for: no file, so nothing is accounted.
    fn empty() -> Self {
        Self {
            files: Vec::new(),
            unread: Vec::new(),
            postings: HashMap::new(),
        }
    }

    pub fn files(&self) -> &[RepoPath] {
        &self.files
    }

    /// The indices of the files that have no token set, ascending.
    pub fn unread(&self) -> &[u32] {
        &self.unread
    }

    /// The indices of the tokenised files that spell one of `names`, ascending.
    pub fn spelling(&self, names: &Names) -> Vec<u32> {
        if names.everywhere {
            return (0..self.files.len() as u32)
                .filter(|at| self.unread.binary_search(at).is_err())
                .collect();
        }
        let mut found: Vec<u32> = Vec::new();
        for words in &names.alternatives {
            let mut lists = words.iter().map(|word| self.postings.get(&**word));
            let Some(Some(first)) = lists.next() else {
                continue;
            };
            let mut held = first.clone();
            for list in lists {
                match list {
                    Some(list) => held = intersection(&held, list),
                    None => held.clear(),
                }
            }
            found.extend(held);
        }
        found.sort_unstable();
        found.dedup();
        found
    }

    /// Whether `path` spells one of `names`: `None` when the group does not
    /// hold it or it was not tokenised.
    pub fn spells(&self, path: &str, names: &Names) -> Option<bool> {
        let at = self
            .files
            .binary_search_by(|file| file.as_str().cmp(path))
            .ok()?;
        let at = u32::try_from(at).ok()?;
        if self.unread.binary_search(&at).is_ok() {
            return None;
        }
        let held = |word: &str| {
            self.postings
                .get(word)
                .is_some_and(|list| list.binary_search(&at).is_ok())
        };
        Some(
            names.everywhere
                || names
                    .alternatives
                    .iter()
                    .any(|words| words.iter().all(|word| held(word))),
        )
    }
}

fn intersection(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// The head snapshot the index reads: its reader and listing, the run's budget
/// and the cache directory.
struct Head {
    reader: SnapshotReader,
    listing: Arc<Listing>,
    run: RunBudget,
    store: Option<Store>,
}

/// Token to files, built per group on first use, shared by every cell of a run.
#[derive(Default)]
pub struct TokenIndex {
    head: OnceLock<Head>,
    groups: Mutex<HashMap<Group, Arc<GroupTokens>>>,
}

impl TokenIndex {
    /// Binds the index to the head snapshot of `ctx`; the first call wins, and
    /// every cell of one run reads the same head.
    pub fn bind(&self, ctx: &AnalysisContext, cache: &FactsCache) -> Result<()> {
        if self.head.get().is_some() {
            return Ok(());
        }
        let listing = cache.listing(&ctx.head)?;
        let store = cache.dir().map(|dir| Store::new(dir.join("tokens")));
        let _ = self.head.set(Head {
            reader: ctx.head.clone(),
            listing,
            run: ctx.run.clone(),
            store,
        });
        Ok(())
    }

    /// The group's files and what they spell. A group of an unbound index has
    /// no file.
    pub fn group(&self, group: Group) -> Arc<GroupTokens> {
        let mut built = self
            .groups
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(known) = built.get(&group) {
            return Arc::clone(known);
        }
        let tokens = Arc::new(
            self.head
                .get()
                .map_or_else(GroupTokens::empty, |head| build(head, group)),
        );
        built.insert(group, Arc::clone(&tokens));
        tokens
    }
}

/// Tokenises the files of `group` in the head listing: the admitted ones by
/// reading them, the listed-but-unread ones (oversize, symlinks, untracked) as
/// files with no token set.
fn build(head: &Head, group: Group) -> GroupTokens {
    let mut files: Vec<(&RepoPath, Option<&Oid>, bool)> = head
        .listing
        .entries
        .iter()
        .filter(|entry| group.has_path(entry.path.as_str()))
        .map(|entry| (&entry.path, entry.oid.as_ref(), true))
        .collect();
    files.extend(
        head.listing
            .not_analysed
            .iter()
            .filter(|(path, _)| group.has_path(path.as_str()))
            .map(|(path, _)| (path, None, false)),
    );
    files.sort_by(|a, b| a.0.cmp(b.0));
    files.dedup_by(|a, b| a.0 == b.0);
    let mut copies: HashMap<&Oid, usize> = HashMap::new();
    for oid in files.iter().filter_map(|(_, oid, _)| *oid) {
        *copies.entry(oid).or_default() += 1;
    }
    let mut shared: HashMap<&Oid, Arc<Vec<Box<str>>>> = HashMap::new();
    let mut unread: Vec<u32> = Vec::new();
    let mut postings: HashMap<Box<str>, Vec<u32>> = HashMap::new();
    for (at, (path, oid, readable)) in files.iter().enumerate() {
        let at = at as u32;
        let tokens = if *readable && !expired(&head.run) {
            token_set(head, path, *oid, &copies, &mut shared)
        } else {
            None
        };
        let Some(tokens) = tokens else {
            unread.push(at);
            continue;
        };
        for token in tokens.iter() {
            match postings.get_mut(&**token) {
                Some(list) => list.push(at),
                None => {
                    postings.insert(token.clone(), vec![at]);
                }
            }
        }
    }
    GroupTokens {
        files: files.into_iter().map(|(path, _, _)| path.clone()).collect(),
        unread,
        postings,
    }
}

/// The token set of one file: from the store when its blob is there, else read
/// and tokenised, and stored. A blob several paths hold is tokenised once.
fn token_set<'a>(
    head: &Head,
    path: &RepoPath,
    oid: Option<&'a Oid>,
    copies: &HashMap<&'a Oid, usize>,
    shared: &mut HashMap<&'a Oid, Arc<Vec<Box<str>>>>,
) -> Option<Arc<Vec<Box<str>>>> {
    if let Some(known) = oid.and_then(|oid| shared.get(oid)) {
        return Some(Arc::clone(known));
    }
    let stored = oid
        .zip(head.store.as_ref())
        .and_then(|(oid, store)| store.load(oid));
    let tokens = match stored {
        Some(tokens) => tokens,
        None => {
            let bytes = match head.reader.read(path) {
                Ok(Some(bytes)) => bytes,
                Ok(None) | Err(_) => return None,
            };
            let tokens = identifiers(&bytes);
            if let Some((oid, store)) = oid.zip(head.store.as_ref()) {
                store.save(oid, &tokens);
            }
            tokens
        }
    };
    let tokens = Arc::new(tokens);
    if let Some(oid) = oid.filter(|oid| copies.get(oid).is_some_and(|n| *n > 1)) {
        shared.insert(oid, Arc::clone(&tokens));
    }
    Some(tokens)
}

/// The token sets on disk, one file per blob. A set that is missing, cut short
/// or unreadable is a miss, never a set with fewer tokens: it is read again.
struct Store {
    dir: PathBuf,
    used: Mutex<Option<u64>>,
}

impl Store {
    fn new(root: PathBuf) -> Self {
        Self {
            dir: root.join(format!("v{TOKENIZER_VERSION}")),
            used: Mutex::new(None),
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the token store is the tool's own cache under the user cache directory, not repository bytes; an entry that does not decode is a miss"
    )]
    fn load(&self, oid: &Oid) -> Option<Vec<Box<str>>> {
        let text = std::fs::read_to_string(self.dir.join(oid.as_str())).ok()?;
        let (count, body) = text.split_once('\n')?;
        let count: usize = count.parse().ok()?;
        let tokens: Vec<Box<str>> = body.lines().map(Box::from).collect();
        (tokens.len() == count && body.ends_with('\n') == (count > 0)).then_some(tokens)
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "the token store is the tool's own cache under the user cache directory; a set that cannot be written costs a re-read"
    )]
    fn save(&self, oid: &Oid, tokens: &[Box<str>]) {
        let mut text = format!("{}\n", tokens.len());
        for token in tokens {
            text.push_str(token);
            text.push('\n');
        }
        let temp = self
            .dir
            .join(format!(".tmp-{}-{}", std::process::id(), oid));
        let written = std::fs::create_dir_all(&self.dir)
            .and_then(|()| std::fs::write(&temp, text.as_bytes()))
            .and_then(|()| std::fs::rename(&temp, self.dir.join(oid.as_str())));
        if written.is_err() {
            let _ = std::fs::remove_file(&temp);
            return;
        }
        self.account(text.len() as u64);
    }

    /// Notes `added` bytes written and, past the cap, drops the least recently
    /// written sets down to nine tenths of it.
    #[allow(
        clippy::disallowed_methods,
        reason = "the token store is the tool's own cache directory; an entry whose metadata cannot be read is left out of the count"
    )]
    fn account(&self, added: u64) {
        let mut used = self
            .used
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let held = |dir: &PathBuf| -> Vec<(std::time::SystemTime, u64, PathBuf)> {
            std::fs::read_dir(dir)
                .into_iter()
                .flatten()
                .flatten()
                .filter_map(|entry| {
                    let meta = entry.metadata().ok()?;
                    Some((meta.modified().ok()?, meta.len(), entry.path()))
                })
                .collect()
        };
        let total = match *used {
            Some(total) => total + added,
            None => held(&self.dir).iter().map(|(_, len, _)| *len).sum(),
        };
        *used = Some(if total > CACHE_CAP_BYTES {
            let mut sets = held(&self.dir);
            sets.sort();
            let mut left: u64 = sets.iter().map(|(_, len, _)| *len).sum();
            for (_, len, path) in sets {
                if left <= CACHE_CAP_BYTES - CACHE_CAP_BYTES / 10 {
                    break;
                }
                if std::fs::remove_file(path).is_ok() {
                    left = left.saturating_sub(len);
                }
            }
            left
        } else {
            total
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        identifiers(text.as_bytes())
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn identifiers_are_raw_lowercased_and_distinct() {
        assert_eq!(words("A a; _x(a1) $Foo"), ["_x", "a", "a1", "foo"]);
        assert_eq!(words("a-b.c"), ["a", "b", "c"]);
        assert_eq!(words("x"), ["x"]);
    }

    #[test]
    fn a_name_is_spelled_by_all_of_its_identifiers() {
        let tokens = GroupTokens {
            files: vec![
                RepoPath::new("a.py").expect("path"),
                RepoPath::new("b.py").expect("path"),
                RepoPath::new("c.py").expect("path"),
            ],
            unread: vec![2],
            postings: HashMap::from([
                (Box::from("pod"), vec![0, 1]),
                (Box::from("labels"), vec![1]),
            ]),
        };
        let names = Names::new(["pod-labels"]);
        assert_eq!(tokens.spelling(&names), [1]);
        assert_eq!(tokens.spells("a.py", &names), Some(false));
        assert_eq!(tokens.spells("b.py", &names), Some(true));
        assert_eq!(tokens.spells("c.py", &names), None);
        assert_eq!(tokens.unread(), [2]);
        assert_eq!(tokens.spelling(&Names::new(["*"])), [0, 1]);
    }
}
