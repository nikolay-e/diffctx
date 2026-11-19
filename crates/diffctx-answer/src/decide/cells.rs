//! The cells of a run: the spelled lines the token index reads for them, and how they merge.

use std::cell::OnceCell;

use std::collections::{BTreeMap, BTreeSet};

use std::sync::Arc;

use anyhow::Result;

use crate::run_provenance::state::ScoredState;

use diffctx_facts::facts::{DefKey, FactsCache, HeldAs};

use diffctx_occurrence::tokens::{GroupTokens, Names, TokenIndex};

use diffctx_resolve::resolve::Relation;

use diffctx_resolve::vocabulary::family::{Family, Group};

use diffctx_snapshot::resource::LimitReason;

use diffctx_snapshot::snapshot::{AnalysisContext, NotAnalysedReason, RepoPath};

use super::cell::{Cell, context};
use super::changed::{Changed, How};
use super::verdict::Reach;

/// The head a run with no cell at all reads the files of a group from.
struct Bare {
    ctx: AnalysisContext,
    cache: FactsCache,
}

/// What a search for spelled keys found, and the files it could not look in.
#[derive(Default)]
pub(crate) struct Spelled {
    /// (path, line, text) of each line that holds a needle.
    pub rows: Vec<(String, u32, String)>,
    /// Files past the size cap or unreadable: one of them may spell a needle.
    pub unread: Vec<String>,
}

/// The most a file of no language the token index groups may hold to be read whole
/// for a spelled key.
const UNGROUPED_BYTES: u64 = 1 << 20;

/// The cells of one run: one per family of languages the answer is about.
pub(crate) struct Cells {
    cells: Vec<Cell>,
    /// Families whose files could not be resolved: their definitions are
    /// undetermined, never answered by the paths the cells replaced.
    lost: Vec<Family>,
    /// Token to files, built once per run for every cell and every group.
    tokens: Arc<TokenIndex>,
    range: Option<String>,
    symbol: bool,
    bare: OnceCell<Option<Bare>>,
}

impl Cells {
    pub(crate) fn open(state: &ScoredState, range: Option<&str>, symbol: bool) -> Self {
        let tokens = Arc::new(TokenIndex::default());
        let mut cells = Vec::new();
        let mut lost = Vec::new();
        for family in Family::ALL
            .into_iter()
            .filter(|f| Cell::concerns(*f, state, symbol))
        {
            match Cell::open(family, state, range, symbol, Arc::clone(&tokens)) {
                Ok(cell) => {
                    for stop in cell.stops() {
                        state.run.note(stop);
                    }
                    cells.push(cell);
                }
                Err(e) => {
                    tracing::warn!(
                        "impact: the {} cell was not resolved ({e}); its definitions are undetermined",
                        family.name()
                    );
                    state.run.note(LimitReason::ReadFailed);
                    lost.push(family);
                }
            }
        }
        Self {
            cells,
            lost,
            tokens,
            range: range.map(str::to_string),
            symbol,
            bare: OnceCell::new(),
        }
    }

    /// The head of a run that opened no cell, read for the files of a group;
    /// `None` when it cannot be read.
    fn bare(&self, state: &ScoredState) -> Option<&Bare> {
        self.bare
            .get_or_init(|| {
                let Ok(ctx) = context(state, self.range.as_deref(), self.symbol) else {
                    return None;
                };
                let cache = FactsCache::user();
                if self.tokens.bind(&ctx, &cache).is_err() {
                    return None;
                }
                Some(Bare { ctx, cache })
            })
            .as_ref()
    }

    /// The files of `group` and what they spell, when the head can be read.
    pub(crate) fn group_tokens(
        &self,
        state: &ScoredState,
        group: Group,
    ) -> Option<Arc<GroupTokens>> {
        if self.cells.is_empty() {
            self.bare(state)?;
        }
        Some(self.tokens.group(group))
    }

