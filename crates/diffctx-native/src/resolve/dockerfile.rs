//! Dockerfile stages, variables and the places its instructions name, bound.
//!
//! - A stage a `FROM` or a `COPY --from` / `RUN --mount=from=` names is found
//!   among the stages before the one that writes it, by the name exactly as the
//!   file spells it, or by its position (`--from=0`). Stage names are not stated
//!   case-insensitive by the Dockerfile reference: a name found only with another
//!   case is a `Candidate` (`CaseFold`), never a silent resolution or a silent
//!   miss (hadolint 2.15.1 reads names case-sensitively, BuildKit lowercases them).
//!   A name of a stage that comes later, or of the stage itself, is no stage there:
//!   an unknown. A name no stage has is an image or a named context: the
//!   registry's, `External`.
//! - A variable a stage reads is the last `ENV` of the name before the read in the
//!   stage, else the last `ARG`, else what the stage it is built `FROM` has at its
//!   end (an `ENV` always overrides an `ARG` of the name; an `ARG` before the first
//!   `FROM` is read by `FROM` lines only and stays out of the stages unless one
//!   redeclares it). One no stage defines is a build argument or a shell variable:
//!   `External`.
//! - A place an instruction copies or mounts is read as a path of the build
//!   context: the Dockerfile's own directory, else the repository root (the usual
//!   `docker build -f dir/Dockerfile .`), as an `Imports` binding of the file it
//!   lands on. A glob, a directory and the whole context are `Opaque`; a name no
//!   file has is `MissingModule`, an unknown.

use std::collections::{HashMap, HashSet};

use super::index::dir_of;
use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution,
    UnresolvedReason,
};
use crate::facts::{
    DefKey, DefKind, Fact, FactsCache, FileFacts, Language, Name, Scope, expired, facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, RepoPath, Result, SideTag, Stage};

/// The bindings of one snapshot's Dockerfiles.
pub struct DockerfileResolver {
    definitions: Vec<Definition>,
    bindings: Vec<Binding>,
    by_target: HashMap<DefKey, Vec<usize>>,
    losses: Vec<Loss>,
    read: Vec<RepoPath>,
}

struct StageRec {
    key: DefKey,
    name: String,
    /// The text after `FROM` in its signature.
    base: String,
}

struct VarRec {
    key: DefKey,
    name: String,
    env: bool,
    /// Where its instruction ends: a read before that is not of it.
    end: u32,
    /// The stage it is in, by name; none before the first `FROM`.
    stage: Option<String>,
}

struct FileRec {
    stages: Vec<StageRec>,
    vars: Vec<VarRec>,
}

