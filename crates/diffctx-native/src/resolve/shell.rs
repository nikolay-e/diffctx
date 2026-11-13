//! Shell: every call, read and write of a script's facts bound to the function or
//! variable it lands on, through the files the script sources.
//!
//! A function is called by its name in command position, from its own script or
//! from one that sources its file, directly or through a library (#533, #541);
//! functions and variables are global once sourced. What a path spells decides
//! where a `source` leads:
//!
//! - its own directory (`$(dirname "$0")/lib.sh`): that file, exactly;
//! - a literal, `./lib.sh`: the file beside the script, at the root, or wherever
//!   the caller stood, else the file of that name if the repository has one;
//! - a value nothing settles, `$LIB_DIR/lib.sh`: the file of that name, if one;
//!   two files of a name make it ambiguous, and no file name to go by
//!   (`source "$CONFIG"`) makes it unknown.
//!
//! How a script reaches the file that defines a name then is one of:
//! - bound: through sources that lead to the file exactly: `Resolved`;
//! - ambiguous: through a source that may lead to it: `Candidate`;
//! - shadowed: another file it sources defines the name first, or the script
//!   defines it itself: no use of this one;
//! - unknown: the script sources something unknown, which may define it:
//!   `Candidate`, as an import nothing resolved.
//!
//! A library calls what the scripts that source it source beside it: such a call
//! is resolved when one of them reaches the definition bound.

use std::collections::{HashMap, HashSet};

use super::{
    Binding, CandidateReason, Definition, Evidence, EvidenceKind, Relation, Resolution, Solved,
    UnresolvedReason,
};
use crate::facts::shell::{PathHead, PathUse};
use crate::facts::{
    DefKey, DefKind, Fact, FactsCache, FileFacts, Language, Name, Scope, UseKind, expired, facts_of,
};
use crate::snapshot::{AnalysisContext, Loss, Result, SideTag, Stage};

/// The bindings of one snapshot's shell scripts.
pub struct ShellResolver {
    solved: Solved,
    unknown_sources: usize,
    unread: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Named {
    Function,
    Variable,
}

/// What a script defines and sources.
#[derive(Default)]
struct Script {
    functions: HashSet<String>,
    variables: HashSet<String>,
    sources: Vec<(PathHead, String)>,
}

impl Script {
    fn defines(&self, named: Named, name: &str) -> bool {
        match named {
            Named::Function => self.functions.contains(name),
            Named::Variable => self.variables.contains(name),
        }
    }
}

/// Where a sourced path leads in the repository.
enum Target {
    File(String),
    /// A file name several files of the repository carry.
    Ambiguous(Vec<String>),
    /// Nothing the repository holds: `/etc/os-release`, `$HOME/.cargo/env`.
    Outside,
    /// No file name to go by: `source "$CONFIG"`.
    Unknown,
}

/// How a script reaches a file through what it sources, transitively.
#[derive(PartialEq, Eq)]
enum Reach {
    Bound,
    Ambiguous,
    Shadowed,
    Unknown,
    None,
}

/// `a/./b/../c` as `a/c`; `None` when it climbs out of the repository.
fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

struct Index<'a> {
    paths: HashSet<&'a str>,
    /// Every file of the repository by its file name.
    by_name: HashMap<&'a str, Vec<&'a str>>,
    scripts: HashMap<&'a str, Script>,
    /// The scripts that source each file, read once off every script.
    sourced_by: HashMap<String, Vec<String>>,
}

