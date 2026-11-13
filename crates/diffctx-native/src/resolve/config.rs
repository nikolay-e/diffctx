//! Configuration objects bound to the objects their fields name.
//!
//! The facts say which fields of an object name which kind of object, by a row of
//! the table (`facts::config::k8s`), and which fields merely look like it. This
//! layer finds the object. A target is identified by kind, name and namespace; the
//! group is not part of what a field writes. What a field names stands in the
//! namespace the field writes, else in the namespace of the object that holds it,
//! unless its kind stands in none. An object a file writes without a namespace is
//! deployed into one the file does not say (a Kustomize `namespace`, `kubectl -n`),
//! so it answers for any namespace, but only where no object states the
//! namespace itself.
//!
//! - one object fits: `Resolved`;
//! - several fit, in several files or namespaces the referrer does not tell
//!   apart: `Candidate`;
//! - none stands in the repository: `Unresolved` as `External`, which is no
//!   unknown (the Secret an operator makes is no caller that went unfound);
//! - a field that only looks like a reference and finds an object of its name:
//!   `Candidate`, labelled as a suffix match.
//!
//! A label selector names no object: it is a candidate (`LabelSelector`) for every
//! label definition in its namespace that holds all of its pairs, and never
//! resolved. The labels of an object are definitions of their own, so an edit of
//! them reaches the selectors and no other edit of the object does.
//!
//! A place a Kustomize, Argo CD, Flux, Helmfile or chart file names resolves to the
//! file it lands on (a directory to the file its tool reads there), as an
//! `Imports` binding of the module. A place outside the repository, and a field
//! of a reference that a template writes, are not followed: they are counted, and
//! the cell says so as a gap (`ExternalSource`, `Templated`) where it would claim
//! an absence.

use std::collections::{HashMap, HashSet};

use super::chart;
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason,
};
use crate::facts::config::{
    ConfigRef, LabelSet, ObjectId, Resolved, Selector, SourcePointer, Strength, directory_of,
    identity_of, k8s,
};
use crate::facts::{
    DefKey, DefKind, Fact, FactsCache, FileFacts, Language, LocalKey, Name, Namespace, Scope,
    expired, facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, Stage};

/// The files a tool reads where its place is a directory.
const DIRECTORY_FILES: &[&str] = &[
    "kustomization.yaml",
    "kustomization.yml",
    "Kustomization",
    "Chart.yaml",
    "helmfile.yaml",
    "helmfile.yaml.gotmpl",
];

/// The bindings of one snapshot's configuration objects.
pub struct ConfigResolver {
    solved: Solved,
    external: usize,
    templated: usize,
    unbound: usize,
}

/// One label definition of the snapshot, as a selector can select it.
struct LabelEntry {
    key: DefKey,
    parent: ObjectId,
    parent_key: DefKey,
    set: LabelSet,
    pairs: Vec<(String, String)>,
}

/// One object of the snapshot, as a field can name it.
struct Target {
    key: DefKey,
    kind: String,
    name: String,
    namespace: Option<String>,
}

