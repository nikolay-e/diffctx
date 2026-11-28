//! What a Python file's syntax declares: with the class and function they sit
//! in, the names a module or a class body assigns, which the fragmenter does not
//! cut. A name a module or a class body assigns (`TABLE: dict = {…}`, `valued_at:
//! Mapped[str] = …`) is declared there like a `def`, and read elsewhere.

use std::sync::{Arc, Mutex};

use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;

use super::{DeclKind, Declaration};
use crate::parsers::python_decls::{self, PyDeclKind};

#[derive(Debug, Default)]
pub(super) struct PythonFacts {
    pub(super) declarations: Vec<Declaration>,
}

const CACHE_LIMIT: usize = 8192;
type Key = (u64, usize);

static CACHE: Lazy<Mutex<FxHashMap<Key, Arc<PythonFacts>>>> =
    Lazy::new(|| Mutex::new(FxHashMap::default()));

fn key(content: &str) -> Key {
    use std::hash::{Hash, Hasher};
    let mut h = rustc_hash::FxHasher::default();
    content.hash(&mut h);
    (h.finish(), content.len())
}

/// The facts of one blob, parsed once per run whoever asks first. `None`
/// when the grammar is not compiled in or the parse timed out.
#[allow(
    clippy::disallowed_methods,
    reason = "a poisoned cache lock is a miss: the blob is parsed again"
)]
pub(super) fn facts(content: &str) -> Option<Arc<PythonFacts>> {
    let key = key(content);
    let hit = CACHE.lock().ok().and_then(|c| c.get(&key).cloned());
    if hit.is_some() {
        return hit;
    }
    let parsed = python_decls::facts(content)?;
    let declarations = parsed
        .iter()
        .map(|d| Declaration {
            name: d.name.clone(),
            kind: match d.kind {
                PyDeclKind::Function => DeclKind::Function,
                PyDeclKind::Class => DeclKind::Class,
                PyDeclKind::Assignment => DeclKind::Assignment,
            },
            lines: d.lines,
            class: d.class.clone(),
            scope: d.scope,
        })
        .collect();
    let facts = Arc::new(PythonFacts { declarations });
    if let Ok(mut cache) = CACHE.lock() {
        if cache.len() >= CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&facts));
    }
    Some(facts)
}
