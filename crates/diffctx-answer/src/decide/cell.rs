//! The cell: what the resolver and the token index say about one changed definition and its uses, with
//! the modules that import it.

use std::cell::{OnceCell, RefCell};

use std::collections::{BTreeSet, HashMap, HashSet};

use std::rc::Rc;

use std::sync::Arc;

use anyhow::Result;

use crate::run_provenance::state::ScoredState;

use diffctx_facts::facts::{
    DefKey, DefKind, FactsCache, FileFacts, Language, ModulePath, ProjectId, Scope,
};

use diffctx_occurrence::tokens::occurrence::debug::Written;

use diffctx_occurrence::tokens::occurrence::{Index, Standing};

use diffctx_occurrence::tokens::{Names, TokenIndex};

use diffctx_resolve::resolve::{Binding, CandidateReason, EvidenceKind, Relation, Resolution};

use diffctx_resolve::vocabulary::family::{Family, Group, Resolver, context_in};

use diffctx_snapshot::resource::LimitReason;

use diffctx_snapshot::snapshot::{AnalysisContext, ChangeKind, Loss, RepoPath, SideTag};

use super::changed::{Changed, How, Hunks};
use super::gap::{Gap, Reason};
use super::losses::{Losses, stopped_by};
use super::verdict::{Accounting, Subject, Use};

mod account;
mod changes;
mod construction;
mod reach;
mod search;

/// The reasons a candidate row carries in the answer.
const UNTYPED_RECEIVER: &str = "receiver_unresolved";

const AMBIGUOUS_MODULE: &str = "ambiguous_module";

const IMPORT_UNRESOLVED: &str = "import_unresolved";

const IMPORT_GLOB: &str = "import_glob";

const GLOBAL: &str = "global";

const OVERLOADED: &str = "overloaded";

const DISPATCH: &str = "dispatch";

const MACRO: &str = "macro";

const ADL: &str = "adl";

const ANY_NAMESPACE: &str = "any_namespace";

const NAME_SUFFIX: &str = "name_suffix";

const LABEL_SELECTOR: &str = "label_selector";

const NO_RELEASE: &str = "no_release";

const CASE_FOLD: &str = "case_fold";

const SEARCH_PATH: &str = "search_path";

const REBOUND: &str = "rebound";

/// The most lines a removed name is listed at where nothing says they were its uses.
const MAX_UNPLACED_LISTED: usize = 500;

pub(crate) struct Cell {
    pub(super) family: Family,
    pub(super) ctx: AnalysisContext,
    pub(super) cache: FactsCache,
    head: Resolver,
    base: OnceCell<Result<Resolver, String>>,
    changed: Vec<Changed>,
    /// Every key the range changed outside layout, by its head key.
    changed_keys: HashSet<DefKey>,
    hunks: Hunks,
    /// The lines each file gained or rewrote, by number, outside formatter-only
    /// hunks.
    added: HashMap<String, Vec<(u32, String)>>,
    /// The project, module and language of each file that holds a definition.
    homes: HashMap<String, (ProjectId, ModulePath, Language)>,
    /// Definitions by file: indices into the head's definitions.
    defs_by_file: HashMap<String, Vec<usize>>,
    /// Definitions by key: indices into the head's definitions.
    defs_by_key: HashMap<DefKey, usize>,
    /// Head bindings by file.
    bindings_by_file: HashMap<String, Vec<usize>>,
    /// Files the resolver read.
    files: HashSet<String>,
    /// Files outside the run's pathspecs that hold uses the cell left out.
    outside: HashSet<String>,
    /// The (user file, used file) pairs a binding of those links.
    reached_from_outside: HashSet<(String, String)>,
    /// Rust files that invoke `cfg_if!`.
    macro_files: HashSet<String>,
    /// Python files that import a library which executes annotations at run time.
    runtime_files: RefCell<HashMap<String, bool>>,
    /// Resolved uses by target: the file and line of each.
    landed: HashMap<DefKey, Vec<(String, u32)>>,
    /// (user file, used file) pairs a binding links.
    reaches: HashSet<(String, String)>,
    losses: Vec<Loss>,
    /// The run's pathspecs: the cell reads the files inside them alone, as the
    /// fragments do. Empty for the whole repository.
    scope: Vec<String>,
    /// What every cell of the run asks whether a file spells a name.
    tokens: Arc<TokenIndex>,
    /// The paths the head resolver read.
    read: HashSet<String>,
    /// Of those, the ones no extension puts in the family's group.
    outside_group: Vec<String>,
    /// What the run lost that does not change while the cell answers: its own
    /// losses, the manifests it could not read, the files its listing set aside.
    /// `None` when the listing cannot be had again.
    fixed: OnceCell<Option<Losses>>,
    /// The head's files the resolver read, for the shadow accounting.
    shadow_files: OnceCell<Vec<FileFacts>>,
    /// What the shadow wrote already: `verdict` runs from several places.
    shadow_written: RefCell<Written>,
    /// The head's bindings indexed for the occurrence account, once per cell.
    index: OnceCell<Rc<Index>>,
    /// The occurrence account of each changed definition, once per cell: the
    /// answer asks for a definition's verdict from several places.
    accounted: RefCell<HashMap<DefKey, Rc<Accounting>>>,
}