impl ConfigResolver {
    /// Resolves every YAML file of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files: Vec<FileFacts> = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        for entry in &listing.entries {
            if !Language::from_path(&entry.path).is_some_and(Language::is_config_family) {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                losses.extend(file.losses());
                read.push(entry.path.clone());
                files.push(file);
            }
        }
        let paths: HashSet<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        let mut directories: HashSet<&str> = HashSet::new();
        for &path in &paths {
            let mut dir = directory_of(path);
            while directories.insert(dir) && !dir.is_empty() {
                dir = directory_of(dir);
            }
        }
        let places = Places {
            files: &paths,
            directories: &directories,
        };

        let mut definitions = Vec::new();
        let mut targets: Vec<Target> = Vec::new();
        for file in &files {
            for def in file.defs() {
                let key = file.key(&def.local);
                let object = def.kind == DefKind::Object;
                if let Some(id) = identity_of(&def.local).filter(|_| object) {
                    targets.push(Target {
                        key: key.clone(),
                        kind: id.kind,
                        name: id.name,
                        namespace: id.namespace,
                    });
                }
                definitions.push(Definition {
                    file: file.path.clone(),
                    key,
                    kind: def.kind,
                    span: def.span.clone(),
                    name_span: def.name_span.clone(),
                });
            }
        }
        let mut by_kind_name: HashMap<(&str, &str), Vec<usize>> = HashMap::new();
        let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
        for (at, target) in targets.iter().enumerate() {
            by_kind_name
                .entry((target.kind.as_str(), target.name.as_str()))
                .or_default()
                .push(at);
            by_name.entry(target.name.as_str()).or_default().push(at);
        }
        let engine = Engine {
            targets: &targets,
            by_kind_name: &by_kind_name,
            by_name: &by_name,
        };

        let labels = label_entries(&files);

        let mut bindings = Vec::new();
        let mut external = 0;
        let mut templated = 0;
        for file in &files {
            let mut templated_here = false;
            for fact in &file.blob.facts {
                match fact {
                    Fact::ConfigRef(reference) if reference.templated => templated_here = true,
                    Fact::ConfigRef(reference) => {
                        if let Some(resolution) = engine.resolve(reference) {
                            bindings.push(Binding {
                                file: file.path.clone(),
                                site: reference.site.clone(),
                                from: reference.enclosing.clone(),
                                relation: Relation::Reads,
                                name: Name::new(reference.target_name.clone()),
                                resolution,
                                entry_guard: false,
                            });
                        }
                    }
                    Fact::ConfigSelector(selector) => {
                        let own = match &selector.enclosing {
                            Scope::Def(local) => Some(file.key(local)),
                            _ => None,
                        };
                        let targets = select(&labels, selector, own.as_ref());
                        let resolution = if targets.is_empty() {
                            Resolution::Unresolved {
                                reason: UnresolvedReason::NoMatch,
                            }
                        } else {
                            Resolution::Candidate {
                                targets,
                                reason: CandidateReason::LabelSelector,
                            }
                        };
                        bindings.push(Binding {
                            file: file.path.clone(),
                            site: selector.site.clone(),
                            from: selector.enclosing.clone(),
                            relation: Relation::Reads,
                            name: Name::new(selector.field.clone()),
                            resolution,
                            entry_guard: false,
                        });
                    }
                    Fact::ConfigPointer(pointer) => match places.resolve(&file.path, pointer) {
                        Placed::Lands(resolution) => bindings.push(Binding {
                            file: file.path.clone(),
                            site: pointer.site.clone(),
                            from: Scope::Module,
                            relation: Relation::Imports,
                            name: Name::new(pointer.text.clone()),
                            resolution,
                            entry_guard: false,
                        }),
                        Placed::Outside => external += 1,
                        Placed::Templated => templated_here = true,
                    },
                    _ => {}
                }
            }
            if templated_here {
                templated += 1;
            }
        }
        let chart = chart::bind(&files, &paths);
        bindings.extend(chart.bindings);
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
            external,
            templated,
            unbound: chart.unbound_values,
        })
    }

    /// The places the snapshot's files name outside the repository (a remote
    /// base, a repository an Argo CD application deploys from, a chart in a
    /// registry): what they reach, and what reaches here through them, is not
    /// held.
    pub fn external_sources(&self) -> usize {
        self.external
    }

    /// The values files no release names and no chart holds as its own: what
    /// they set may be read in a release the repository does not write.
    pub fn unbound_values(&self) -> usize {
        self.unbound
    }

    /// The files that write a reference or a place by a template: what they name
    /// is not known.
    pub fn templated_files(&self) -> usize {
        self.templated
    }
}

solved_by!(ConfigResolver);

struct Engine<'a> {
    targets: &'a [Target],
    by_kind_name: &'a HashMap<(&'a str, &'a str), Vec<usize>>,
    by_name: &'a HashMap<&'a str, Vec<usize>>,
}

impl Engine<'_> {
    fn resolve(&self, reference: &ConfigRef) -> Option<Resolution> {
        let candidate = reference.strength == Strength::Candidate;
        let pool: &[usize] = if candidate {
            self.by_name
                .get(reference.target_name.as_str())
                .map_or(&[][..], Vec::as_slice)
        } else {
            self.by_kind_name
                .get(&(
                    reference.target_kind.as_str(),
                    reference.target_name.as_str(),
                ))
                .map_or(&[][..], Vec::as_slice)
        };
        let namespaced = candidate || k8s::is_namespaced(&reference.target_kind).unwrap_or(true);
        let wanted = namespaced
            .then(|| {
                reference
                    .target_namespace
                    .as_ref()
                    .or(reference.from.namespace.as_ref())
            })
            .flatten();
        let mut chosen: Vec<&Target> = pool.iter().map(|at| &self.targets[*at]).collect();
        if let Some(namespace) = wanted {
            let exact: Vec<&Target> = chosen
                .iter()
                .copied()
                .filter(|t| t.namespace.as_ref() == Some(namespace))
                .collect();
            chosen = if exact.is_empty() {
                chosen
                    .into_iter()
                    .filter(|t| t.namespace.is_none())
                    .collect()
            } else {
                exact
            };
        }
        if candidate {
            // A field is never a candidate for the object that holds it.
            if let Scope::Def(own) = &reference.enclosing {
                chosen.retain(|t| &t.key.local != own);
            }
            if chosen.is_empty() {
                return None;
            }
            return Some(Resolution::Candidate {
                targets: chosen.iter().map(|t| t.key.clone()).collect(),
                reason: CandidateReason::NameSuffix,
            });
        }
        Some(match chosen.as_slice() {
            [] => Resolution::Unresolved {
                reason: UnresolvedReason::External,
            },
            [only] => Resolution::Resolved {
                target: only.key.clone(),
                evidence: Evidence::new(EvidenceKind::SameScope),
            },
            several => Resolution::Candidate {
                targets: several.iter().map(|t| t.key.clone()).collect(),
                reason: if namespaced {
                    CandidateReason::AnyNamespace
                } else {
                    CandidateReason::AmbiguousModule
                },
            },
        })
    }
}

