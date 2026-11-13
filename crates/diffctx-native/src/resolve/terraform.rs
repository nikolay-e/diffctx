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

use super::{
    Binding, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved, UnresolvedReason,
};
use crate::facts::config::{PointerKind, Resolved, SourcePointer};
use crate::facts::{
    DefKey, DefKind, Fact, FactsCache, FileFacts, Language, Name, Ref, Span, UseKind, expired,
    facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, Stage};

pub struct TerraformResolver {
    solved: Solved,
    /// Module calls whose source is outside the repository.
    external: usize,
}

impl TerraformResolver {
    /// Resolves every Terraform file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            if Language::from_path(&entry.path) != Some(Language::Terraform) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                if file.language() == Language::Terraform {
                    losses.extend(file.losses());
                    read.push(entry.path.clone());
                    files.push(file);
                }
            }
        }
        let index = Index::build(files);
        let (definitions, bindings, external) = Engine { ix: &index }.run();
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
            external,
        })
    }

    /// How many module calls name a source outside the repository: what they
    /// reach is not held, and one of them may be a caller of any module here.
    pub fn external_sources(&self) -> usize {
        self.external
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
    crate::facts::config::directory_of(path)
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
    fn run(&self) -> (Vec<Definition>, Vec<Binding>, usize) {
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
        let external = self
            .ix
            .defs
            .iter()
            .enumerate()
            .filter(|(_, rec)| rec.key.local.container.as_slice() == [Name::new("module")])
            .filter(|(at, _)| self.source_of(*at) == Source::Outside)
            .count();
        (definitions, bindings, external)
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
            Source::Outside => Err(UnresolvedReason::External),
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
            Resolved::External => Source::Outside,
            Resolved::Escapes | Resolved::Rooted(_) => Source::Escapes,
            Resolved::Computed => Source::Computed,
        }
    }
}

#[derive(PartialEq, Eq)]
enum Source {
    Local(String),
    /// A registry, a repository, an archive: not held.
    Outside,
    Computed,
    Escapes,
}