impl DockerfileResolver {
    /// Resolves every Dockerfile of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files: Vec<FileFacts> = Vec::new();
        let mut losses = Vec::new();
        let mut read_paths = Vec::new();
        for entry in &listing.entries {
            if Language::from_path(&entry.path) != Some(Language::Dockerfile) {
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
                read_paths.push(entry.path.clone());
                files.push(file);
            }
        }
        let paths: HashSet<&str> = listing.entries.iter().map(|e| e.path.as_str()).collect();
        let mut directories: HashSet<&str> = HashSet::new();
        for &path in &paths {
            let mut dir = dir_of(path);
            while directories.insert(dir) && !dir.is_empty() {
                dir = dir_of(dir);
            }
        }
        let mut definitions = Vec::new();
        let mut bindings = Vec::new();
        for file in &files {
            let rec = read(file);
            for def in file.defs() {
                definitions.push(Definition {
                    file: file.path.clone(),
                    key: file.key(&def.local),
                    kind: def.kind,
                    span: def.span.clone(),
                    name_span: def.name_span.clone(),
                });
            }
            for fact in &file.blob.facts {
                match fact {
                    Fact::Reference(r) => {
                        let resolution = match r.qualifier.first().map(Name::as_str) {
                            Some("from" | "copy") => rec.stage_use(r),
                            _ => rec.variable_use(r),
                        };
                        bindings.push(Binding {
                            file: file.path.clone(),
                            site: r.site.clone(),
                            from: r.enclosing.clone(),
                            relation: Relation::Reads,
                            name: r.name.clone(),
                            resolution,
                            entry_guard: false,
                        });
                    }
                    Fact::PathMention(mention) => bindings.push(Binding {
                        file: file.path.clone(),
                        site: mention.site.clone(),
                        from: mention.enclosing.clone(),
                        relation: Relation::Imports,
                        name: Name::new(mention.text.clone()),
                        resolution: place(&paths, &directories, file.path.as_str(), &mention.text),
                        entry_guard: false,
                    }),
                    _ => {}
                }
            }
        }
        let mut by_target: HashMap<DefKey, Vec<usize>> = HashMap::new();
        for (at, binding) in bindings.iter().enumerate() {
            for key in binding.targets() {
                by_target.entry(key.clone()).or_default().push(at);
            }
        }
        Ok(Self {
            definitions,
            bindings,
            by_target,
            losses,
            read: read_paths,
        })
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// The bindings that resolve to `key` or have it among their candidates.
    pub fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        match self.by_target.get(key) {
            Some(at) => at.iter().map(|i| &self.bindings[*i]).collect(),
            None => Vec::new(),
        }
    }

    /// What the answer is partial for: files the extractor could not fully read,
    /// reads that failed, a run cut short.
    pub fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// The files the resolver read: what an absence may rest on.
    pub fn read_paths(&self) -> &[RepoPath] {
        &self.read
    }
}

fn unresolved(reason: UnresolvedReason) -> Resolution {
    Resolution::Unresolved { reason }
}

fn read(file: &FileFacts) -> FileRec {
    let mut stages = Vec::new();
    let mut vars = Vec::new();
    let mut defs: Vec<_> = file.defs().collect();
    defs.sort_by_key(|d| d.span.bytes.start);
    for def in defs {
        let signature = def.signature.as_deref().unwrap_or("");
        match def.kind {
            DefKind::Object => stages.push(StageRec {
                key: file.key(&def.local),
                name: def.local.name.as_str().to_string(),
                base: signature.strip_prefix("FROM ").unwrap_or("").to_string(),
            }),
            DefKind::Variable => vars.push(VarRec {
                key: file.key(&def.local),
                name: def.local.name.as_str().to_string(),
                env: signature.starts_with("ENV "),
                end: def.span.bytes.end,
                stage: def.local.container.first().map(|n| n.as_str().to_string()),
            }),
            _ => {}
        }
    }
    FileRec { stages, vars }
}

impl FileRec {
    fn stage_at(&self, scope: &Scope) -> Option<usize> {
        let Scope::Def(local) = scope else {
            return None;
        };
        self.stages.iter().position(|s| s.key.local == *local)
    }

    fn resolved(key: &DefKey) -> Resolution {
        Resolution::Resolved {
            target: key.clone(),
            evidence: Evidence::new(EvidenceKind::SameScope),
        }
    }