/// What a place a file names comes to.
enum Placed {
    Lands(Resolution),
    /// Outside the repository, or computed by something other than a template.
    Outside,
    /// Written by a template.
    Templated,
}

/// The paths of one snapshot, for the places that name them.
struct Places<'a> {
    files: &'a HashSet<&'a str>,
    directories: &'a HashSet<&'a str>,
}

impl Places<'_> {
    fn resolve(&self, file: &RepoPath, pointer: &SourcePointer) -> Placed {
        let path = match pointer.resolve(directory_of(file.as_str())) {
            Resolved::Local(path) | Resolved::Rooted(path) => path,
            Resolved::External | Resolved::Escapes => return Placed::Outside,
            Resolved::Computed if pointer.templated => return Placed::Templated,
            Resolved::Computed => return Placed::Outside,
        };
        if let Some(found) = self.files.get(path.as_str()) {
            return Placed::Lands(self.module(found));
        }
        if self.directories.contains(path.as_str()) {
            for name in DIRECTORY_FILES {
                let inside = if path.is_empty() {
                    (*name).to_string()
                } else {
                    format!("{path}/{name}")
                };
                if let Some(found) = self.files.get(inside.as_str()) {
                    return Placed::Lands(self.module(found));
                }
            }
        }
        Placed::Lands(Resolution::Unresolved {
            reason: UnresolvedReason::MissingModule,
        })
    }

    fn module(&self, path: &str) -> Resolution {
        match RepoPath::new(path) {
            Ok(file) => Resolution::Module {
                file,
                evidence: Evidence::new(EvidenceKind::SameScope),
            },
            Err(_) => Resolution::Unresolved {
                reason: UnresolvedReason::MissingModule,
            },
        }
    }
}

/// The label definitions of the snapshot with their pairs: `labels` of an object
/// (its own) and `pod-labels` of a workload (its pod template's).
fn label_entries(files: &[FileFacts]) -> Vec<LabelEntry> {
    let mut pairs: HashMap<DefKey, Vec<(String, String)>> = HashMap::new();
    for file in files {
        for fact in &file.blob.facts {
            if let Fact::ConfigLabels(labels) = fact {
                if let Scope::Def(local) = &labels.enclosing {
                    pairs.insert(file.key(local), labels.pairs.clone());
                }
            }
        }
    }
    let mut entries = Vec::new();
    for file in files {
        for def in file.defs() {
            let [parent_name] = def.local.container.as_slice() else {
                continue;
            };
            let set = match def.local.name.as_str() {
                "labels" => LabelSet::Own,
                "pod-labels" => LabelSet::Pod,
                _ => continue,
            };
            let key = file.key(&def.local);
            let Some(pairs) = pairs.get(&key) else {
                continue;
            };
            let mut parent = LocalKey::new(Vec::new(), parent_name.clone(), Namespace::Value);
            parent.disambig.clone_from(&def.local.disambig);
            let Some(identity) = identity_of(&parent) else {
                continue;
            };
            entries.push(LabelEntry {
                key,
                parent: identity,
                parent_key: file.key(&parent),
                set,
                pairs: pairs.clone(),
            });
        }
    }
    entries
}

/// The label definitions a selector holds candidates among: of its namespace,
/// of the kinds it can select, holding every pair, and never its own object's
/// when that object is not meant to select itself.
fn select(labels: &[LabelEntry], selector: &Selector, own: Option<&DefKey>) -> Vec<DefKey> {
    labels
        .iter()
        .filter(|entry| {
            let set = match selector.matches {
                LabelSet::Pod => {
                    entry.set == LabelSet::Pod
                        || (entry.set == LabelSet::Own && entry.parent.kind == "Pod")
                }
                LabelSet::Own => entry.set == LabelSet::Own,
            };
            let kind = selector.target_kinds.is_empty()
                || selector.target_kinds.contains(&entry.parent.kind);
            let namespace = match (&selector.namespace, &entry.parent.namespace) {
                (Some(wanted), Some(held)) => wanted == held,
                _ => true,
            };
            set && kind
                && namespace
                && own != Some(&entry.parent_key)
                && selector.pairs.iter().all(|pair| entry.pairs.contains(pair))
        })
        .map(|entry| entry.key.clone())
        .collect()
}
