//! Test-path memoization, [`Run`] indexing by target file, and zero-allocation command matching.

use std::cell::RefCell;

use diffctx_base::testfiles::DeclaredTests;
use diffctx_base::types::Fragment;
use rustc_hash::FxHashMap;

use super::super::dispatch::base_symbol;
use super::Run;

/// Memoizes [`DeclaredTests::runs`] per path across the run's test searches.
pub(in crate::impact) struct TestFilter {
    declared: DeclaredTests,
    memo: RefCell<FxHashMap<String, bool>>,
}

impl TestFilter {
    pub(in crate::impact) fn new(declared: DeclaredTests) -> Self {
        Self {
            declared,
            memo: RefCell::default(),
        }
    }

    pub(in crate::impact) const fn inner(&self) -> &DeclaredTests {
        &self.declared
    }

    pub(in crate::impact) fn runs(&self, path: &str) -> bool {
        let known = self.memo.borrow().get(path).copied();
        if let Some(known) = known {
            return known;
        }
        let runs = self.declared.runs(path);
        self.memo.borrow_mut().insert(path.to_string(), runs);
        runs
    }
}

/// [`Run`]s indexed by target file as fragments spell it, preserving `runs` order.
pub(in crate::impact) struct RunIndex {
    runs: Vec<Run>,
    by_file: FxHashMap<String, Vec<usize>>,
}

impl RunIndex {
    pub(in crate::impact) fn new(runs: Vec<Run>) -> Self {
        let mut by_file: FxHashMap<String, Vec<usize>> = FxHashMap::default();
        for (i, run) in runs.iter().enumerate() {
            by_file.entry(run.file.clone()).or_default().push(i);
        }
        Self { runs, by_file }
    }

    pub(in crate::impact) fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    pub(in crate::impact) fn entering<'r>(
        &'r self,
        f: &'r Fragment,
    ) -> impl Iterator<Item = &'r Run> + 'r {
        let sym = f.symbol_name.as_deref().map(base_symbol);
        self.by_file
            .get(f.path())
            .into_iter()
            .flatten()
            .map(move |&i| &self.runs[i])
            .filter(move |run| run.symbol.as_deref().is_none_or(|s| sym == Some(s)))
    }
}

/// The name as one quoted word, or the first word of a quoted command:
/// `["mytool", "--flag"]`, `'mytool --flag'`.
pub(in crate::impact) fn spelled_as_command(text: &str, name: &str) -> bool {
    if !name.is_empty() && !name.contains(['"', '\'']) {
        let bytes = text.as_bytes();
        return text.match_indices(name).any(|(at, _)| {
            at > 0
                && matches!(bytes[at - 1], b'"' | b'\'')
                && matches!(
                    bytes.get(at + name.len()).copied(),
                    Some(q) if q == bytes[at - 1] || q == b' '
                )
        });
    }
    ['"', '\'']
        .into_iter()
        .any(|q| text.contains(&format!("{q}{name}{q}")) || text.contains(&format!("{q}{name} ")))
}
