//! What locate and pack say about the change itself, in the words impact uses:
//! `range`, `inventory`, `coverage` and `fit` name one fact in every mode. Each
//! list is a [`Listing`], so what is omitted is a difference and never a second
//! counter, and a list with nothing in it is absent.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::v2::{Fit, Listing, Range, RangeKind};

#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, PartialEq, Eq)]
pub struct Rename {
    pub from: String,
    pub to: String,
}

/// The files the range touched, by what became of them.
#[derive(Serialize, Deserialize, JsonSchema, Clone, Debug, Default)]
pub struct Inventory {
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub changed: Listing<String>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub deleted: Listing<String>,
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub renamed: Listing<Rename>,
    /// Lock files touched by the range. Paths only: the raw hunks are thousands of
    /// tokens of checksum churn for one line of signal (#112).
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub lockfiles: Listing<String>,
    /// Changed files withheld by ignore rules. Silent exclusion misreads as "the
    /// diff did not touch this" (#188: a reviewer filed "no tests" against a change
    /// whose tests the tool had filtered).
    #[serde(default, skip_serializing_if = "Listing::is_empty")]
    pub ignored: Listing<String>,
    /// Files excluded by `.diffctx/ignore` or the secret-path policy: a count
    /// only, because the policy's point is that these paths stay out of the answer.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub withheld: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl Inventory {
    pub fn new(
        changed: Vec<String>,
        deleted: Vec<String>,
        renamed: Vec<(String, String)>,
        lockfiles: Vec<String>,
        ignored: Vec<String>,
        withheld: usize,
    ) -> Self {
        let all = |items: Vec<String>| Listing::of(items, 0);
        Self {
            changed: all(changed),
            deleted: all(deleted),
            renamed: Listing::of(
                renamed
                    .into_iter()
                    .map(|(from, to)| Rename { from, to })
                    .collect(),
                0,
            ),
            lockfiles: all(lockfiles),
            ignored: all(ignored),
            withheld,
        }
    }

    /// The range touched nothing a reader would want named: no file changed,
    /// deleted, renamed, locked, withheld or listed as ignored.
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty()
            && self.deleted.is_empty()
            && self.renamed.is_empty()
            && self.lockfiles.is_empty()
            && self.ignored.is_empty()
            && self.withheld == 0
    }

    /// Deletions, renames, lock files and withheld files are signal even when no
    /// fragment was selected: only a range carrying none of them is empty.
    pub fn says_something_without_fragments(&self) -> bool {
        !(self.deleted.is_empty()
            && self.renamed.is_empty()
            && self.lockfiles.is_empty()
            && self.ignored.is_empty()
            && self.withheld == 0)
    }
}

/// The range an answer is about, as the impact answer states it: what was asked
/// (`spec`), what kind of range that is, the staged tree when it is one, how many
/// commits it holds and the newest of their messages.
pub fn range_of(
    spec: Option<&str>,
    index_tree: Option<&str>,
    commits: usize,
    messages: Vec<String>,
) -> Range {
    Range {
        kind: super::v2::range_kind(spec, index_tree, false),
        spec: spec.map(str::to_string),
        index_tree: index_tree.map(str::to_string),
        commits,
        messages,
    }
}

/// A range that names no revision: the working tree.
pub fn worktree_range() -> Range {
    Range {
        kind: RangeKind::Worktree,
        spec: None,
        index_tree: None,
        commits: 0,
        messages: Vec::new(),
    }
}

/// The fit of an answer that was sized by a budget: what was asked and the cap the
/// run resolved it to.
pub fn fit_of(asked: Option<u32>, cap: u32) -> Fit {
    Fit {
        asked,
        cap,
        needs: 0,
        least: None,
        exempt: 0,
    }
}

/// The messages a range states: the commits' own, newest first, or, when the run
/// read none (`--full`), the head's subject, the one message it has.
pub fn messages_of(commit_message: Option<String>, commit_messages: Vec<String>) -> Vec<String> {
    if commit_messages.is_empty() {
        commit_message.into_iter().collect()
    } else {
        commit_messages
    }
}