    /// The stage a name stands for, written inside stage `at`.
    fn stage_named(&self, at: usize, name: &str) -> Resolution {
        if let Ok(index) = name.parse::<usize>() {
            return match self.stages.get(index) {
                Some(stage) if index < at => Self::resolved(&stage.key),
                _ => unresolved(UnresolvedReason::NoBinding),
            };
        }
        let earlier = &self.stages[..at];
        let exact: Vec<&StageRec> = earlier.iter().filter(|s| s.name == name).collect();
        match exact.as_slice() {
            [one] => return Self::resolved(&one.key),
            [] => {}
            several => {
                return Resolution::Candidate {
                    targets: several.iter().map(|s| s.key.clone()).collect(),
                    reason: CandidateReason::Overloaded,
                };
            }
        }
        let folded: Vec<&StageRec> = earlier
            .iter()
            .filter(|s| s.name.eq_ignore_ascii_case(name))
            .collect();
        if !folded.is_empty() {
            return Resolution::Candidate {
                targets: folded.iter().map(|s| s.key.clone()).collect(),
                reason: CandidateReason::CaseFold,
            };
        }
        let not_yet = self.stages[at..]
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(name));
        if not_yet {
            unresolved(UnresolvedReason::NoBinding)
        } else {
            unresolved(UnresolvedReason::External)
        }
    }

    fn stage_use(&self, r: &crate::facts::Ref) -> Resolution {
        match self.stage_at(&r.enclosing) {
            Some(at) => self.stage_named(at, r.name.as_str()),
            None => unresolved(UnresolvedReason::NoBinding),
        }
    }

    /// The stage a stage is built `FROM`, when the base names one.
    fn parent(&self, at: usize) -> Option<usize> {
        let base = self.stages[at].base.as_str();
        if base.contains(['$', ':', '@']) || base == "scratch" {
            return None;
        }
        let found = |same: fn(&str, &str) -> bool| {
            self.stages[..at].iter().rposition(|s| same(&s.name, base))
        };
        found(|a, b| a == b).or_else(|| found(|a, b| a.eq_ignore_ascii_case(b)))
    }

    /// The `ENV` of the name, else the `ARG`, among definitions `before` a place.
    fn pick<'a>(vars: impl Iterator<Item = &'a VarRec>) -> Option<&'a VarRec> {
        let mut last_env = None;
        let mut last_arg = None;
        for var in vars {
            if var.env {
                last_env = Some(var);
            } else {
                last_arg = Some(var);
            }
        }
        last_env.or(last_arg)
    }

    fn variable_use(&self, r: &crate::facts::Ref) -> Resolution {
        let name = r.name.as_str();
        let mut stage = self.stage_at(&r.enclosing);
        let mut before = r.site.bytes.start;
        for _ in 0..=self.stages.len() {
            let owner = stage.map(|s| self.stages[s].name.as_str());
            let found = Self::pick(
                self.vars
                    .iter()
                    .filter(|v| v.name == name && v.stage.as_deref() == owner && v.end <= before),
            );
            if let Some(var) = found {
                return Self::resolved(&var.key);
            }
            let Some(at) = stage else { break };
            // What the stage it is built from ends with.
            stage = self.parent(at);
            before = u32::MAX;
            if stage.is_none() {
                break;
            }
        }
        unresolved(UnresolvedReason::External)
    }
}

/// A path a `COPY` or a mount names, as a file of the repository.
fn place(
    files: &HashSet<&str>,
    directories: &HashSet<&str>,
    dockerfile: &str,
    text: &str,
) -> Resolution {
    if text.contains(['*', '?', '[']) {
        return unresolved(UnresolvedReason::Opaque);
    }
    let Some(path) = normalise(text.trim_start_matches('/')) else {
        return unresolved(UnresolvedReason::External);
    };
    if path.is_empty() {
        return unresolved(UnresolvedReason::Opaque);
    }
    let dir = dir_of(dockerfile);
    let beside = if dir.is_empty() {
        path.clone()
    } else {
        format!("{dir}/{path}")
    };
    let candidates = [
        (beside, EvidenceKind::SameScope),
        (path, EvidenceKind::AmbiguousModule),
    ];
    for (candidate, evidence) in candidates {
        if let Some(found) = files.get(candidate.as_str()) {
            return match RepoPath::new(*found) {
                Ok(file) => Resolution::Module {
                    file,
                    evidence: Evidence::new(evidence),
                },
                Err(_) => unresolved(UnresolvedReason::MissingModule),
            };
        }
        if directories.contains(candidate.as_str()) {
            return unresolved(UnresolvedReason::Opaque);
        }
    }
    unresolved(UnresolvedReason::MissingModule)
}

/// A relative path without `.` and `..` segments; none when it climbs out.
fn normalise(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            other => out.push(other),
        }
    }
    Some(out.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_normalised_or_climbs_out() {
        assert_eq!(normalise("./a/../b/c").as_deref(), Some("b/c"));
        assert_eq!(normalise("../x"), None);
    }
}