impl Index<'_> {
    fn exists(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    fn by_file_name(&self, path: &str) -> Target {
        let name = path.rsplit('/').next().unwrap_or(path);
        match self.by_name.get(name).map(Vec::as_slice) {
            None | Some([]) => Target::Outside,
            Some([one]) => Target::File((*one).to_string()),
            Some(many) => {
                let mut many: Vec<String> = many.iter().map(|p| (*p).to_string()).collect();
                many.sort();
                Target::Ambiguous(many)
            }
        }
    }

    fn resolve_source(&self, script: &str, head: PathHead, tail: &str) -> Target {
        let dir = script.rsplit_once('/').map_or("", |(d, _)| d);
        let file_name = tail.rsplit('/').next().unwrap_or("");
        match head {
            _ if file_name.is_empty() => Target::Unknown,
            PathHead::ScriptDir => match normalize(&format!("{dir}/{tail}")) {
                Some(p) if self.exists(&p) => Target::File(p),
                _ => Target::Outside,
            },
            // Run from anywhere, a relative path names a file beside the script,
            // at the root, or wherever the caller stood.
            PathHead::Literal if !tail.starts_with(['/', '~']) => {
                [format!("{dir}/{tail}"), tail.to_string()]
                    .iter()
                    .filter_map(|p| normalize(p))
                    .find(|p| self.exists(p))
                    .map_or_else(|| self.by_file_name(tail), Target::File)
            }
            // `$LIB_DIR/x-lib.sh`: a whole file name after an unknown directory
            // is looked up by name.
            PathHead::Unknown if !tail.contains('/') => Target::Unknown,
            _ => self.by_file_name(tail),
        }
    }

    /// Every script that sources `path`, directly or through a library.
    fn sourcers(&self, path: &str) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut stack = vec![path.to_string()];
        while let Some(p) = stack.pop() {
            for r in self.sourced_by.get(&p).into_iter().flatten() {
                if seen.insert(r.clone()) {
                    stack.push(r.clone());
                }
            }
        }
        let mut out: Vec<String> = seen.into_iter().collect();
        out.sort();
        out
    }

    fn reach(&self, from: &str, target: &str, named: Named, name: &str) -> Reach {
        let mut exact: HashSet<String> = HashSet::new();
        let mut any: HashSet<String> = HashSet::new();
        let mut queue = vec![(from.to_string(), true)];
        let (mut ambiguous, mut shadowed, mut unknown) = (false, false, false);
        while let Some((path, bound)) = queue.pop() {
            if !(if bound { &mut exact } else { &mut any }).insert(path.clone()) {
                continue;
            }
            let Some(script) = self.scripts.get(path.as_str()) else {
                continue;
            };
            if path != from && script.defines(named, name) {
                shadowed |= bound;
            }
            for (head, tail) in &script.sources {
                match self.resolve_source(&path, *head, tail) {
                    Target::File(f) if f == target && bound => return Reach::Bound,
                    Target::File(f) if f == target => ambiguous = true,
                    Target::File(f) => queue.push((f, bound)),
                    Target::Ambiguous(all) => {
                        ambiguous |= all.iter().any(|f| f == target);
                        queue.extend(all.into_iter().map(|f| (f, false)));
                    }
                    Target::Unknown => unknown = true,
                    Target::Outside => {}
                }
            }
        }
        if ambiguous {
            Reach::Ambiguous
        } else if shadowed {
            Reach::Shadowed
        } else if unknown {
            Reach::Unknown
        } else {
            Reach::None
        }
    }
}

struct Defined<'a> {
    path: &'a str,
    key: &'a DefKey,
}

fn resolutions(
    index: &Index,
    defs: Option<&Vec<Defined>>,
    from: &str,
    named: Named,
    name: &str,
) -> Vec<Resolution> {
    let Some(defs) = defs else {
        return vec![Resolution::Unresolved {
            reason: UnresolvedReason::External,
        }];
    };
    let own = index
        .scripts
        .get(from)
        .is_some_and(|script| script.defines(named, name));
    let (mut resolved, mut ambiguous, mut unknown): (Vec<Resolution>, Vec<DefKey>, Vec<DefKey>) =
        (Vec::new(), Vec::new(), Vec::new());
    let mut sourcers: Option<Vec<String>> = None;
    for def in defs {
        if def.path == from {
            resolved.push(Resolution::Resolved {
                target: def.key.clone(),
                evidence: Evidence::new(EvidenceKind::SameScope),
            });
            continue;
        }
        // A script that defines the name itself calls its own.
        if own {
            continue;
        }
        let bound = |evidence: EvidenceKind| Resolution::Resolved {
            target: def.key.clone(),
            evidence: Evidence::new(evidence),
        };
        match index.reach(from, def.path, named, name) {
            Reach::Bound => resolved.push(bound(EvidenceKind::ImportedName)),
            Reach::Ambiguous => ambiguous.push(def.key.clone()),
            Reach::Shadowed => {}
            other => {
                let through = sourcers.get_or_insert_with(|| index.sourcers(from));
                if through
                    .iter()
                    .any(|r| index.reach(r, def.path, named, name) == Reach::Bound)
                {
                    resolved.push(bound(EvidenceKind::ImportedName));
                } else if other == Reach::Unknown {
                    unknown.push(def.key.clone());
                }
            }
        }
    }
    if !ambiguous.is_empty() {
        resolved.push(Resolution::Candidate {
            targets: ambiguous,
            reason: CandidateReason::AmbiguousModule,
        });
    }
    if !unknown.is_empty() {
        resolved.push(Resolution::Candidate {
            targets: unknown,
            reason: CandidateReason::ImportUnresolved,
        });
    }
    if resolved.is_empty() {
        resolved.push(Resolution::Unresolved {
            reason: UnresolvedReason::External,
        });
    }
    resolved
}

