//! Terraform references to the definitions they name, module by module.
//!
//! A module is a directory: a `var.x`, `local.x`, `data.t.n`, `t.n` or
//! `module.m` of a file names the definition of that address in the files of
//! the same directory, and nothing else. The root of the traversal says what
//! kind of thing it names, so a local called `name`, a variable called `name` and
//! a resource called `name` are three definitions and no reference is a guess.
//!
//! A `module "m"` call states where the module is. A `source` that starts with
//! `./` or `../` is a directory of the repository; any other (a registry
//! address, a repository, an archive) is [`PointerKind::Repo`], never a path: its
//! variables and outputs are not held, and the references through the call are
//! unresolved for that reason. Through a local call, `module.m.out` is the
//! `output "out"` of the called directory and an argument `in = ...` of the call
//! is a write of its `variable "in"`.

use std::collections::{HashMap, HashSet};

use super::solved::{Gathered, gather_of};
use super::{
    Binding, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved, UnresolvedReason,
};
use diffctx_facts::facts::config::{PointerKind, Resolved, SourcePointer};
use diffctx_facts::facts::{
    DefKey, DefKind, Fact, FactsCache, FileFacts, Language, Name, Ref, Span, UseKind,
};
use diffctx_snapshot::snapshot::{AnalysisContext, RepoPath, Result, SideTag};

pub struct TerraformResolver {
    solved: Solved,
    /// Module calls whose source is outside the repository.
    external: usize,
    /// The directories a module call of the repository sources: their variables
    /// are set and their outputs read by that call.
    called: HashSet<String>,
}

impl TerraformResolver {
    /// Resolves every Terraform file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let Gathered {
            losses,
            files,
            read,
            ..
        } = gather_of(ctx, side, cache, Language::Terraform)?;
        let index = Index::build(files);
        let (definitions, bindings, external, called) = Engine { ix: &index }.run();
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
            external,
            called,
        })
    }

    /// How many module calls name a source outside the repository: what they
    /// reach is not held, and one of them may be a caller of any module here.
    pub fn external_sources(&self) -> usize {
        self.external
    }

    /// Whether the module `file` is in is entered from outside: what sets its
    /// variables and reads its outputs (an operator's apply, another repository's
    /// call) is not in the snapshot. So is a directory no call of the repository
    /// sources, with or without a backend, a variable file or a lock file, none of
    /// which a module published for others keeps (fj#193); and so is the
    /// repository's root directory, the module a registry publishes, even where
    /// the repository's own examples call it.
    pub fn is_root(&self, file: &str) -> bool {
        let dir = dir_of(file);
        dir.is_empty() || !self.called.contains(dir)
    }
}

solved_by!(TerraformResolver);

// ---- the index

struct DefRec {
    file: usize,
    key: DefKey,
    kind: DefKind,
    span: Span,
    name_span: Span,
    signature: String,
}

#[derive(Default)]
struct Index {
    files: Vec<FileFacts>,
    defs: Vec<DefRec>,
    /// The definitions of a directory by container and name; twins in order.
    by_address: HashMap<(String, Vec<Name>, Name), Vec<usize>>,
    /// The directories that hold a file.
    dirs: HashSet<String>,
}

fn dir_of(path: &str) -> &str {
    diffctx_facts::facts::config::directory_of(path)
}

impl Index {
    fn build(files: Vec<FileFacts>) -> Self {
        let mut index = Self::default();
        for (at, facts) in files.into_iter().enumerate() {
            let dir = dir_of(facts.path.as_str()).to_string();
            for def in facts.blob.defs() {
                let id = index.defs.len();
                index
                    .by_address
                    .entry((
                        dir.clone(),
                        def.local.container.clone(),
                        def.local.name.clone(),
                    ))
                    .or_default()
                    .push(id);
                index.defs.push(DefRec {
                    file: at,
                    key: facts.key(&def.local),
                    kind: def.kind,
                    span: def.span.clone(),
                    name_span: def.name_span.clone(),
                    signature: def.signature.as_deref().unwrap_or("").to_string(),
                });
            }
            index.dirs.insert(dir);
            index.files.push(facts);
        }
        index
    }
}

// ---- the engine

/// Why a name could not be followed to a definition.
enum Outcome {
    Found(usize, EvidenceKind),
    Unresolved(UnresolvedReason),
}

struct Engine<'a> {
    ix: &'a Index,
}

