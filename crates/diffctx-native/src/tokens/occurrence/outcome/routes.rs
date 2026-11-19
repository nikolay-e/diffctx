//! Whether an object of the subject's class can come to a file by an import: the witness an
//! untyped member read is no use of the subject, read off the side's import bindings and the
//! file's facts.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::{Claim, Reading, encloses};
use crate::facts::{DefKey, Fact, FileFacts, Name, Receiver, Scope, UseKind};
use crate::resolve::{Binding, Definition, Relation, Resolution, UnresolvedReason};
use crate::snapshot::RepoPath;
use crate::snapshot::workspace::{Ecosystem, WorkspaceModel};
use crate::testfiles::is_test_path;
use crate::tokens::occurrence::rows::{CLASSIC_PAGES, DESERIALIZERS, ISOLATED_MODULES, RUNS_CODE};
use crate::tokens::occurrence::{Reason, Witness};

/// The files each file of a family that isolates modules imports, as its import bindings land.
#[derive(Default)]
pub(super) struct Routes {
    imports: HashMap<RepoPath, HashSet<RepoPath>>,
    importers: HashMap<RepoPath, HashSet<RepoPath>>,
    /// The names such a file spells that nothing binds (an import taken out leaves them so).
    unbound: HashMap<RepoPath, HashSet<Name>>,
    /// The names the top level of such a file defines.
    defined: HashMap<RepoPath, HashSet<Name>>,
    /// Such files a named project publishes: whoever installs it imports them.
    published: HashSet<RepoPath>,
    /// An import of such a file landed on no place the bindings name: an edge may be missing.
    open: bool,
    /// Whether no route brings the subject's file to the file, by (file, subject's file).
    cut: RefCell<HashMap<(RepoPath, RepoPath), bool>>,
}

impl Routes {
    pub(super) fn new(
        files: &[FileFacts],
        bindings: &[Binding],
        definitions: &[Definition],
        workspace: &WorkspaceModel,
    ) -> Self {
        let isolated: HashSet<&RepoPath> = (files.iter())
            .filter(|file| ISOLATED_MODULES.iter().any(|f| f.has(file.language())))
            .map(|file| &file.path)
            .collect();
        let home: HashMap<&DefKey, &RepoPath> = definitions
            .iter()
            .map(|def| (&def.key, &def.file))
            .collect();
        let mut routes = Self {
            published: (isolated.iter())
                .filter(|path| published(workspace, path.as_str()))
                .map(|path| (*path).clone())
                .collect(),
            ..Self::default()
        };
        let imports = bindings.iter().filter(|binding| {
            matches!(binding.relation, Relation::Imports | Relation::Reexports)
                && isolated.contains(&binding.file)
        });
        for binding in imports {
            let landed = match &binding.resolution {
                Resolution::Module { file, .. } => Some(file),
                Resolution::Resolved { target, .. } => home.get(target).copied(),
                Resolution::Unresolved { reason }
                    if !matches!(
                        reason,
                        UnresolvedReason::NoBinding | UnresolvedReason::Opaque
                    ) =>
                {
                    continue;
                }
                Resolution::Unresolved { .. } | Resolution::Candidate { .. } => None,
            };
            let Some(file) = landed else {
                routes.open = true;
                continue;
            };
            let (from, to) = (binding.file.clone(), file.clone());
            routes
                .importers
                .entry(to.clone())
                .or_default()
                .insert(from.clone());
            routes.imports.entry(from).or_default().insert(to);
        }
        let spelled = bindings.iter().filter(|binding| {
            isolated.contains(&binding.file)
                && !matches!(binding.relation, Relation::Imports | Relation::Reexports)
                && matches!(
                    binding.resolution,
                    Resolution::Unresolved {
                        reason: UnresolvedReason::NoBinding
                    }
                )
        });
        for binding in spelled {
            (routes.unbound.entry(binding.file.clone()).or_default()).insert(binding.name.clone());
        }
        for def in definitions
            .iter()
            .filter(|def| def.key.local.container.is_empty() && isolated.contains(&def.file))
        {
            (routes.defined.entry(def.file.clone()).or_default())
                .insert(def.key.local.name.clone());
        }
        routes
    }

    /// `start` and the files `edges` lead to from it, transitively.
    fn closure(
        edges: &HashMap<RepoPath, HashSet<RepoPath>>,
        start: &RepoPath,
    ) -> HashSet<RepoPath> {
        let mut found = HashSet::from([start.clone()]);
        let mut queue = vec![start];
        while let Some(at) = queue.pop() {
            for next in edges.get(at).into_iter().flatten() {
                if found.insert(next.clone()) {
                    queue.push(next);
                }
            }
        }
        found
    }

    /// Whether `file` spells a name `subject` defines and nothing binds: it may have come by an
    /// import the facts do not hold.
    fn spells(&self, file: &RepoPath, subject: &RepoPath) -> bool {
        let (Some(unbound), Some(defined)) = (self.unbound.get(file), self.defined.get(subject))
        else {
            return false;
        };
        !unbound.is_disjoint(defined)
    }