/// The context the cell reads: the range the run analysed, or one snapshot of
/// the checkout for a symbol.
pub(super) fn context(
    state: &ScoredState,
    range: Option<&str>,
    symbol: bool,
) -> Result<AnalysisContext> {
    context_in(&state.root_dir, state.run.io_limits(), range, symbol)
}

/// An import that happens when the code runs, `lazy(() => import('./Card'))`:
/// the module is loaded for what it offers, so the line is a use of that.
fn loads_lazily(b: &Binding) -> bool {
    b.relation == Relation::Imports
        && matches!(&b.resolution, Resolution::Resolved { evidence, .. }
            if evidence.kind == EvidenceKind::DynamicImport)
}

/// Whether `file` is inside the run's pathspecs (all of the repository when
/// there are none).
fn within_scope(scope: &[String], file: &str) -> bool {
    scope.is_empty()
        || scope.iter().any(|p| {
            file == p
                || file
                    .strip_prefix(p.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
}

pub(super) fn within(line: u32, (start, end): (u32, u32)) -> bool {
    start <= line && line <= end
}

fn root_scope(scope: &Scope) -> &Scope {
    match scope {
        Scope::Callback { parent, .. } => root_scope(parent),
        other => other,
    }
}

fn standing_of(resolution: &Resolution) -> Option<Standing> {
    match resolution {
        Resolution::Resolved { .. } => Some(Standing::Resolved),
        Resolution::Candidate { reason, .. } => Some(Standing::Candidate(match reason {
            CandidateReason::UntypedReceiver | CandidateReason::AnyReceiver => UNTYPED_RECEIVER,
            CandidateReason::AmbiguousModule => AMBIGUOUS_MODULE,
            CandidateReason::ImportUnresolved => IMPORT_UNRESOLVED,
            CandidateReason::ImportGlob => IMPORT_GLOB,
            CandidateReason::Global => GLOBAL,
            CandidateReason::Overloaded => OVERLOADED,
            CandidateReason::Dispatch => DISPATCH,
            CandidateReason::Macro => MACRO,
            CandidateReason::Adl => ADL,
            CandidateReason::AnyNamespace => ANY_NAMESPACE,
            CandidateReason::NameSuffix => NAME_SUFFIX,
            CandidateReason::LabelSelector => LABEL_SELECTOR,
            CandidateReason::NoRelease => NO_RELEASE,
            CandidateReason::CaseFold => CASE_FOLD,
            CandidateReason::SearchPath => SEARCH_PATH,
        })),
        Resolution::Module { .. } | Resolution::Unresolved { .. } => None,
    }
}

/// A module a `--symbol` name names and the imports that land on it.
pub(crate) struct ModuleOf {
    pub file: String,
    pub lines: usize,
    pub importers: Vec<Importer>,
}

pub(crate) struct Importer {
    pub file: String,
    pub line: u32,
    pub inside: Option<String>,
}

/// The modules of the working tree `name` names, each with the imports the
/// resolver landed on it. A module is no definition of the facts layer, so a
/// name that defines nothing is looked for here.
pub(crate) fn modules_named(
    root: &std::path::Path,
    limits: (
        Option<std::time::Instant>,
        Arc<std::sync::atomic::AtomicBool>,
    ),
    name: &str,
) -> Result<Vec<ModuleOf>> {
    let ctx = context_in(root, limits, None, true)?;
    let cache = FactsCache::user();
    let resolver = Resolver::open(Family::Python, &ctx, SideTag::Head, &cache)?;
    let modules = resolver.modules_named(name);
    modules
        .iter()
        .map(|module| {
            let lines = ctx.head.read(module)?.map_or(1, |bytes| {
                String::from_utf8_lossy(&bytes).lines().count().max(1)
            });
            Ok(ModuleOf {
                file: module.as_str().to_string(),
                lines,
                importers: resolver
                    .bindings()
                    .iter()
                    .filter(|b| b.relation == Relation::Imports)
                    .filter(|b| {
                        matches!(&b.resolution, Resolution::Module { file, evidence }
                        if file == module && evidence.kind != EvidenceKind::AmbiguousModule)
                    })
                    .map(|b| Importer {
                        file: b.file.as_str().to_string(),
                        line: b.site.lines.0,
                        inside: match &b.from {
                            Scope::Def(local) => Some(local.name.as_str().to_string()),
                            _ => None,
                        },
                    })
                    .collect(),
            })
        })
        .collect()
}

impl Cell {
    /// Whether any file the answer is about is of the family: a changed file
    /// with its extension, or none and its shebang, or a queried name in one.
    pub(super) fn concerns(family: Family, state: &ScoredState, symbol: bool) -> bool {
        let source = state.run.source();
        let python = |path: &str| {
            let Ok(p) = RepoPath::new(path) else {
                return false;
            };
            match Language::from_path(&p) {
                Some(language) => family.has(language),
                None => {
                    Language::needs_shebang(&p)
                        && source
                            .read_bytes(&state.root_dir.join(path))
                            .is_some_and(|bytes| {
                                Language::from_shebang(&bytes).is_some_and(|l| family.has(l))
                            })
                }
            }
        };
        let rel = |p: &std::path::Path| diffctx_base::paths::display_rel_or_abs(&state.root_dir, p);
        if symbol {
            return state
                .all_fragments
                .iter()
                .any(|f| python(&rel(std::path::Path::new(f.path()))));
        }
        state
            .changed_files
            .iter()
            .map(|p| rel(p))
            .chain(state.deleted_files.iter().cloned())
            .chain(state.renamed_files.iter().map(|(old, _)| old.clone()))
            .any(|p| python(&p))
    }

    /// Resolves the family's files of the snapshot the run reads and, for a
    /// range, reads what it changed.
    pub(super) fn open(
        family: Family,
        state: &ScoredState,
        range: Option<&str>,
        symbol: bool,
        tokens: Arc<TokenIndex>,
    ) -> Result<Self> {
        let ctx = context(state, range, symbol)?;
        let cache = FactsCache::user();
        let head = Resolver::open(family, &ctx, SideTag::Head, &cache)?;
        tokens.bind(&ctx, &cache)?;
        let read: HashSet<String> = head
            .read_paths()
            .iter()
            .map(|path| path.as_str().to_string())
            .collect();
        let group = family.group();
        let mut outside_group: Vec<String> = read
            .iter()
            .filter(|path| !group.has_path(path))
            .cloned()
            .collect();
        outside_group.sort();
        let mut cell = Self {
            family,
            ctx,
            cache,
            head,
            base: OnceCell::new(),
            changed: Vec::new(),
            changed_keys: HashSet::new(),
            hunks: HashMap::new(),
            added: HashMap::new(),
            homes: HashMap::new(),
            defs_by_file: HashMap::new(),
            defs_by_key: HashMap::new(),
            bindings_by_file: HashMap::new(),
            files: HashSet::new(),
            outside: HashSet::new(),
            reached_from_outside: HashSet::new(),
            macro_files: HashSet::new(),
            runtime_files: RefCell::new(HashMap::new()),
            landed: HashMap::new(),
            reaches: HashSet::new(),
            losses: Vec::new(),
            scope: state.scope.clone(),
            tokens,
            read,
            outside_group,
            fixed: OnceCell::new(),
            shadow_files: OnceCell::new(),
            shadow_written: RefCell::default(),
            index: OnceCell::new(),
            accounted: RefCell::default(),
        };
        // A queried name has no diff: the lines a symbol run anchors its
        // definitions on are no hunks, and a use inside one is a caller.
        for h in state.hunk_texts.iter().filter(|_| !symbol) {
            cell.hunks
                .entry(h.path.clone())
                .or_default()
                .push(h.new_range());
            if !h.layout {
                cell.added
                    .entry(h.path.clone())
                    .or_default()
                    .extend((h.new_start..).zip(h.added.iter().cloned()));
            }
        }
        cell.index();
        if !symbol {
            cell.read_changes()?;
        }
        if let Some(loss) = cell.injected_loss() {
            cell.losses.push(loss);
        }
        Ok(cell)
    }

    /// The loss the test knob `DIFFCTX_TEST_LOSS` puts on the n-th file, by path,
    /// of the cell's group.
    fn injected_loss(&self) -> Option<Loss> {
        let (kind, at) = diffctx_snapshot::resource::test_loss()?;
        let group = self.family.group();
        let Ok(listing) = self.cache.listing(&self.ctx.head) else {
            return None;
        };
        let path = listing
            .entries
            .iter()
            .map(|entry| &entry.path)
            .filter(|path| group.has_path(path.as_str()))
            .nth(at)?;
        Loss::samples(path)
            .into_iter()
            .find(|loss| loss.kind() == kind.as_str())
    }

    fn index(&mut self) {
        let mut file_of: HashMap<&DefKey, &str> = HashMap::new();
        for (at, def) in self.head.definitions().iter().enumerate() {
            let file = def.file.as_str();
            self.files.insert(file.to_string());
            self.homes.entry(file.to_string()).or_insert_with(|| {
                (
                    def.key.project.clone(),
                    def.key.module.clone(),
                    def.key.language,
                )
            });
            self.defs_by_file
                .entry(file.to_string())
                .or_default()
                .push(at);
            file_of.insert(&def.key, file);
            self.defs_by_key.entry(def.key.clone()).or_insert(at);
        }
        for (at, b) in self.head.bindings().iter().enumerate() {
            let file = b.file.as_str();
            let used = match &b.resolution {
                Resolution::Resolved { target, .. } => file_of.get(target).copied(),
                Resolution::Module { file: used, .. } => Some(used.as_str()),
                Resolution::Candidate { .. } | Resolution::Unresolved { .. } => None,
            };
            let reach = used.map(|used| (file.to_string(), used.to_string()));
            if !within_scope(&self.scope, file) {
                self.outside.insert(file.to_string());
                self.reached_from_outside.extend(reach);
                continue;
            }
            // A configuration file that only names places (a Kustomize file) is
            // not the cell's: it holds no object a use could land on. A JVM cell
            // reads the neighbouring language's files for their uses of its own
            // definitions, and answers for none of them.
            if !(self.family == Family::Config && b.relation == Relation::Imports)
                && (self.family.group() != Group::Jvm || self.family.has_path(file))
            {
                self.files.insert(file.to_string());
            }
            if (self.family == Family::Rust
                && b.relation == Relation::Calls
                && b.name.as_str() == "cfg_if")
                || (self.family == Family::C
                    && matches!(
                        b.resolution,
                        Resolution::Candidate {
                            reason: CandidateReason::Macro,
                            ..
                        }
                    ))
            {
                self.macro_files.insert(file.to_string());
            }
            self.bindings_by_file
                .entry(file.to_string())
                .or_default()
                .push(at);
            let line = b.site.lines.0;
            // A Rust or TypeScript test reaches a type through the function that
            // names it in a signature or a type argument (`size_of::<T>()`).
            if let Resolution::Resolved { target, .. } = &b.resolution {
                if b.relation.uses()
                    || (matches!(self.family, Family::Rust | Family::TypeScript)
                        && b.relation == Relation::TypeUses)
                {
                    self.landed
                        .entry(target.clone())
                        .or_default()
                        .push((file.to_string(), line));
                }
            }
            self.reaches.extend(reach);
        }
        self.losses = self.head.losses().to_vec();
    }

    /// The resolver of the base side, built when a removed or moved definition
    /// asks who still names it. A side that could not be resolved is the reason
    /// the question went unasked, not an empty answer to it.
    fn base(&self) -> Result<&Resolver, Gap> {
        self.base
            .get_or_init(|| {
                Resolver::open(self.family, &self.ctx, SideTag::Base, &self.cache)
                    .map_err(|e| format!("{e:#}"))
            })
            .as_ref()
            .map_err(|reason| Gap::BaseUnresolved {
                reason: reason.clone(),
            })
    }

    pub(crate) fn changed(&self) -> &[Changed] {
        &self.changed
    }

    /// What the cell could not read, for the run's limits: a read that failed or
    /// a deadline. Files it read in part are said by the answer's own
    /// unanalysed list.
    pub(super) fn stops(&self) -> Vec<LimitReason> {
        stopped_by(&self.losses)
    }

    /// The runs of changed lines in the family's files that no tracked definition
    /// holds and that are code, not a blank line, a comment or an import: a
    /// module-level statement, the body of an `if __name__ == "__main__":`. The
    /// cell traces definitions, so what these lines change is not traced, and the
    /// answer says so. A file the range adds has no callers to lose; a test
    /// file's top-level code is still named, never read as a change of nothing.
    pub(crate) fn outside_definitions(&self) -> Vec<(String, Vec<(u32, u32)>)> {
        let added_files: HashSet<&str> = self
            .ctx
            .changes
            .changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Added)
            .map(|c| c.path.as_str())
            .collect();
        let mut files: Vec<&String> = self.added.keys().collect();
        files.sort();
        let mut out = Vec::new();
        for file in files {
            if !self.family.has_path(file)
                || !within_scope(&self.scope, file)
                || added_files.contains(file.as_str())
            {
                continue;
            }
            let held: Vec<(u32, u32)> = self.defs_in(file).map(|d| d.span.lines).collect();
            let imported: Vec<(u32, u32)> = self
                .bindings_by_file
                .get(file)
                .into_iter()
                .flatten()
                .map(|at| &self.head.bindings()[*at])
                .filter(|b| matches!(b.relation, Relation::Imports | Relation::Reexports))
                .map(|b| b.site.lines)
                .collect();
            let mut spans: Vec<(u32, u32)> = Vec::new();
            for (line, text) in &self.added[file] {
                let text = text.trim_start();
                let code = text.chars().any(char::is_alphanumeric)
                    && !["#", "//", "/*", "*", "@"]
                        .iter()
                        .any(|c| text.starts_with(c))
                    && !held.iter().any(|s| within(*line, *s))
                    && !imported.iter().any(|s| within(*line, *s));
                if !code {
                    continue;
                }
                match spans.last_mut() {
                    Some(last) if last.1 + 1 == *line => last.1 = *line,
                    _ => spans.push((*line, *line)),
                }
            }
            if !spans.is_empty() {
                out.push((file.clone(), spans));
            }
        }
        out
    }

    /// Whether the cell answers for `rel`: a file of its family in the snapshot.
    pub(crate) fn owns(&self, rel: &str) -> bool {
        self.files.contains(rel) || self.family.has_path(rel)
    }

    /// Whether what consumes the family's files lies outside the code the cell
    /// reads: a CI workflow, a compose file or a script runs a Dockerfile, and
    /// a tool applies a configuration. The cell traces the relations inside and
    /// between the files; their consumers are reached only by a mention.
    pub(crate) fn consumed_outside(&self) -> bool {
        matches!(self.family, Family::Config | Family::Dockerfile)
    }

    pub(crate) const fn names_are_global(&self) -> bool {
        self.family.names_are_global()
    }

    /// The files other than `gone` whose base bindings landed in the module `file`,
    /// an import of it or a resolved use of what it defined: who still depends on a module
    /// the range deletes. A Go package is one module, so a file of the directory
    /// stands for all of it. `None` when the base could not be read.
    pub(crate) fn importers_of(&self, file: &str, gone: &[String]) -> Option<Vec<String>> {
        let Ok(base) = self.base() else {
            return None;
        };
        let dir = |path: &str| path.rsplit_once('/').map(|(dir, _)| dir.to_string());
        let go = self.family == Family::Go;
        let unit = |path: &str| path == file || (go && dir(path) == dir(file));
        let defined: HashSet<&DefKey> = base
            .definitions()
            .iter()
            .filter(|d| unit(d.file.as_str()))
            .map(|d| &d.key)
            .collect();
        let found: BTreeSet<&str> = base
            .bindings()
            .iter()
            .filter(|b| !unit(b.file.as_str()) && !gone.iter().any(|g| g == b.file.as_str()))
            .filter(|b| match &b.resolution {
                Resolution::Module { file, .. } => unit(file.as_str()),
                Resolution::Resolved { target, .. } => defined.contains(target),
                // A name that any same-named member might answer binds no file.
                Resolution::Candidate { .. } | Resolution::Unresolved { .. } => false,
            })
            .map(|b| b.file.as_str())
            .collect();
        Some(found.into_iter().map(str::to_string).collect())
    }

    /// Whether the cell read the file without a loss: its definitions and uses
    /// are the cell's. A file the grammar read only in part, or lost any other
    /// way, has uses the cell never saw.
    pub(crate) fn is_bound(&self, rel: &str) -> bool {
        within_scope(&self.scope, rel) && self.read.contains(rel) && self.loss_about(rel).is_none()
    }

    /// The files of the cell's group that spell `name` (or could not be tokenised)
    /// and were lost, with why: where a name asked for may be defined though no
    /// definition of it was read.
    pub(crate) fn lost_namers(&self, name: &str) -> Vec<(RepoPath, Reason)> {
        let tokens = self.tokens.group(self.family.group());
        let mut at = tokens.spelling(&Names::new([name]));
        at.extend_from_slice(tokens.unread());
        at.sort_unstable();
        at.into_iter()
            .map(|at| &tokens.files()[at as usize])
            .filter_map(|file| {
                self.loss_about(file.as_str())
                    .map(|reason| (file.clone(), reason))
            })
            .collect()
    }

    /// Why a loss leaves `rel` unread, when one does. A listing that cannot be had
    /// again leaves every file lost.
    pub(crate) fn loss_about(&self, rel: &str) -> Option<Reason> {
        let Some(fixed) = self.fixed_losses() else {
            return Some(Reason::NotRead);
        };
        let reason = |(reason, _): (Reason, &Loss)| reason;
        fixed
            .about(rel)
            .map(reason)
            .or_else(|| self.moving_losses().about(rel).map(reason))
    }

    /// What the run lost that stays what it is while the cell answers.
    fn fixed_losses(&self) -> Option<&Losses> {
        self.fixed
            .get_or_init(|| {
                let Ok(listing) = self.cache.listing(&self.ctx.head) else {
                    return None;
                };
                Some(Losses::of(
                    self.losses
                        .iter()
                        .chain(self.ctx.workspace.losses())
                        .chain(listing.losses().iter()),
                ))
            })
            .as_ref()
    }

    /// What the run lost that grows as it reads: the reads of either snapshot
    /// that failed.
    fn moving_losses(&self) -> Losses {
        let reads = self.ctx.head.losses();
        let before = self.ctx.base.losses();
        Losses::of(reads.iter().chain(before.iter()))
    }

    /// The definition of a queried name or a fragment at these lines, as the
    /// cell would report it.
    pub(crate) fn define(&self, file: &str, name: &str, lines: (u32, u32)) -> Vec<Changed> {
        let Some(def) = self.definition_at(file, name, lines) else {
            return Vec::new();
        };
        self.report_as(
            def,
            How::Edited { layout_only: false },
            Vec::new(),
            false,
            false,
        )
    }

    /// The head's definitions in `file`, in the resolver's order.
    fn defs_in(&self, file: &str) -> impl Iterator<Item = &diffctx_resolve::resolve::Definition> {
        let at = self.defs_by_file.get(file).into_iter().flatten();
        at.map(|at| &self.head.definitions()[*at])
    }

    /// The definition called `name` whose span is nearest `lines` in `file`.
    fn definition_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Option<&diffctx_resolve::resolve::Definition> {
        self.defs_in(file)
            .filter(|d| {
                d.key.local.name.as_str() == name
                    || (d.key.language == Language::Sql
                        && d.key.local.name.as_str().eq_ignore_ascii_case(name))
            })
            .filter(|d| d.span.lines.0 <= lines.1 && lines.0 <= d.span.lines.1)
            .min_by_key(|d| d.span.lines.0.abs_diff(lines.0) + d.span.lines.1.abs_diff(lines.1))
    }

    fn key_in(&self, file: &str, scope: &Scope) -> Option<DefKey> {
        let Scope::Def(local) = root_scope(scope) else {
            return None;
        };
        let (project, module, language) = self.homes.get(file)?;
        Some(DefKey {
            project: project.clone(),
            language: *language,
            module: module.clone(),
            local: local.clone(),
        })
    }

    /// A definition's use is in the diff when the range changed it or wrote the use's line: an
    /// added impl header names its untouched self type (fj#260). A reformatted line does not.
    fn in_diff(&self, file: &str, line: u32, scope: &Scope) -> bool {
        match root_scope(scope) {
            Scope::Def(_) => {
                self.key_in(file, scope)
                    .is_some_and(|key| self.changed_keys.contains(&key))
                    || (self.added.get(file))
                        .is_some_and(|lines| lines.iter().any(|(l, _)| *l == line))
            }
            _ => self
                .hunks
                .get(file)
                .is_some_and(|ranges| ranges.iter().any(|r| within(line, *r))),
        }
    }

    fn use_of(&self, b: &Binding) -> Option<Use> {
        let standing = standing_of(&b.resolution)?;
        let file = b.file.as_str().to_string();
        let line = b.site.lines.0;
        Some(Use {
            also_changed: self.in_diff(&file, line, &b.from),
            file,
            line,
            site: b.site.bytes.clone(),
            relation: b.relation,
            standing,
            scope: b.from.clone(),
            folded: None,
            sharing: b.targets().len(),
        })
    }

    /// Whether a use under the `__main__` guard of the definition's own file is
    /// the script running its entry function: the definition is a function of the
    /// module itself, called by name.
    fn runs_as_entry(&self, changed: &Changed, subject: &Subject, b: &Binding) -> bool {
        b.file.as_str() == changed.file
            && self
                .definition(&subject.key)
                .is_some_and(|d| d.key.local.container.is_empty() && d.kind == DefKind::Function)
    }

    /// `export { a } from './m'` and `from .m import a as a` in a package's
    /// `__init__` offer a changed definition on: a module that re-exports it is
    /// where it reaches the package's users.
    fn reexports(&self, b: &Binding) -> bool {
        matches!(self.family, Family::TypeScript | Family::Python)
            && b.relation == Relation::Reexports
    }

    /// An annotation or a base class: a changed type is read by every signature,
    /// declaration and subclass that states it, which is how the language's own
    /// indexers count it (`reads`).
    fn names_type(&self, b: &Binding) -> bool {
        b.relation == Relation::TypeUses
    }

    /// The files outside the run's pathspecs that could name the definition: those
    /// holding a binding the cell left out, and those of its group that spell one
    /// of its names or could not be tokenised, whether or not the resolver read a
    /// binding in them. A file of a project that declares no dependency on the
    /// definition's still counts when a binding of its reaches the definition's
    /// file: a relative import crosses the manifests.
    fn outside_scope(&self, changed: &Changed) -> usize {
        if self.scope.is_empty() {
            return 0;
        }
        let group = self.tokens.group(self.family.group());
        let names = self.reference_names(changed);
        let mut spelled = group.spelling(&Names::new(names.iter().map(String::as_str)));
        spelled.extend_from_slice(group.unread());
        let unbound = (spelled.into_iter())
            .map(|at| group.files()[at as usize].as_str())
            .filter(|file| !within_scope(&self.scope, file));
        let files: HashSet<&str> = self
            .outside
            .iter()
            .map(String::as_str)
            .chain(unbound)
            .collect();
        (files.into_iter())
            .filter(|file| {
                self.ctx.workspace.may_reference(file, &changed.file)
                    || (self.reached_from_outside)
                        .contains(&(file.to_string(), changed.file.clone()))
            })
            .count()
    }

    /// The interface a class outside the repository could implement to reach a Java
    /// method nothing calls: the head resolver names it for each public method.
    fn open_world_via(&self, changed: &Changed, uses: &[Use]) -> Option<String> {
        let Resolver::Java(head) = &self.head else {
            return None;
        };
        if !uses.is_empty() {
            return None;
        }
        changed
            .subjects
            .iter()
            .find_map(|subject| head.open_route(&subject.key))
            .map(str::to_string)
    }
}