impl Engine<'_> {
    fn run(&self) -> (Vec<Definition>, Vec<Binding>, usize, HashSet<String>) {
        let mut bindings = Vec::new();
        for (f, file) in self.ix.files.iter().enumerate() {
            for fact in &file.blob.facts {
                if let Fact::Reference(r) = fact {
                    bindings.push(self.bind(f, r));
                }
            }
        }
        let definitions = self
            .ix
            .defs
            .iter()
            .map(|rec| Definition {
                file: self.ix.files[rec.file].path.clone(),
                key: rec.key.clone(),
                kind: rec.kind,
                span: rec.span.clone(),
                name_span: rec.name_span.clone(),
            })
            .collect();
        let calls: Vec<Source> = self
            .ix
            .defs
            .iter()
            .enumerate()
            .filter(|(_, rec)| rec.key.local.container.as_slice() == [Name::new("module")])
            .map(|(at, _)| self.source_of(at))
            .collect();
        let external = calls.iter().filter(|s| **s == Source::Outside).count();
        let called = calls
            .into_iter()
            .filter_map(|source| match source {
                Source::Local(path) => Some(path),
                _ => None,
            })
            .collect();
        (definitions, bindings, external, called)
    }

    fn path_of(&self, f: usize) -> &RepoPath {
        &self.ix.files[f].path
    }

    /// The first definition of an address in a directory.
    fn find(&self, dir: &str, container: &[&str], name: &str) -> Option<usize> {
        let key = (
            dir.to_string(),
            container.iter().map(|c| Name::new(*c)).collect(),
            Name::new(name),
        );
        self.ix.by_address.get(&key)?.first().copied()
    }

    fn bind(&self, f: usize, r: &Ref) -> Binding {
        let dir = dir_of(self.path_of(f).as_str());
        let outcome = self.outcome(dir, r);
        let resolution = match outcome {
            Outcome::Found(id, kind) => Resolution::Resolved {
                target: self.ix.defs[id].key.clone(),
                evidence: Evidence::new(kind),
            },
            Outcome::Unresolved(reason) => Resolution::Unresolved { reason },
        };
        Binding {
            file: self.path_of(f).clone(),
            site: r.site.clone(),
            from: r.enclosing.clone(),
            relation: match r.use_kind {
                UseKind::Write => Relation::Writes,
                _ => Relation::Reads,
            },
            name: r.name.clone(),
            resolution,
            entry_guard: false,
        }
    }

    fn outcome(&self, dir: &str, r: &Ref) -> Outcome {
        let name = r.name.as_str();
        let qualifier: Vec<&str> = r.qualifier.iter().map(Name::as_str).collect();
        let same = |container: &[&str]| match self.find(dir, container, name) {
            Some(id) => Outcome::Found(id, EvidenceKind::SameScope),
            None => Outcome::Unresolved(UnresolvedReason::NoBinding),
        };
        match (qualifier.as_slice(), r.use_kind) {
            (["var"], _) => same(&["var"]),
            (["local"], _) => same(&["local"]),
            (["module"], _) => same(&["module"]),
            (["data", ty], _) => same(&[format!("data.{ty}").as_str()]),
            (["module", call], use_kind) => {
                let child = match self.child(dir, call) {
                    Ok(child) => child,
                    Err(reason) => return Outcome::Unresolved(reason),
                };
                let container: &[&str] = if use_kind == UseKind::Write {
                    &["var"]
                } else {
                    &["output"]
                };
                match self.find(&child, container, name) {
                    Some(id) => Outcome::Found(id, EvidenceKind::ModuleAttribute),
                    None => Outcome::Unresolved(UnresolvedReason::NoMember),
                }
            }
            ([ty], _) => same(&[*ty]),
            _ => Outcome::Unresolved(UnresolvedReason::NoBinding),
        }
    }

    /// The directory a call of the module `dir` named `call` calls.
    fn child(&self, dir: &str, call: &str) -> std::result::Result<String, UnresolvedReason> {
        let Some(at) = self.find(dir, &["module"], call) else {
            return Err(UnresolvedReason::NoBinding);
        };
        match self.source_of(at) {
            Source::Local(path) if self.ix.dirs.contains(&path) => Ok(path),
            Source::Local(_) => Err(UnresolvedReason::MissingModule),
            Source::Outside | Source::Registry => Err(UnresolvedReason::External),
            Source::Computed => Err(UnresolvedReason::Opaque),
            Source::Escapes => Err(UnresolvedReason::OutsideProject),
        }
    }

    /// Where a module call's `source` points, from the directory of its file.
    fn source_of(&self, call: usize) -> Source {
        let rec = &self.ix.defs[call];
        let mut words = rec.signature.split_whitespace();
        let text = loop {
            match words.next() {
                Some("source") => break words.next().unwrap_or(""),
                Some(_) => {}
                None => return Source::Computed,
            }
        };
        let local = text.starts_with("./") || text.starts_with("../");
        let pointer = SourcePointer {
            site: Span::default(),
            kind: if local {
                PointerKind::Path
            } else {
                PointerKind::Repo
            },
            text: text.to_string(),
            templated: text == "<computed>",
        };
        let base = dir_of(self.ix.files[rec.file].path.as_str());
        match pointer.resolve(base) {
            Resolved::Local(path) => Source::Local(path),
            Resolved::External if is_registry(text) => Source::Registry,
            Resolved::External => Source::Outside,
            Resolved::Escapes | Resolved::Rooted(_) => Source::Escapes,
            Resolved::Computed => Source::Computed,
        }
    }
}

/// Whether a source is a registry address: a namespace, a name and a provider, and
/// perhaps a subdirectory, with no scheme and no host.
fn is_registry(source: &str) -> bool {
    let address = source
        .split_once("//")
        .map_or(source, |(address, _)| address);
    let parts: Vec<&str> = address.split('/').collect();
    parts.len() == 3
        && !source.contains("::")
        && !parts[0].contains(['.', ':', '@'])
        && parts.iter().all(|part| !part.is_empty())
}

#[derive(PartialEq, Eq)]
enum Source {
    Local(String),
    /// A repository or an archive: not held, and what it holds may be this repository's own
    /// directory.
    Outside,
    /// A registry module (`namespace/name/provider`): not held, and no directory of the
    /// repository.
    Registry,
    Computed,
    Escapes,
}