    /// The lines of the head's files that hold one of `needles` (fixed strings),
    /// as (path, line, text): where a key or a value a config change touched is
    /// spelled back. The token index names the files that spell every identifier
    /// of a needle, whatever their language; the head's own bytes give the lines.
    /// A file of no language the index groups is read whole when it is small, and
    /// a file the index could not tokenise (past the size cap) is not searched.
    pub(crate) fn spelled_lines(&self, state: &ScoredState, needles: &[String]) -> Result<Spelled> {
        if self.cells.is_empty() && self.bare(state).is_none() {
            anyhow::bail!("the head snapshot cannot be read");
        }
        let (ctx, cache) = match (self.cells.first(), self.bare.get()) {
            (Some(cell), _) => (&cell.ctx, &cell.cache),
            (None, Some(Some(bare))) => (&bare.ctx, &bare.cache),
            _ => anyhow::bail!("the head snapshot cannot be read"),
        };
        let listing = cache.listing(&ctx.head)?;
        let wanted: Vec<Names> = needles
            .iter()
            .map(|needle| Names::new([needle.as_str()]))
            .collect();
        let mut groups: Vec<Group> = Vec::new();
        let mut candidates: BTreeMap<&str, &RepoPath> = BTreeMap::new();
        let mut unread: BTreeSet<String> = listing
            .not_analysed
            .iter()
            .filter(|(_, reason)| *reason == NotAnalysedReason::Oversize)
            .map(|(path, _)| path.as_str().to_string())
            .collect();
        for entry in &listing.entries {
            match Group::of_path(&entry.path) {
                Some(group) => {
                    if !groups.contains(&group) {
                        groups.push(group);
                    }
                }
                None if entry.size <= UNGROUPED_BYTES => {
                    candidates.insert(entry.path.as_str(), &entry.path);
                }
                None => {
                    unread.insert(entry.path.as_str().to_string());
                }
            }
        }
        for group in groups {
            let tokens = self.tokens.group(group);
            for at in tokens.unread() {
                if let Some(path) = tokens.files().get(*at as usize) {
                    if listing
                        .entries
                        .binary_search_by(|entry| entry.path.cmp(path))
                        .is_ok()
                    {
                        unread.insert(path.as_str().to_string());
                    }
                }
            }
            for names in &wanted {
                for at in tokens.spelling(names) {
                    if let Some(path) = tokens.files().get(at as usize) {
                        if let Ok(at) = listing
                            .entries
                            .binary_search_by(|entry| entry.path.cmp(path))
                        {
                            let held = &listing.entries[at].path;
                            candidates.insert(held.as_str(), held);
                        }
                    }
                }
            }
        }
        let mut rows: Vec<(String, u32, String)> = Vec::new();
        for (name, path) in candidates {
            if !state.run.check() {
                break;
            }
            let Some(bytes) = ctx.head.read(path)? else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            for (at, line) in text.lines().enumerate() {
                if needles.iter().any(|needle| line.contains(needle.as_str())) {
                    rows.push((name.to_string(), at as u32 + 1, line.to_string()));
                }
            }
        }
        rows.sort_unstable();
        rows.dedup();
        Ok(Spelled {
            rows,
            unread: unread.into_iter().collect(),
        })
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &Cell> {
        self.cells.iter()
    }

    /// The cell whose family's group holds `rel`, whether or not it has a
    /// definition or a use in it: the one that read the file or lost it.
    pub(crate) fn reading(&self, rel: &str) -> Option<&Cell> {
        self.cells
            .iter()
            .find(|cell| cell.family.group().has_path(rel))
    }

    /// The cell that answers for `rel`.
    pub(crate) fn of(&self, rel: &str) -> Option<&Cell> {
        self.cells.iter().find(|cell| cell.owns(rel))
    }

    /// What the tree of `rel` holds as a string on `line`, read by the cell that
    /// answers for it, else by any: every cell reads the same head.
    pub(crate) fn held_strings_at(&self, rel: &str, line: u32) -> Vec<(HeldAs, String)> {
        self.of(rel)
            .or_else(|| self.cells.first())
            .map_or_else(Vec::new, |cell| cell.held_strings_at(rel, line))
    }

    /// The head text of `rel` and the byte ranges of its string literals, read by
    /// the cell that answers for it, else by any.
    pub(crate) fn string_pieces(
        &self,
        rel: &str,
    ) -> anyhow::Result<Option<(String, Vec<std::ops::Range<u32>>)>> {
        self.of(rel)
            .or_else(|| self.cells.first())
            .map_or(Ok(None), |cell| cell.string_pieces(rel))
    }

    /// Whether a head file of a cell language may import a module by a name it
    /// computes (`importlib.import_module(name)`, `import(path)`, `require(path)`),
    /// which no importer search can see: some file holds a `ComputedImport` fact, or
    /// a family could not be read at all, so none is known to hold none.
    pub(crate) fn computed_imports_possible(&self) -> bool {
        !self.lost.is_empty() || self.cells.iter().any(Cell::computes_an_import)
    }

    /// Whether `rel` is a file of a family that could not be resolved.
    pub(crate) fn lost(&self, rel: &str) -> bool {
        self.lost.iter().any(|family| family.has_path(rel))
    }
}

/// Whether a line that names a definition the range removed is stranded by it: a
/// use that now finds nothing, or an export that still offers the missing name. In
/// the code families a name written where a type is expected, and an import of it,
/// are the only uses a removed interface, class or alias may have: a file that still
/// imports a removed type and annotates with it is broken, and without them the
/// search of a removed type finds nothing and the answer says nothing references it.
pub(super) fn strands(family: Family, relation: Relation) -> bool {
    relation.uses()
        || relation == Relation::Reexports
        || (matches!(relation, Relation::TypeUses | Relation::Imports)
            && !matches!(
                family,
                Family::Config | Family::Terraform | Family::Dockerfile | Family::Shell
            ))
}

/// Reports one definition once, with the subjects of every reason it is
/// reported for.
pub(super) fn merge(by_key: &mut BTreeMap<DefKey, Changed>, changed: Changed) {
    let Some(first) = changed.subjects.first() else {
        return;
    };
    match by_key.get_mut(&first.key) {
        Some(known) => {
            for subject in changed.subjects {
                match known.subjects.iter_mut().find(|s| s.key == subject.key) {
                    Some(seen) if subject.reach == Reach::Every => seen.reach = Reach::Every,
                    Some(_) => {}
                    None => known.subjects.push(subject),
                }
            }
            known.stale.extend(changed.stale);
            known.unpaired |= changed.unpaired;
            // Two reasons for one entry fold by neither: the table has no row
            // for the two together.
            if known.basis != changed.basis {
                known.basis = None;
            }
            known.not_layout |= changed.not_layout;
            known.api |= changed.api;
            known.delta = known.delta.or(changed.delta);
            known.origin = known.origin.or(changed.origin);
            known.how = match (known.how, changed.how) {
                (How::Edited { layout_only: a }, How::Edited { layout_only: b }) => How::Edited {
                    layout_only: a && b,
                },
                (How::Added, _) | (_, How::Added) => How::Added,
                (other, _) => other,
            };
        }
        None => {
            by_key.insert(first.key.clone(), changed);
        }
    }
}