impl ShellResolver {
    /// Resolves every shell script of `side`.
    pub fn open(ctx: &AnalysisContext, side: SideTag, cache: &FactsCache) -> Result<Self> {
        let reader = match side {
            SideTag::Base => &ctx.base,
            SideTag::Head => &ctx.head,
        };
        let listing = cache.listing(reader)?;
        let mut files: Vec<FileFacts> = Vec::new();
        let mut losses = Vec::new();
        let mut read = Vec::new();
        let mut unread = 0;
        for entry in &listing.entries {
            let language = Language::from_path(&entry.path);
            let script = matches!(language, Some(Language::Shell | Language::ShellUnread))
                || (language.is_none() && Language::needs_shebang(&entry.path));
            if !script {
                continue;
            }
            if expired(&ctx.run) {
                losses.push(Loss::Deadline {
                    stage: Stage::Search,
                });
                break;
            }
            if let Some(file) = facts_of(ctx, side, cache, &entry.path)? {
                if !matches!(file.language(), Language::Shell | Language::ShellUnread) {
                    continue;
                }
                let file_losses = file.losses();
                if file_losses
                    .iter()
                    .any(|loss| matches!(loss, Loss::Unsupported { .. }))
                {
                    unread += 1;
                }
                losses.extend(file_losses);
                read.push(entry.path.clone());
                files.push(file);
            }
        }

        let mut index = Index {
            paths: listing.entries.iter().map(|e| e.path.as_str()).collect(),
            by_name: HashMap::new(),
            scripts: HashMap::new(),
            sourced_by: HashMap::new(),
        };
        for entry in &listing.entries {
            let path = entry.path.as_str();
            let name = path.rsplit('/').next().unwrap_or(path);
            index.by_name.entry(name).or_default().push(path);
        }
        let mut definitions = Vec::new();
        let mut functions: HashMap<&str, Vec<Defined>> = HashMap::new();
        let mut variables: HashMap<&str, Vec<Defined>> = HashMap::new();
        let mut keys: Vec<(usize, DefKey, DefKind)> = Vec::new();
        let mut script_keys: HashMap<&str, DefKey> = HashMap::new();
        for (at, file) in files.iter().enumerate() {
            let script = index.scripts.entry(file.path.as_str()).or_default();
            for def in file.defs() {
                let key = file.key(&def.local);
                match def.kind {
                    DefKind::Function => {
                        script.functions.insert(def.local.name.to_string());
                    }
                    DefKind::Variable => {
                        script.variables.insert(def.local.name.to_string());
                    }
                    DefKind::Module => {
                        script_keys.insert(file.path.as_str(), key.clone());
                    }
                    _ => {}
                }
                definitions.push(Definition {
                    file: file.path.clone(),
                    key: key.clone(),
                    kind: def.kind,
                    span: def.span.clone(),
                    name_span: def.name_span.clone(),
                });
                keys.push((at, key, def.kind));
            }
            for fact in &file.blob.facts {
                if let Fact::ShellPath(path) = fact {
                    if path.how == PathUse::Source {
                        script.sources.push((path.head, path.tail.clone()));
                    }
                }
            }
        }
        for (at, key, kind) in &keys {
            let defined = Defined {
                path: files[*at].path.as_str(),
                key,
            };
            let map = match kind {
                DefKind::Function => &mut functions,
                DefKind::Variable => &mut variables,
                _ => continue,
            };
            map.entry(key.local.name.as_str())
                .or_default()
                .push(defined);
        }
        let mut scripts: Vec<&str> = index.scripts.keys().copied().collect();
        scripts.sort_unstable();
        let mut sourced_by: HashMap<String, Vec<String>> = HashMap::new();
        for script in scripts {
            for (head, tail) in &index.scripts[script].sources {
                if let Target::File(found) = index.resolve_source(script, *head, tail) {
                    sourced_by
                        .entry(found)
                        .or_default()
                        .push(script.to_string());
                }
            }
        }
        index.sourced_by = sourced_by;

        let mut bindings = Vec::new();
        let mut unknown_files: HashSet<&str> = HashSet::new();
        for file in &files {
            let from = file.path.as_str();
            for fact in &file.blob.facts {
                match fact {
                    Fact::Reference(reference) => {
                        let (named, relation, defs) = match reference.use_kind {
                            UseKind::Call => (
                                Named::Function,
                                Relation::Calls,
                                functions.get(reference.name.as_str()),
                            ),
                            UseKind::Read => (
                                Named::Variable,
                                Relation::Reads,
                                variables.get(reference.name.as_str()),
                            ),
                            UseKind::Write => (
                                Named::Variable,
                                Relation::Writes,
                                variables.get(reference.name.as_str()),
                            ),
                            _ => continue,
                        };
                        for resolution in
                            resolutions(&index, defs, from, named, reference.name.as_str())
                        {
                            bindings.push(Binding {
                                file: file.path.clone(),
                                site: reference.site.clone(),
                                from: reference.enclosing.clone(),
                                relation,
                                name: reference.name.clone(),
                                resolution,
                                entry_guard: false,
                            });
                        }
                    }
                    Fact::ShellPath(path) => {
                        let target = index.resolve_source(from, path.head, &path.tail);
                        let relation = match path.how {
                            PathUse::Source => Relation::Imports,
                            PathUse::Run => Relation::Calls,
                        };
                        let resolution = match target {
                            // The script it leads to is the definition that is read:
                            // what sources it runs it here, which is how a use of what
                            // it reaches is told from an import of the name.
                            Target::File(found) => match script_keys.get(found.as_str()) {
                                Some(key) => Resolution::Resolved {
                                    target: key.clone(),
                                    evidence: Evidence::new(match path.how {
                                        PathUse::Source => EvidenceKind::DynamicImport,
                                        PathUse::Run => EvidenceKind::ImportedName,
                                    }),
                                },
                                None => match crate::snapshot::RepoPath::new(&found) {
                                    Ok(file) => Resolution::Module {
                                        file,
                                        evidence: Evidence::new(EvidenceKind::SameScope),
                                    },
                                    Err(_) => Resolution::Unresolved {
                                        reason: UnresolvedReason::MissingModule,
                                    },
                                },
                            },
                            Target::Outside => Resolution::Unresolved {
                                reason: UnresolvedReason::External,
                            },
                            Target::Ambiguous(_) => Resolution::Unresolved {
                                reason: UnresolvedReason::Opaque,
                            },
                            Target::Unknown => {
                                if path.how == PathUse::Source {
                                    unknown_files.insert(from);
                                }
                                Resolution::Unresolved {
                                    reason: UnresolvedReason::Opaque,
                                }
                            }
                        };
                        bindings.push(Binding {
                            file: file.path.clone(),
                            site: path.site.clone(),
                            from: Scope::Module,
                            relation,
                            name: Name::new(path.tail.clone()),
                            resolution,
                            entry_guard: false,
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(Self {
            solved: Solved::new(definitions, bindings, losses, read),
            unknown_sources: unknown_files.len(),
            unread,
        })
    }

    /// The scripts that source a path nothing settles (`source "$CONFIG"`): what
    /// they source may define any name.
    pub fn unknown_sources(&self) -> usize {
        self.unknown_sources
    }

    /// The scripts the bash grammar does not read (a zsh, ksh or fish shebang):
    /// they may call or define any name.
    pub fn unread_scripts(&self) -> usize {
        self.unread
    }
}

solved_by!(ShellResolver);
