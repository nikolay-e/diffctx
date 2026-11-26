//! Reading what a range changed into changed definitions: the semantic changes, the moved declarations,
//! lineages and the reshaped defaults.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Range;

use anyhow::Result;

use crate::fold::{self, Basis};

use diffctx_facts::facts::{
    Def, DefChange, DefKey, DefKind, Delta, EntryKind, Export, Fact, Framework, HeldAs, Import,
    ImportKind, Language, Lexical, Name, Origin, Scope, SemanticDef, Visibility, facts_of,
    holds_data, members_of, semantic_changes,
};

use diffctx_resolve::resolve::Definition;

use diffctx_resolve::vocabulary::family::{Family, Resolver};

use diffctx_snapshot::snapshot::{ChangeKind, RepoPath, SideTag};

use super::super::verdict::{Reach, Subject};

use super::super::changed::{
    Changed, How, Whole, bases_in, is_label_definition, own_span, own_tokens_held,
};

use super::super::cells::merge;

use super::Cell;
use super::within_scope;

impl Cell {
    /// What the range did to the family's definitions: the cell's changed list.
    pub(super) fn read_changes(&mut self) -> Result<()> {
        let set = semantic_changes(&self.ctx, &self.cache)?;
        self.losses.extend(set.losses.iter().cloned());
        let (reshaped, away) = self.reshaped_defaults(&set.defs)?;
        let python: Vec<_> = set
            .defs
            .iter()
            .filter(|d| !away.contains(&d.key))
            .chain(reshaped.iter())
            .filter(|d| self.family.has(d.key.language))
            .filter(|d| {
                [&d.head, &d.base]
                    .into_iter()
                    .flatten()
                    .any(|file| within_scope(&self.scope, file.as_str()))
            })
            .collect();
        // A field that changed class and nothing else (into the subclass that
        // inherited it, out to a new base) is the same field: the classes it
        // moved between are what changed.
        let moved_as_it_was = |d: &SemanticDef| {
            self.family == Family::Python
                && matches!(&d.change, DefChange::Changed { origin: Origin::Moved { .. }, delta }
                    if !(delta.signature || delta.body || delta.visibility || delta.decorators))
                && self.definition(&d.key).is_some_and(|def| {
                    matches!(
                        def.kind,
                        DefKind::Field | DefKind::Variable | DefKind::Constant
                    ) && !def.key.local.container.is_empty()
                })
        };
        // A file git renames byte for byte took its definitions along: their
        // place changed, nothing they say did (#593). The stale readers of the
        // old path are the file-level channel's.
        let pure_moves: HashSet<(&RepoPath, &RepoPath)> = self
            .ctx
            .changes
            .changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Renamed && c.similarity == Some(100))
            .filter_map(|c| Some((c.from.as_ref()?, &c.path)))
            .collect();
        let moved_with_its_file = |d: &SemanticDef| {
            matches!(&d.change, DefChange::Changed { origin: Origin::Moved { .. }, delta }
                if !(delta.signature || delta.body || delta.visibility || delta.decorators))
                && matches!((&d.base, &d.head), (Some(from), Some(to))
                    if pure_moves.contains(&(from, to)))
        };
        let python: Vec<_> = python
            .into_iter()
            .filter(|&d| !moved_as_it_was(d) && !moved_with_its_file(d))
            .collect();
        let moved = self.moved_declarations(&python)?;
        let python: Vec<_> = python
            .into_iter()
            .filter(|d| !moved.contains(&d.key))
            .collect();
        // A decorated function, method or class is never set aside as a doc or
        // layout edit: a decorator may read the docstring (CLI help, routes). A
        // docstring edit lists its rows; a comment-only one is traced and its rows
        // fold as no code change (`basis_of`).
        let mut decorated: HashSet<DefKey> = HashSet::new();
        for d in &python {
            if own_tokens_held(d) && self.decorated_at_head(d) {
                decorated.insert(d.key.clone());
            }
        }
        let entries: HashMap<&DefKey, &SemanticDef> = python.iter().map(|d| (&d.key, *d)).collect();
        let mut wholes = HashMap::new();
        for d in python.iter().filter(|d| own_tokens_held(d)) {
            self.whole(d, &entries, &decorated, &mut wholes)?;
        }
        let whole = |d: &SemanticDef| {
            (own_tokens_held(d) && !decorated.contains(&d.key))
                .then(|| wholes.get(&d.key).copied().unwrap_or(Whole::Layout))
        };
        let layout_only = |d: &SemanticDef| whole(d) == Some(Whole::Layout);
        // A definition whose members were added, removed or edited, and nothing
        // else: their own rows are the change, and say what it was.
        let folded = |d: &SemanticDef| whole(d) == Some(Whole::Members);
        for d in &python {
            if !layout_only(d) && !folded(d) {
                self.changed_keys.insert(d.key.clone());
            }
        }
        let class_removed = |key: &DefKey| {
            let Some((name, outer)) = key.local.container.split_last() else {
                return false;
            };
            python.iter().any(|c| {
                c.head.is_none()
                    && matches!(c.change, DefChange::Removed)
                    && c.key.project == key.project
                    && c.key.module == key.module
                    && c.key.language == key.language
                    && c.key.local.name == *name
                    && c.key.local.container.as_slice() == outer
            })
        };
        // The members of a Rust type that moved (an enum's variants, a struct's
        // fields) went with it: they are no removals of their own.
        let family = self.family;
        let holder_moved = |key: &DefKey| {
            let Some((name, outer)) = key.local.container.split_last() else {
                return false;
            };
            family == Family::Rust
                && python.iter().any(|c| {
                    matches!(&c.change, DefChange::Changed { origin: Origin::Moved { from }, .. }
                        if from.project == key.project
                            && from.module == key.module
                            && from.language == key.language
                            && from.local.name == *name
                            && from.local.container.as_slice() == outer)
                })
        };
        let mut by_key: BTreeMap<(DefKey, String), Changed> = BTreeMap::new();
        let mut removed: Vec<Changed> = Vec::new();
        for d in &python {
            match &d.change {
                // A definition the range removed, or one whose pairing is ambiguous and
                // that only the base has: reported either way, the second as not
                // determined (`Gap::Unpaired`), never dropped.
                DefChange::Removed | DefChange::Unknown { .. } if d.head.is_none() => {
                    let unpaired = matches!(d.change, DefChange::Unknown { .. });
                    let Some(file) = d.base.as_ref() else {
                        continue;
                    };
                    let Some((kind, lines, public)) = self.base_definition(file, &d.key)? else {
                        continue;
                    };
                    // The members of a class that left go with its line.
                    if matches!(kind, DefKind::Field | DefKind::Variable | DefKind::Constant)
                        && class_removed(&d.key)
                    {
                        continue;
                    }
                    if holder_moved(&d.key) || self.moved_within_type(&d.key, kind) {
                        continue;
                    }
                    let gone = Changed {
                        name: self.shown_name(kind, d.key.local.name.as_str(), file.as_str()),
                        kind,
                        file: file.as_str().to_string(),
                        lines,
                        how: How::Removed,
                        subjects: Vec::new(),
                        stale: vec![d.key.clone()],
                        unpaired,
                        local: false,
                        public,
                        basis: None,
                        not_layout: false,
                        api: false,
                        delta: None,
                        origin: None,
                    };
                    // A class that lost its constructor or a field is changed like
                    // one whose constructor or field was edited: what builds,
                    // extends or names it is affected, and nothing says so under
                    // the removed member.
                    let rebuilt = match kind {
                        DefKind::Constructor => Some(Reach::Constructor),
                        DefKind::Field | DefKind::Variable | DefKind::Constant => {
                            Some(Reach::Construction)
                        }
                        _ => None,
                    };
                    if let (Some(reach), Family::Python) = (rebuilt, self.family) {
                        if let Some(class) = self.class_holding(&gone.file, &d.key) {
                            let passing = (reach == Reach::Construction)
                                .then(|| self.passed_to_reach(&gone.file, &d.key, class))
                                .flatten();
                            merge(
                                &mut by_key,
                                Changed {
                                    name: class.key.local.name.as_str().to_string(),
                                    kind: class.kind,
                                    file: class.file.as_str().to_string(),
                                    lines: class.span.lines,
                                    how: How::Edited { layout_only: false },
                                    subjects: vec![Subject {
                                        key: class.key.clone(),
                                        reach,
                                        within: own_span(class.kind, class.span.lines),
                                        passing,
                                    }],
                                    stale: Vec::new(),
                                    unpaired: false,
                                    local: false,
                                    public: false,
                                    basis: None,
                                    not_layout: false,
                                    api: false,
                                    delta: None,
                                    origin: None,
                                },
                            );
                        }
                    }
                    removed.push(gone);
                }
                _ if folded(d) => {}
                change => {
                    let in_file =
                        |file: &RepoPath| self.defs_in(file.as_str()).find(|def| def.key == d.key);
                    let Some(def) = d
                        .head
                        .as_ref()
                        .and_then(in_file)
                        .or_else(|| self.definition(&d.key))
                    else {
                        continue;
                    };
                    if wholes.get(&d.key) == Some(&Whole::Members)
                        && self.lost_only_omittable_fields(d, def, &python)
                    {
                        continue;
                    }
                    let unaccounted = whole(d) == Some(Whole::Unaccounted);
                    let how = match change {
                        DefChange::Added => How::Added,
                        _ => How::Edited {
                            layout_only: layout_only(d),
                        },
                    };
                    let basis = match change {
                        DefChange::Changed {
                            origin: Origin::Same,
                            delta,
                        } if !unaccounted => self.basis_of(d, def, *delta),
                        _ => None,
                    };
                    let mut stale = match change {
                        DefChange::Changed {
                            origin: Origin::Moved { from } | Origin::Renamed { from },
                            ..
                        } => vec![from.clone()],
                        _ => Vec::new(),
                    };
                    // A Rust definition that moved while a file still names the old
                    // path is a removal as well: what names it broke, and says so
                    // under the old path, not as a caller of the new one.
                    if let (
                        Family::Rust,
                        DefChange::Changed {
                            origin: Origin::Moved { from },
                            ..
                        },
                        Some(base_file),
                    ) = (self.family, change, d.base.as_ref())
                    {
                        if let Some((kind, lines, public)) = self
                            .base_definition(base_file, from)?
                            .filter(|(kind, ..)| !self.moved_within_type(from, *kind))
                        {
                            let gone = Changed {
                                name: from.local.name.as_str().to_string(),
                                kind,
                                file: base_file.as_str().to_string(),
                                lines,
                                how: How::Removed,
                                subjects: Vec::new(),
                                stale: vec![from.clone()],
                                unpaired: false,
                                local: false,
                                public,
                                basis: None,
                                not_layout: false,
                                api: false,
                                delta: None,
                                origin: None,
                            };
                            if self.stranded(&gone) {
                                removed.push(gone);
                                stale = Vec::new();
                            }
                        }
                    }
                    // The selectors that selected labels at the base, and select
                    // nothing after the edit, are the readers the edit broke: the
                    // key is its own stale name.
                    if self.family == Family::Config
                        && is_label_definition(&d.key)
                        && matches!(change, DefChange::Changed { origin: Origin::Same, delta }
                            if delta.body)
                    {
                        stale = vec![d.key.clone()];
                    }
                    let unpaired = matches!(change, DefChange::Unknown { .. });
                    // A default export that only changed its form is one export: its
                    // place in the surface moves with its signature, not its name.
                    let reshaped_here = reshaped.iter().any(|r| r.key == d.key);
                    let api = self.surface_moved(d, change, reshaped_here);
                    for mut changed in self.report_as(def, how, stale, unpaired, true) {
                        changed.basis.clone_from(&basis);
                        changed.not_layout = decorated.contains(&d.key) || unaccounted;
                        changed.api =
                            api && changed.subjects.first().is_some_and(|s| s.key == d.key);
                        if changed.subjects.first().is_some_and(|s| s.key == d.key) {
                            if let DefChange::Changed { origin, delta } = change {
                                changed.delta = Some(*delta);
                                changed.origin = match origin {
                                    Origin::Same => None,
                                    Origin::Moved { .. } => Some("moved"),
                                    Origin::Renamed { .. } => Some("renamed"),
                                };
                            }
                        }
                        merge(&mut by_key, changed);
                    }
                }
            }
        }
        self.changed = by_key.into_values().chain(removed).collect();
        self.changed.sort_by(|a, b| {
            (a.file.as_str(), a.lines, a.name.as_str()).cmp(&(
                b.file.as_str(),
                b.lines,
                b.name.as_str(),
            ))
        });
        Ok(())
    }

    /// Whether a Rust member that left its `impl` is still a member of its type:
    /// an `impl` of that type, in any module, has one of its name and kind. An
    /// inherent member is matched by an inherent one and a trait impl's by the
    /// impl of the same trait.
    fn moved_within_type(&self, key: &DefKey, kind: DefKind) -> bool {
        let Resolver::Rust(head) = &self.head else {
            return false;
        };
        if key.local.container.is_empty() {
            return false;
        }
        let Ok(Resolver::Rust(base)) = self.base() else {
            return false;
        };
        let Some(owner) = base.owner_of(key) else {
            return false;
        };
        head.definitions().iter().any(|d| {
            d.kind == kind
                && d.key.local.name == key.local.name
                && d.key.local.disambig == key.local.disambig
                && head.owner_of(&d.key) == Some(owner)
        })
    }

    /// Whether the range moved the definition's place in the module's surface: it
    /// is new, moved to another file or container, renamed, or has another
    /// signature or visibility than at the base. A body, doc or layout edit is
    /// none, and a default export that only changed its form (`reshaped`) moves
    /// with its signature and visibility alone. In Kotlin (JVM callers and the
    /// modules that depend on them name it)
    /// it also has to be public at the head.
    fn surface_moved(&self, d: &SemanticDef, change: &DefChange, reshaped: bool) -> bool {
        let moved = match change {
            DefChange::Changed { delta, .. } if reshaped => delta.signature || delta.visibility,
            DefChange::Changed { origin, delta } => {
                !matches!(origin, Origin::Same) || delta.signature || delta.visibility
            }
            _ => true,
        };
        moved
            && (self.family != Family::Kotlin
                || d.head.as_ref().is_some_and(|file| {
                    self.declared(SideTag::Head, file, &d.key)
                        .unwrap_or(None)
                        .is_some_and(|def| def.visibility == Visibility::Public)
                }))
    }

    /// The definition of `file` at the head named `name` whose span meets `lines`,
    /// the first in the resolver's order.
    fn first_definition_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Option<&diffctx_resolve::resolve::Definition> {
        self.defs_in(file).find(|d| {
            d.key.local.name.as_str() == name
                && d.span.lines.0 <= lines.1
                && lines.0 <= d.span.lines.1
        })
    }

    /// Whether the definition at the head is linked by its bare name: a C function the
    /// linker exports (`static` makes it private), or a Go function a `//export`
    /// comment hands to C. Any language that loads the library reaches it by name.
    /// The facts say it, never the declaration's text.
    pub(crate) fn linked_by_name_at(&self, file: &str, name: &str, lines: (u32, u32)) -> bool {
        let Some(def) = self.first_definition_at(file, name, lines) else {
            return false;
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Head, &self.cache, &def.file) else {
            return false;
        };
        let language = facts.language();
        facts.defs().any(|d| {
            d.local == def.key.local
                && d.kind == DefKind::Function
                && match language {
                    Language::C => d.visibility != Visibility::Private,
                    Language::Go => d.decorators.iter().any(|n| n.as_str() == "export"),
                    _ => false,
                }
        })
    }

    /// The annotations a Kotlin definition states at the head, by where it is:
    /// the JVM entry points a framework calls are read off them. `None` for
    /// another family or a definition the facts do not hold.
    pub(crate) fn annotations_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Option<Vec<String>> {
        if self.family != Family::Kotlin {
            return None;
        }
        let def = self.first_definition_at(file, name, lines)?;
        let found = self
            .declared(SideTag::Head, &def.file, &def.key)
            .unwrap_or(None)?;
        Some(
            found
                .decorators
                .iter()
                .map(|n| n.as_str().to_string())
                .collect(),
        )
    }

    /// Whether a call spanning `line` of `file` at the head reaches a definition by a
    /// name the code computes (`getattr(obj, name)`, `globals()[key]`, `eval`).
    pub(crate) fn computes_name_at(&self, file: &str, line: u32) -> bool {
        let Ok(path) = RepoPath::new(file) else {
            return false;
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Head, &self.cache, &path) else {
            return false;
        };
        facts.blob.facts.iter().any(|fact| {
            matches!(fact, Fact::ComputedName { site, .. }
                if (site.lines.0..=site.lines.1).contains(&line))
        })
    }

    /// Whether Jackson calls the Java definition at the head, by where it is: a
    /// getter of a class it serializes.
    pub(crate) fn serialized_at(&self, file: &str, name: &str, lines: (u32, u32)) -> bool {
        self.first_definition_at(file, name, lines)
            .is_some_and(|def| self.head.serialized(&def.key))
    }

    /// The imports `file` states at the head, without the package it declares.
    pub(crate) fn imports_at(&self, file: &str) -> Vec<Import> {
        let Ok(path) = RepoPath::new(file) else {
            return Vec::new();
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Head, &self.cache, &path) else {
            return Vec::new();
        };
        facts
            .blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Import(import) if import.kind != ImportKind::SideEffect => {
                    Some(import.clone())
                }
                _ => None,
            })
            .collect()
    }

    /// The strings of `file` at the head that span `line`, with what their
    /// position in the file's tree holds them as. A Go package's callable strings
    /// count only where the package itself resolves a name by string (`Lookup`,
    /// `MethodByName`, `FieldByName`).
    pub(crate) fn held_strings_at(&self, file: &str, line: u32) -> Vec<(HeldAs, String)> {
        let Ok(path) = RepoPath::new(file) else {
            return Vec::new();
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Head, &self.cache, &path) else {
            return Vec::new();
        };
        let mut held: Vec<(HeldAs, String)> = facts
            .blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::HeldString(held)
                    if (held.site.lines.0..=held.site.lines.1).contains(&line) =>
                {
                    Some((held.held.clone(), held.text.clone()))
                }
                _ => None,
            })
            .collect();
        if self.family == Family::Go
            && held.iter().any(|(how, _)| *how == HeldAs::Callable)
            && self.owns(file)
            && !self.package_looks_up_by_name(file)
        {
            held.retain(|(how, _)| *how != HeldAs::Callable);
        }
        held
    }

    /// The head text of `file` and the byte ranges of its string literals, as the
    /// file's tree reads them; `None` where no tree records the strings.
    pub(crate) fn string_pieces(&self, file: &str) -> Result<Option<(String, Vec<Range<u32>>)>> {
        let path = RepoPath::new(file)?;
        let Some(facts) = facts_of(&self.ctx, SideTag::Head, &self.cache, &path)? else {
            return Ok(None);
        };
        let ranges: Vec<Range<u32>> = facts
            .blob
            .lexical
            .iter()
            .filter(|(_, kind)| *kind == Lexical::String)
            .map(|(range, _)| range.clone())
            .collect();
        if ranges.is_empty() {
            return Ok(None);
        }
        let Some(bytes) = self.ctx.head.read(&path)? else {
            return Ok(None);
        };
        let Ok(text) = String::from_utf8(bytes.to_vec()) else {
            return Ok(None);
        };
        Ok(Some((text, ranges)))
    }

    /// Whether a file of `file`'s directory, the Go package, references one of the calls
    /// that resolve a name given as a string.
    fn package_looks_up_by_name(&self, file: &str) -> bool {
        let dir = |path: &str| path.rsplit_once('/').map(|(dir, _)| dir.to_string());
        self.bindings_by_file
            .iter()
            .filter(|(path, _)| dir(path) == dir(file))
            .flat_map(|(_, at)| at)
            .any(|&at| {
                diffctx_facts::facts::lang::go::BY_NAME_LOOKUPS
                    .contains(&self.head.bindings()[at].name.as_str())
            })
    }

    /// The entries a framework makes of the definition at the head, by where it
    /// is: what the facts state of who calls it (a route, a scheduled job, a
    /// command, a tool, a fixture). Empty for a definition no rule matches, and
    /// for one the facts do not hold.
    pub(crate) fn framework_entries_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Vec<(EntryKind, Framework)> {
        let Some(def) = self.first_definition_at(file, name, lines) else {
            return Vec::new();
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Head, &self.cache, &def.file) else {
            return Vec::new();
        };
        facts
            .blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::EntryPoint(entry) if entry.target == Scope::Def(def.key.local.clone()) => {
                    Some((entry.kind, entry.framework.clone()?))
                }
                _ => None,
            })
            .collect()
    }

    /// The entries a framework made of the definition a range removed, read off
    /// the base: what its callers outside the code (a route's clients) reached it
    /// by. `lines` are its span at the base.
    pub(crate) fn removed_entries_at(
        &self,
        file: &str,
        name: &str,
        lines: (u32, u32),
    ) -> Vec<(EntryKind, Framework)> {
        let Ok(path) = RepoPath::new(file) else {
            return Vec::new();
        };
        let Ok(Some(facts)) = facts_of(&self.ctx, SideTag::Base, &self.cache, &path) else {
            return Vec::new();
        };
        facts
            .blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::EntryPoint(entry)
                    if matches!(&entry.target, Scope::Def(local) if local.name.as_str() == name)
                        && entry.site.lines.0 <= lines.1
                        && lines.0 <= entry.site.lines.1 =>
                {
                    Some((entry.kind, entry.framework.clone()?))
                }
                _ => None,
            })
            .collect()
    }

    /// Whether the definition at the head is a function, method or class with
    /// decorators.
    fn decorated_at_head(&self, d: &SemanticDef) -> bool {
        let Some(file) = d.head.as_ref() else {
            return false;
        };
        // A definition whose facts cannot be read is not known to be undecorated:
        // `true` keeps it listed.
        let Ok(found) = self.declared(SideTag::Head, file, &d.key) else {
            return true;
        };
        found.is_some_and(|def| {
            !def.decorators.is_empty()
                && matches!(
                    def.kind,
                    DefKind::Function | DefKind::Method | DefKind::Constructor | DefKind::Class
                )
        })
    }

    /// What an edit in place did to a definition, as the folding table reads it.
    /// None when the facts of either side are unread, or when the edit moved the
    /// definition's visibility or only its docstring (a decorator may read it): then
    /// nothing folds. A comment-only edit has a basis with no code delta.
    fn basis_of(&self, d: &SemanticDef, def: &Definition, delta: Delta) -> Option<Basis> {
        // The folding table has no row for a configuration definition: one changed
        // key reaching one reader is the whole answer, whatever the kind says
        // (a Helm named template and a shell function are `Function`s).
        if matches!(
            self.family,
            Family::Config | Family::Terraform | Family::Dockerfile | Family::Shell
        ) {
            return None;
        }
        let code = delta.signature || delta.body || delta.decorators;
        if delta.visibility || (!code && delta.doc) {
            return None;
        }
        let (head, base) = (d.head.as_ref()?, d.base.as_ref()?);
        let at_head = self.declared(SideTag::Head, head, &d.key).unwrap_or(None)?;
        let compatible = if delta.signature && !delta.decorators {
            let at_base = self.declared(SideTag::Base, base, &d.key).unwrap_or(None)?;
            fold::compatible(
                at_base.signature.as_deref()?,
                at_head.signature.as_deref()?,
                d.key.local.name.as_str(),
                d.key.language,
            )
        } else {
            None
        };
        Some(Basis {
            kind: def.kind,
            signature: delta.signature,
            body: delta.body,
            decorators: delta.decorators,
            compatible,
            typed: fold::declared_types(
                def.kind,
                d.key.language,
                at_head.signature.as_deref(),
                d.key.local.name.as_str(),
            ),
        })
    }

    /// A field declaration only one side has, with an equal one on the other
    /// side in a class of the same lineage: it moved between a base and its
    /// subclass (merged into the subclass, lifted into a new base), and the
    /// field is the same. Annotation-only fields have no body to pair them by.
    fn moved_declarations(&self, entries: &[&SemanticDef]) -> Result<HashSet<DefKey>> {
        if self.family != Family::Python {
            return Ok(HashSet::new());
        }
        struct Field<'a> {
            entry: &'a SemanticDef,
            file: &'a RepoPath,
            class: Name,
            declaration: Def,
        }
        let mut gone: Vec<Field<'_>> = Vec::new();
        let mut added: Vec<Field<'_>> = Vec::new();
        for &entry in entries {
            let Some(class) = entry.key.local.container.last().cloned() else {
                continue;
            };
            let (side, file, into) = match (&entry.change, &entry.base, &entry.head) {
                (DefChange::Removed, Some(file), None) => (SideTag::Base, file, &mut gone),
                (DefChange::Added, None, Some(file)) => (SideTag::Head, file, &mut added),
                _ => continue,
            };
            let Some(declaration) = self.declared(side, file, &entry.key)? else {
                continue;
            };
            if matches!(
                declaration.kind,
                DefKind::Field | DefKind::Variable | DefKind::Constant
            ) {
                into.push(Field {
                    entry,
                    file,
                    class,
                    declaration,
                });
            }
        }
        let mut moved = HashSet::new();
        for from in &gone {
            for to in added.iter().filter(|to| {
                to.file == from.file
                    && to.entry.key.local.name == from.entry.key.local.name
                    && to.declaration.signature == from.declaration.signature
                    && to.declaration.body_hash == from.declaration.body_hash
                    && to.declaration.visibility == from.declaration.visibility
                    && to.declaration.decorators == from.declaration.decorators
            }) {
                let mut related = false;
                for side in [SideTag::Base, SideTag::Head] {
                    related = related
                        || self
                            .lineage(side, to.file, &to.class)?
                            .iter()
                            .any(|n| n == from.class.as_str())
                        || self
                            .lineage(side, from.file, &from.class)?
                            .iter()
                            .any(|n| n == to.class.as_str());
                }
                if related {
                    moved.insert(from.entry.key.clone());
                    moved.insert(to.entry.key.clone());
                }
            }
        }
        Ok(moved)
    }

    pub(super) fn declared(
        &self,
        side: SideTag,
        file: &RepoPath,
        key: &DefKey,
    ) -> Result<Option<Def>> {
        Ok(facts_of(&self.ctx, side, &self.cache, file)?
            .and_then(|facts| facts.defs().find(|d| d.local == key.local).cloned()))
    }

    /// The names a class lists as bases on one side, and theirs in turn, within
    /// its file.
    fn lineage(&self, side: SideTag, file: &RepoPath, class: &Name) -> Result<Vec<String>> {
        let Some(facts) = facts_of(&self.ctx, side, &self.cache, file)? else {
            return Ok(Vec::new());
        };
        let mut seen: Vec<String> = Vec::new();
        let mut queue = vec![class.as_str().to_string()];
        while let Some(name) = queue.pop() {
            for def in facts
                .defs()
                .filter(|d| d.kind == DefKind::Class && d.local.name.as_str() == name)
            {
                for base in bases_in(def.signature.as_deref()) {
                    if !seen.contains(&base) {
                        seen.push(base.clone());
                        queue.push(base);
                    }
                }
            }
        }
        Ok(seen)
    }

    /// The module's default export when it changed its form and nothing else: `export
    /// default (a) => …` became `export default function name(a) { … }`, or `export
    /// default f` over a constant. The facts see the removal of `default` and a new
    /// named definition; the export is one, so the two are one rename of it (with the
    /// signature compared, since the name was never part of it), and neither is an
    /// export removed or added. The second list holds the keys the pair replaces.
    fn reshaped_defaults(&self, defs: &[SemanticDef]) -> Result<(Vec<SemanticDef>, Vec<DefKey>)> {
        let mut pairs = Vec::new();
        let mut away = Vec::new();
        if self.family != Family::TypeScript {
            return Ok((pairs, away));
        }
        for gone in defs.iter().filter(|d| {
            matches!(d.change, DefChange::Removed)
                && d.head.is_none()
                && d.key.local.name.as_str() == "default"
                && d.key.local.container.is_empty()
        }) {
            let Some(file) = gone.base.as_ref() else {
                continue;
            };
            let Some(head) = facts_of(&self.ctx, SideTag::Head, &self.cache, file)? else {
                continue;
            };
            let Some(local) = head.blob.facts.iter().find_map(|fact| match fact {
                Fact::Export(Export::Default { local: Some(l), .. }) => Some(l.clone()),
                _ => None,
            }) else {
                continue;
            };
            let Some(added) = defs.iter().find(|d| {
                matches!(d.change, DefChange::Added)
                    && d.head.as_ref() == Some(file)
                    && d.key.local.name == local
                    && d.key.local.container.is_empty()
                    && d.key.project == gone.key.project
                    && d.key.module == gone.key.module
            }) else {
                continue;
            };
            let before = self.declared(SideTag::Base, file, &gone.key)?;
            let after = self.declared(SideTag::Head, file, &added.key)?;
            let (Some(before), Some(after)) = (before, after) else {
                continue;
            };
            pairs.push(SemanticDef {
                key: added.key.clone(),
                change: DefChange::Changed {
                    origin: Origin::Renamed {
                        from: gone.key.clone(),
                    },
                    delta: Delta {
                        signature: before.kind != after.kind || before.signature != after.signature,
                        body: true,
                        ..Delta::default()
                    },
                },
                base: Some(file.clone()),
                head: Some(file.clone()),
            });
            away.push(gone.key.clone());
            away.push(added.key.clone());
        }
        Ok((pairs, away))
    }

    /// The name the answer gives a definition: a script is named by its path, its
    /// definition being the program of the file.
    fn shown_name(&self, kind: DefKind, name: &str, file: &str) -> String {
        if self.family == Family::Shell && kind == DefKind::Module {
            file.to_string()
        } else {
            name.to_string()
        }
    }

    pub(super) fn definition(&self, key: &DefKey) -> Option<&diffctx_resolve::resolve::Definition> {
        self.defs_by_key
            .get(key)
            .map(|at| &self.head.definitions()[*at])
    }

    /// Whether a function is what holds the definition.
    fn inside_function(&self, def: &diffctx_resolve::resolve::Definition) -> bool {
        let Some((name, outer)) = def.key.local.container.split_last() else {
            return false;
        };
        self.defs_in(def.file.as_str()).any(|d| {
            matches!(d.kind, DefKind::Function | DefKind::Method)
                && d.key.local.container.as_slice() == outer
                && &d.key.local.name == name
        })
    }

    /// The class at the head that holds a member, or held one the range removed:
    /// the definition its container names.
    fn class_holding(
        &self,
        file: &str,
        key: &DefKey,
    ) -> Option<&diffctx_resolve::resolve::Definition> {
        let (name, outer) = key.local.container.split_last()?;
        self.defs_in(file).find(|d| {
            d.kind == DefKind::Class
                && d.key.local.container.as_slice() == outer
                && &d.key.local.name == name
        })
    }

    pub(super) fn report_as(
        &self,
        def: &diffctx_resolve::resolve::Definition,
        how: How,
        stale: Vec<DefKey>,
        unpaired: bool,
        holder: bool,
    ) -> Vec<Changed> {
        let mut out = Vec::new();
        let own = |def: &diffctx_resolve::resolve::Definition, reach| Changed {
            name: self.shown_name(def.kind, def.key.local.name.as_str(), def.file.as_str()),
            kind: def.kind,
            file: def.file.as_str().to_string(),
            lines: def.span.lines,
            how,
            subjects: vec![Subject {
                key: def.key.clone(),
                reach,
                within: own_span(def.kind, def.span.lines),
                passing: None,
            }],
            stale: stale.clone(),
            unpaired,
            local: self.inside_function(def),
            public: false,
            basis: None,
            not_layout: false,
            api: false,
            delta: None,
            origin: None,
        };
        let edited = !matches!(how, How::Edited { layout_only: true });
        match def.kind {
            DefKind::Constructor => match self.class_holding(def.file.as_str(), &def.key) {
                Some(class) => {
                    let mut changed = own(class, Reach::Constructor);
                    changed.subjects.push(Subject {
                        key: def.key.clone(),
                        reach: Reach::Every,
                        within: own_span(def.kind, def.span.lines),
                        passing: None,
                    });
                    out.push(changed);
                }
                None => out.push(own(def, Reach::Every)),
            },
            DefKind::Variable | DefKind::Constant | DefKind::Field => {
                // A Python name the range adds has no reader outside it: its
                // holder, or the module, is what changed, not a definition of
                // its own that something is expected to call.
                if !(how == How::Added && self.family == Family::Python) {
                    out.push(own(def, Reach::Every));
                }
                if edited && holder && self.family == Family::Python {
                    if let Some(class) = self.class_holding(def.file.as_str(), &def.key) {
                        let mut changed = own(class, Reach::Construction);
                        changed.stale = Vec::new();
                        changed.unpaired = false;
                        changed.how = How::Edited { layout_only: false };
                        out.push(changed);
                    }
                }
            }
            _ => out.push(own(def, Reach::Every)),
        }
        out
    }

    /// The `Whole` of `d`, whose own tokens held, and of its members on the way.
    /// A member decorated at the head is never a layout edit.
    fn whole(
        &self,
        d: &SemanticDef,
        by_key: &HashMap<&DefKey, &SemanticDef>,
        decorated: &HashSet<DefKey>,
        out: &mut HashMap<DefKey, Whole>,
    ) -> Result<Whole> {
        if let Some(known) = out.get(&d.key) {
            return Ok(*known);
        }
        let (Some(base), Some(head)) = (&d.base, &d.head) else {
            return Ok(Whole::Layout);
        };
        let whole = match (
            self.members(SideTag::Base, base, &d.key)?,
            self.members(SideTag::Head, head, &d.key)?,
        ) {
            (Some(before), Some(after)) => {
                let once = |members: &[(DefKey, bool)], key: &DefKey| {
                    members.iter().filter(|(k, _)| k == key).count() == 1
                };
                let mut kept: HashSet<&DefKey> = HashSet::new();
                let mut accounted = true;
                let mut changed = false;
                for (key, _) in before.iter().chain(&after) {
                    let paired = once(&before, key) && once(&after, key);
                    let same = match by_key.get(key) {
                        None => paired,
                        Some(m) if paired && own_tokens_held(m) && !decorated.contains(key) => {
                            self.whole(m, by_key, decorated, out)? == Whole::Layout
                        }
                        Some(_) => false,
                    };
                    if same {
                        kept.insert(key);
                    } else {
                        changed = true;
                        accounted &= by_key.contains_key(key);
                    }
                }
                let data = |members: &[(DefKey, bool)]| -> Vec<DefKey> {
                    members
                        .iter()
                        .filter(|(key, data)| *data && kept.contains(key))
                        .map(|(key, _)| key.clone())
                        .collect()
                };
                if !accounted || data(&before) != data(&after) {
                    Whole::Unaccounted
                } else if changed {
                    Whole::Members
                } else {
                    Whole::Layout
                }
            }
            _ => Whole::Unaccounted,
        };
        out.insert(d.key.clone(), whole);
        Ok(whole)
    }

    /// The direct members of the definition `key` names in `file` on one side, in
    /// source order, each with whether its place holds data. `None` when the
    /// file does not hold exactly one definition of that name.
    fn members(
        &self,
        side: SideTag,
        file: &RepoPath,
        key: &DefKey,
    ) -> Result<Option<Vec<(DefKey, bool)>>> {
        let Some(facts) = facts_of(&self.ctx, side, &self.cache, file)? else {
            return Ok(None);
        };
        let mut named = facts.defs().filter(|d| d.local == key.local);
        let (Some(outer), None) = (named.next(), named.next()) else {
            return Ok(None);
        };
        Ok(Some(
            members_of(facts.defs(), outer)
                .into_iter()
                .map(|member| (facts.key(&member.local), holds_data(member.kind)))
                .collect(),
        ))
    }

    fn base_definition(
        &self,
        file: &RepoPath,
        key: &DefKey,
    ) -> Result<Option<(DefKind, (u32, u32), bool)>> {
        let Some(facts) = facts_of(&self.ctx, SideTag::Base, &self.cache, file)? else {
            return Ok(None);
        };
        let Some(def) = facts.defs().find(|d| d.local == key.local) else {
            return Ok(None);
        };
        // A member is public when the definition holding it is too.
        let held = key
            .local
            .container
            .split_last()
            .is_none_or(|(name, outer)| {
                facts.defs().any(|c| {
                    c.local.name == *name
                        && c.local.container.as_slice() == outer
                        && c.visibility == Visibility::Public
                })
            });
        Ok(Some((
            def.kind,
            def.span.lines,
            held && def.visibility == Visibility::Public
                && self.head.exports(file.as_str(), &self.ctx.workspace),
        )))
    }
}