    /// Whether an object defined in `subject` can come to `file` by an import: some file imports
    /// it (one of the repository, or whoever installs the project that publishes it), it
    /// imports `subject` (through others), or it imports a file that one reaching `subject`
    /// imports too, a module whose state both can see.
    fn meets(&self, file: &RepoPath, subject: &RepoPath) -> bool {
        if self.open
            || self.importers.contains_key(file)
            || self.published.contains(file)
            || self.spells(file, subject)
        {
            return true;
        }
        let reached = Self::closure(&self.imports, file);
        let reaching = Self::closure(&self.importers, subject);
        reached.contains(subject)
            || (reached.iter())
                .flat_map(|module| self.importers.get(module).into_iter().flatten())
                .any(|importer| reaching.contains(importer))
    }
}

/// Whether `path` is a module of what a named Python project publishes: a directory with an
/// `__init__.py`, or the package or module under one of its import roots that the project's
/// name names. Code outside the snapshot imports it.
fn published(workspace: &WorkspaceModel, path: &str) -> bool {
    let Some(project) = workspace.project_of(path).map(|id| workspace.project(id)) else {
        return false;
    };
    let names: Vec<String> = (project.names.iter())
        .filter(|(ecosystem, _)| *ecosystem == Ecosystem::Python)
        .map(|(_, name)| name.replace(['-', '.'], "_"))
        .collect();
    if names.is_empty() {
        return false;
    }
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    project.python_packages.contains(dir)
        || project.import_roots.iter().any(|root| {
            let under = if root.dir.is_empty() {
                Some(path)
            } else {
                (path.strip_prefix(root.dir.as_str())).and_then(|rest| rest.strip_prefix('/'))
            };
            under
                .and_then(|rest| rest.split('/').next())
                .map(|top| top.strip_suffix(".py").unwrap_or(top).to_ascii_lowercase())
                .is_some_and(|top| names.contains(&top))
        })
}

/// Whether the file loads or runs code no import states: a computed import, `eval`, `exec`, a
/// deserializer.
fn runs_code(file: &FileFacts) -> bool {
    file.blob.facts.iter().any(|fact| match fact {
        Fact::ComputedImport(_) => true,
        Fact::Reference(r) if r.use_kind == UseKind::Call => {
            RUNS_CODE.contains(&r.name.as_str())
                || (r.qualifier.last()).is_some_and(|q| DESERIALIZERS.contains(&q.as_str()))
        }
        _ => false,
    })
}

impl Reading<'_> {
    /// Whether the subject lives in a file a page of classic scripts does not load: the scripts it
    /// names are all it can see, and none is the subject's, so its global is no name there.
    pub(super) fn unloaded(&self) -> bool {
        CLASSIC_PAGES.contains(&self.file.language())
            && self.subject.file != self.file.path
            && !(self.file.blob.facts.iter()).any(|fact| matches!(fact, Fact::ComputedImport(_)))
    }

    /// Whether the read's chain starts at a name nothing in the file binds (`Svc.fetch` where no
    /// import, definition or local is `Svc`): what binds it at run time is unseen, so no import
    /// the file lacks proves that the subject's class cannot be there.
    fn unbound_head(&self, binding: &Binding) -> bool {
        let head = self.file.blob.facts.iter().find_map(|fact| match fact {
            Fact::Reference(r)
                if r.site.bytes == binding.site.bytes
                    && matches!(r.receiver, Receiver::Unknown) =>
            {
                r.qualifier.first()
            }
            _ => None,
        });
        let Some(head) = head else {
            return false;
        };
        !self
            .imports
            .iter()
            .any(|import| self.binds(import, head.as_str().as_bytes()))
            && !self.locals.iter().any(|local| local.name == *head)
            && !self.file.defs().any(|def| def.local.name == *head)
    }

    /// The claim of an untyped member read: not ours when nothing brings an object of the
    /// subject's class to the receiver (no import, entry or run-time load of the file can, and
    /// the receiver is no parameter of a function something outside the code enters: a test, a
    /// route), else a candidate.
    pub(super) fn unreached(&self, binding: &Binding, reason: UnresolvedReason) -> Claim {
        let relation = Some(binding.relation);
        let entered = self.file.blob.facts.iter().any(|fact| {
            matches!(fact, Fact::EntryPoint(entry)
                if matches!(entry.target, Scope::Def(_)) && encloses(&entry.target, &binding.from))
        });
        let (file, subject) = (&self.file.path, &self.subject.file);
        let isolated = |language| ISOLATED_MODULES.iter().any(|f| f.has(language));
        let routes = &self.view.index.routes;
        let cut = || {
            isolated(self.file.language())
                && isolated(self.subject.key.language)
                && file != subject
                && !runs_code(self.file)
                && !is_test_path(Path::new(file.as_str()))
                && !routes.meets(file, subject)
        };
        let cut = *routes
            .cut
            .borrow_mut()
            .entry((file.clone(), subject.clone()))
            .or_insert_with(cut);
        if cut && !entered && !self.unbound_head(binding) {
            Claim::not_ours(relation, Witness::Unreached(file.clone()))
        } else {
            Claim::candidate(relation, Reason::Unresolved(reason))
        }
    }
}
