//! The Java resolver on real git repositories: every Java case of the impact
//! and symbol corpus read by the callers it expects, and the shapes of the
//! model it ports (#335, #424, #496, #531). The
//! expectations come from the case files and from reading each fixture; nothing
//! here runs diffctx to learn an answer.
//!
//! The corpus judge pairs the resolver's relations to a case's target with the
//! case's own, by relation kind and resolution, and compares only what the
//! resolver answers: `tests_reach`, `references_path` and the like belong to the
//! layers above it. Readings fixed here:
//! - an `imports` binding is no caller: it is matched when a case asks for it
//!   and is never an extra;
//! - a type use of a definition is a read of it;
//! - a caller that is itself a changed definition may be listed or left out;
//! - in impact mode a target that no change touched is not judged: the cases
//!   assert what the answer reports about the changed definitions.

mod snapshot_fixture;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use _diffctx::facts::{DefKey, Disambig, FactsCache, LocalKey, Name, Scope, semantic_changes};
use _diffctx::resolve::{
    Binding, CandidateReason, EvidenceKind, JavaResolver, Relation, Resolution,
};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: JavaResolver,
    base: JavaResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = JavaResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = JavaResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
    Fixture {
        _repo: repo,
        ctx,
        head,
        base,
    }
}

/// `files` committed, and the repository read as it stands.
fn snapshot(files: &[(&str, &str)]) -> Fixture {
    let repo = Repo::init();
    for (path, text) in files {
        repo.write(path, text);
    }
    let head = repo.commit("snapshot");
    open(repo, RangeSpec::At(Side::Tree(head)))
}

fn dotted(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(Name::as_str).collect();
    parts.push(local.name.as_str());
    parts.join(".")
}

impl Fixture {
    fn where_is(&self, key: &DefKey) -> String {
        let found = self
            .head
            .definitions()
            .iter()
            .chain(self.base.definitions())
            .find(|d| d.key == *key)
            .expect("a resolved target is a definition of the snapshot");
        format!("{}::{}", found.file, dotted(&key.local))
    }

    /// The one binding of `name` on `line` of `file`, imports apart.
    fn at(&self, file: &str, name: &str, line: u32) -> &Binding {
        let found: Vec<&Binding> = self
            .head
            .bindings()
            .iter()
            .filter(|b| {
                b.file.as_str() == file
                    && b.name.as_str() == name
                    && b.site.lines.0 == line
                    && !matches!(
                        b.relation,
                        Relation::Imports | Relation::Reexports | Relation::Overrides
                    )
            })
            .collect();
        assert_eq!(found.len(), 1, "{file}:{line} {name}: {found:#?}");
        found[0]
    }

    /// The imports and re-exports of a file, in source order.
    fn imports_of(&self, file: &str) -> Vec<&Binding> {
        self.head
            .bindings()
            .iter()
            .filter(|b| {
                b.file.as_str() == file
                    && matches!(b.relation, Relation::Imports | Relation::Reexports)
            })
            .collect()
    }

    /// What the binding lands on, as one line.
    fn lands(&self, binding: &Binding) -> String {
        match &binding.resolution {
            Resolution::Resolved { target, .. } => self.where_is(target),
            Resolution::Module { file, .. } => format!("module {file}"),
            Resolution::Candidate { targets, reason } => {
                let mut names: Vec<String> = targets.iter().map(|t| self.where_is(t)).collect();
                names.sort();
                format!("candidate({reason:?}) {}", names.join(", "))
            }
            Resolution::Unresolved { reason } => format!("unresolved({reason:?})"),
        }
    }

    fn lands_at(&self, file: &str, name: &str, line: u32) -> String {
        self.lands(self.at(file, name, line))
    }
}

fn evidence(binding: &Binding) -> (EvidenceKind, Vec<String>) {
    match &binding.resolution {
        Resolution::Resolved { evidence, .. } | Resolution::Module { evidence, .. } => (
            evidence.kind,
            evidence.through.iter().map(|p| p.to_string()).collect(),
        ),
        other => panic!("no evidence on {other:?}"),
    }
}

// ---- the corpus

#[derive(Deserialize, Default)]
struct Files {
    #[serde(default)]
    initial_files: BTreeMap<String, String>,
    #[serde(default)]
    changed_files: BTreeMap<String, String>,
    #[serde(default)]
    deleted_files: Vec<String>,
    #[serde(default)]
    renamed_files: BTreeMap<String, String>,
    #[serde(default)]
    untracked_files: BTreeMap<String, String>,
    #[serde(default)]
    worktree: bool,
}

#[derive(Deserialize, Default)]
struct Query {
    #[serde(default)]
    symbol: Option<String>,
}

#[derive(Deserialize)]
struct Spec {
    from: String,
    to: String,
    kind: String,
    #[serde(default = "resolved")]
    resolution: String,
    #[serde(default)]
    site: Option<u32>,
}

fn resolved() -> String {
    "resolved".into()
}

fn exact() -> bool {
    true
}

#[derive(Deserialize, Default)]
struct Expect {
    #[serde(default)]
    relations: Vec<Spec>,
    #[serde(default)]
    callers: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    decoys: BTreeMap<String, Vec<String>>,
    #[serde(default = "exact")]
    exact: bool,
    #[serde(default)]
    definitions: Option<Vec<String>>,
    #[serde(default)]
    unknowns: Option<BTreeMap<String, String>>,
}

#[derive(Deserialize)]
struct Case {
    mode: String,
    #[serde(default)]
    dispute: Option<String>,
    repo: Files,
    #[serde(default)]
    query: Query,
    expect: Expect,
}

/// A definition (`path::symbol`) or a file (`path`); an empty path names a
/// symbol wherever it is defined.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Node {
    path: String,
    symbol: Option<String>,
}

impl Node {
    fn parse(text: &str) -> Self {
        match text.split_once("::") {
            Some((path, symbol)) => Self {
                path: path.into(),
                symbol: Some(symbol.into()),
            },
            None => Self {
                path: text.into(),
                symbol: None,
            },
        }
    }

    /// A bare identifier is a symbol anywhere; the rest reads as `parse`.
    fn target(text: &str) -> Self {
        let bare = !text.contains("::")
            && !text.is_empty()
            && text.chars().all(|c| c.is_alphanumeric() || c == '_');
        if bare {
            Self {
                path: String::new(),
                symbol: Some(text.into()),
            }
        } else {
            Self::parse(text)
        }
    }

    /// Whether `self`, read as an expectation, names `found`: an empty path or
    /// a missing symbol stands for any, and a member may be written `Class.name`.
    fn covers(&self, found: &Self) -> bool {
        (self.path.is_empty() || self.path == found.path)
            && self.symbol.as_ref().is_none_or(|wanted| {
                found.symbol.as_ref().is_some_and(|have| {
                    have == wanted
                        || wanted
                            .rsplit(['.', ':'])
                            .next()
                            .is_some_and(|last| last == have)
                })
            })
    }

    fn show(&self) -> String {
        match &self.symbol {
            Some(symbol) if self.path.is_empty() => symbol.clone(),
            Some(symbol) => format!("{}::{symbol}", self.path),
            None => self.path.clone(),
        }
    }
}

struct Want {
    from: Node,
    kind: Option<Relation>,
    resolution: String,
    sites: Vec<u32>,
}

fn relation_of(kind: &str) -> Option<Relation> {
    Some(match kind {
        "calls" => Relation::Calls,
        "reads" => Relation::Reads,
        "writes" => Relation::Writes,
        "instantiates" => Relation::Instantiates,
        "passes_as_value" => Relation::PassesAsValue,
        "overrides" => Relation::Overrides,
        "type_use" => Relation::TypeUses,
        "imports" => Relation::Imports,
        "reexports" => Relation::Reexports,
        _ => return None,
    })
}

/// The expectation as wants per target. A target listed with no want asserts
/// that nothing reaches it. One caller reaching the target on several lines is
/// one want with all the lines.
fn compile(expect: &Expect) -> BTreeMap<Node, Vec<Want>> {
    let mut wants: BTreeMap<Node, Vec<Want>> = BTreeMap::new();
    for spec in &expect.relations {
        let Some(kind) = relation_of(&spec.kind) else {
            continue;
        };
        let to = Node::parse(&spec.to);
        let from = Node::parse(&spec.from);
        let list = wants.entry(to).or_default();
        let twin = list.iter_mut().find(|w| {
            from.symbol.is_some()
                && w.from == from
                && w.kind == Some(kind)
                && w.resolution == spec.resolution
        });
        match twin {
            Some(twin) => twin.sites.extend(spec.site),
            None => list.push(Want {
                from,
                kind: Some(kind),
                resolution: spec.resolution.clone(),
                sites: spec.site.into_iter().collect(),
            }),
        }
    }
    for (target, froms) in &expect.callers {
        let list = wants.entry(Node::target(target)).or_default();
        for from in froms {
            list.push(Want {
                from: Node::parse(from),
                kind: None,
                resolution: "resolved".into(),
                sites: Vec::new(),
            });
        }
    }
    wants
}

/// One caller of a target: a definition (or a file's own top level) reaching it
/// in one relation and resolution, on these lines.
struct Group {
    from: Node,
    kind: Relation,
    resolution: &'static str,
    lines: BTreeSet<u32>,
    /// The caller is itself a changed definition.
    in_diff: bool,
    /// Reached through a glob's pattern: the case asks for the import, and the
    /// calls through the loaded module are the same fact.
    by_glob: bool,
}

/// The definitions the range changed, by file and dotted name.
struct Changed(HashSet<(String, String)>);

fn changed_in(fixture: &Fixture) -> Changed {
    let cache = FactsCache::disabled();
    let set = semantic_changes(&fixture.ctx, &cache).expect("semantic changes");
    let mut changed = HashSet::new();
    for def in set.defs {
        for path in [def.base.as_ref(), def.head.as_ref()].into_iter().flatten() {
            changed.insert((path.as_str().to_string(), dotted(&def.key.local)));
        }
    }
    Changed(changed)
}

fn owner(scope: &Scope) -> Option<&LocalKey> {
    match scope {
        Scope::Def(key) => Some(key),
        Scope::Callback { parent, .. } => owner(parent),
        Scope::Module => None,
    }
}

/// A definition a node names, in the head or only in the base, and the resolver
/// that holds it.
struct Named<'f> {
    resolver: &'f JavaResolver,
    key: DefKey,
    file: String,
}

fn definitions_of<'f>(fixture: &'f Fixture, node: &Node) -> Vec<Named<'f>> {
    let named = |resolver: &'f JavaResolver| -> Vec<Named<'f>> {
        resolver
            .definitions()
            .iter()
            .filter(|d| {
                node.covers(&Node {
                    path: d.file.to_string(),
                    symbol: Some(d.key.local.name.to_string()),
                })
            })
            .map(|d| Named {
                resolver,
                key: d.key.clone(),
                file: d.file.to_string(),
            })
            .collect()
    };
    let in_head = named(&fixture.head);
    let head_keys: Vec<DefKey> = in_head.iter().map(|n| n.key.clone()).collect();
    let mut found = in_head;
    for only_base in named(&fixture.base) {
        if !head_keys.contains(&only_base.key) {
            found.push(only_base);
        }
    }
    found
}

/// No change touched any definition the node names.
fn untouched(fixture: &Fixture, node: &Node, changed: &Changed) -> bool {
    let named = definitions_of(fixture, node);
    !named.is_empty()
        && named
            .iter()
            .all(|n| !changed.0.contains(&(n.file.clone(), dotted(&n.key.local))))
}

fn groups(fixture: &Fixture, node: &Node, changed: &Changed) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for named in definitions_of(fixture, node) {
        for binding in named.resolver.references_to(&named.key) {
            // A type use is its own relation (R9), never a read of the type it names.
            let relation = binding.relation;
            if !relation.uses()
                && !matches!(
                    relation,
                    Relation::Imports | Relation::Reexports | Relation::TypeUses
                )
            {
                continue;
            }
            let resolution = match binding.resolution {
                Resolution::Resolved { .. } => "resolved",
                Resolution::Candidate { .. } => "candidate",
                _ => continue,
            };
            let local = owner(&binding.from);
            let from = Node {
                path: binding.file.to_string(),
                symbol: local.map(|l| l.name.to_string()),
            };
            let in_diff =
                local.is_some_and(|l| changed.0.contains(&(binding.file.to_string(), dotted(l))));
            let line = binding.site.lines.0;
            let by_glob = matches!(
                binding.resolution,
                Resolution::Candidate {
                    reason: CandidateReason::ImportGlob,
                    ..
                }
            );
            let twin = groups
                .iter_mut()
                .find(|g| g.from == from && g.kind == relation && g.resolution == resolution);
            match twin {
                Some(twin) => {
                    twin.lines.insert(line);
                }
                None => groups.push(Group {
                    from,
                    kind: relation,
                    resolution,
                    lines: BTreeSet::from([line]),
                    in_diff,
                    by_glob,
                }),
            }
        }
    }
    groups
}

fn judge_target(
    fixture: &Fixture,
    target: &Node,
    wants: &[Want],
    changed: &Changed,
    exact: bool,
    failures: &mut Vec<String>,
) {
    let groups = groups(fixture, target, changed);
    let mut taken = vec![false; groups.len()];
    let mut lines: Vec<String> = Vec::new();
    for want in wants {
        let mut hits = 0;
        for (i, group) in groups.iter().enumerate() {
            let agrees = want.from.covers(&group.from)
                && want.resolution == group.resolution
                && want.kind.is_none_or(|k| k == group.kind)
                && want.sites.iter().all(|s| group.lines.contains(s));
            if !agrees || (taken[i] && want.sites.is_empty()) {
                continue;
            }
            taken[i] = true;
            hits += 1;
            if want.from.symbol.is_some() {
                break;
            }
        }
        if hits == 0 {
            let kind = want.kind.map_or("*", Relation::name);
            lines.push(format!(
                "  - missing {} -{kind}-> {} [{}{}]",
                want.from.show(),
                target.show(),
                want.resolution,
                if want.sites.is_empty() {
                    String::new()
                } else {
                    format!(", line {:?}", want.sites)
                }
            ));
        }
    }
    for (i, group) in groups.iter().enumerate() {
        // A caller that is itself changed may be listed or left out; an import
        // is listed only when a case asks for it.
        let listed_kind = group.kind.uses() || group.kind == Relation::TypeUses;
        if !taken[i] && exact && !group.in_diff && !group.by_glob && listed_kind {
            lines.push(format!(
                "  + extra   {} -{}-> {} [{}, line {:?}]",
                group.from.show(),
                group.kind.name(),
                target.show(),
                group.resolution,
                group.lines
            ));
        }
    }
    if !lines.is_empty() {
        failures.push(format!(
            "relations to `{}`:\n{}",
            target.show(),
            lines.join("\n")
        ));
    }
}

fn judge_decoys(fixture: &Fixture, expect: &Expect, changed: &Changed, failures: &mut Vec<String>) {
    for (key, decoys) in &expect.decoys {
        let target = Node::target(key);
        let groups = groups(fixture, &target, changed);
        for decoy in decoys {
            let decoy = Node::parse(decoy);
            for group in groups.iter().filter(|g| decoy.covers(&g.from)) {
                failures.push(format!(
                    "decoy `{}` is related to `{key}`: {} -{}-> [{}, line {:?}]",
                    decoy.show(),
                    group.from.show(),
                    group.kind.name(),
                    group.resolution,
                    group.lines
                ));
            }
        }
    }
}

/// The definitions of the symbol the case asks for, by file and name; a member
/// may be written `Class.name`. `PATH:NAME` names the definitions of that file
/// alone (the query is qualified), a bare name every definition of it.
fn judge_definitions(fixture: &Fixture, case: &Case, failures: &mut Vec<String>) {
    let (Some(expected), Some(symbol)) = (&case.expect.definitions, &case.query.symbol) else {
        return;
    };
    let (only_in, asked) = match symbol.rsplit_once(':') {
        Some((path, name)) => (Some(path), name),
        None => (None, symbol.as_str()),
    };
    let found: BTreeSet<String> = fixture
        .head
        .definitions()
        .iter()
        .filter(|d| d.key.local.name.as_str() == asked)
        .filter(|d| only_in.is_none_or(|path| d.file.as_str() == path))
        .map(|d| format!("{}::{}", d.file, d.key.local.name))
        .collect();
    let wanted: BTreeSet<String> = expected
        .iter()
        .map(|d| {
            let node = Node::parse(d);
            let name = node.symbol.unwrap_or_default();
            format!(
                "{}::{}",
                node.path,
                name.rsplit('.').next().unwrap_or(&name)
            )
        })
        .collect();
    if found != wanted {
        failures.push(format!("definitions: expected {wanted:?}, found {found:?}"));
    }
}

/// `unknowns: {}`: nothing the resolver left as an unknown is named like a
/// definition the range changed.
fn judge_unknowns(fixture: &Fixture, case: &Case, changed: &Changed, failures: &mut Vec<String>) {
    if !case
        .expect
        .unknowns
        .as_ref()
        .is_some_and(BTreeMap::is_empty)
    {
        return;
    }
    let names: HashSet<&str> = changed
        .0
        .iter()
        .filter_map(|(_, dotted)| dotted.rsplit('.').next())
        .collect();
    for binding in fixture.head.bindings() {
        let unknown = match &binding.resolution {
            Resolution::Unresolved { reason } => reason.is_unknown(),
            _ => false,
        };
        if unknown && names.contains(binding.name.as_str()) {
            failures.push(format!(
                "unknown `{}` at {}:{}: {}",
                binding.name,
                binding.file,
                binding.site.lines.0,
                fixture.lands(binding)
            ));
        }
    }
}

fn build(case: &Case) -> Fixture {
    let repo = Repo::init();
    let files = &case.repo;
    for (path, text) in &files.initial_files {
        repo.write(path, text);
    }
    let base = repo.commit("base");
    if case.mode == "symbol" {
        return open(repo, RangeSpec::At(Side::Tree(base)));
    }
    for (old, new) in &files.renamed_files {
        let text = files.initial_files.get(old).expect("a renamed file exists");
        repo.remove(old);
        repo.write(new, text);
    }
    for path in &files.deleted_files {
        repo.remove(path);
    }
    for (path, text) in &files.changed_files {
        repo.write(path, text);
    }
    if files.worktree {
        for (path, text) in &files.untracked_files {
            repo.write(path, text);
        }
        let range = working_tree(&repo.root).expect("a working-tree range");
        return open(repo, range);
    }
    let head = repo.commit("head");
    for (path, text) in &files.untracked_files {
        repo.write(path, text);
    }
    open(repo, RangeSpec::Review { base, head })
}

enum Verdict {
    /// The case asserts nothing the resolver answers.
    NotApplicable,
    Green,
    Red(Vec<String>),
}

fn run_case(case: &Case) -> Verdict {
    let wants = compile(&case.expect);
    let mut targets: BTreeSet<Node> = wants.keys().cloned().collect();
    targets.extend(case.expect.decoys.keys().map(|k| Node::target(k)));
    let asks_unknowns = case
        .expect
        .unknowns
        .as_ref()
        .is_some_and(BTreeMap::is_empty);
    if targets.is_empty() && case.expect.definitions.is_none() && !asks_unknowns {
        return Verdict::NotApplicable;
    }
    let fixture = build(case);
    let changed = changed_in(&fixture);
    let judged = |target: &Node| case.mode != "impact" || !untouched(&fixture, target, &changed);
    let mut failures = Vec::new();
    for (target, list) in &wants {
        if judged(target) {
            judge_target(
                &fixture,
                target,
                list,
                &changed,
                case.expect.exact,
                &mut failures,
            );
        }
    }
    // A decoy key that is not also a listed target asserts only that its decoys are
    // unrelated (`judge_decoys`): other callers of it are no one's concern here.
    let decoys = Expect {
        decoys: case
            .expect
            .decoys
            .iter()
            .filter(|(key, _)| judged(&Node::target(key)))
            .map(|(key, list)| (key.clone(), list.clone()))
            .collect(),
        ..Expect::default()
    };
    judge_decoys(&fixture, &decoys, &changed, &mut failures);
    judge_definitions(&fixture, case, &mut failures);
    judge_unknowns(&fixture, case, &changed, &mut failures);
    if failures.is_empty() {
        Verdict::Green
    } else {
        Verdict::Red(failures)
    }
}

fn cases_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the repository root")
        .join("tests")
        .join("cases")
}

/// The Java cases the resolver cannot answer yet, each with the reason. The
/// list is held both ways: a listed case that turns green fails the run, as does
/// a red case that is not listed.
const RED: &[(&str, &str)] = &[
    (
        "impact/java/gh704_sole_implementor_called_through_a_subinterface_has_a_test_link",
        "gh#704: a call through a subinterface of the implemented interface states no candidate \
         binding to the sole implementor; the resolver leaves it a gap with a site",
    ),
    (
        "impact/java/gh732_constant_appended_to_a_constants_class_reaches_only_its_readers",
        "gh#732: no resolver capability is missing: the resolver states every read of a member of \
         the class, and the class-keyed decoys are withheld by impact, which lists the appended \
         constant's readers instead of the class's, so only the full pipeline judges them",
    ),
    (
        "impact/java/gh734_additive_dto_field_does_not_list_the_class_users",
        "gh#734: no resolver capability is missing: the resolver states every use of the class, \
         and the class-keyed decoys are withheld by impact, which does not list the users of a \
         class for an added field, so only the full pipeline judges them",
    ),
];

#[test]
fn the_java_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("java");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/java/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 7, "the walk found {} cases", cases.len());
    let (mut green, mut idle, mut disputed) = (0, 0, 0);
    let mut unexpected: Vec<String> = Vec::new();
    let mut stale: Vec<String> = Vec::new();
    for (name, path) in &cases {
        let raw = fs::read_to_string(path).expect("a case file");
        let case: Case =
            serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("{name} does not parse: {e}"));
        if case.dispute.is_some() {
            disputed += 1;
            continue;
        }
        let listed = RED.iter().find(|(n, _)| n == name);
        match (run_case(&case), listed) {
            (Verdict::NotApplicable, _) => idle += 1,
            (Verdict::Green, None) => green += 1,
            (Verdict::Green, Some((_, reason))) => {
                stale.push(format!("{name} is green now; drop it from RED ({reason})"));
            }
            (Verdict::Red(_), Some(_)) => {}
            (Verdict::Red(failures), None) => {
                unexpected.push(format!("{name}:\n{}", failures.join("\n")));
            }
        }
    }
    eprintln!(
        "java corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
         resolver answers, {disputed} disputed",
        cases.len(),
        RED.len()
    );
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "red but not listed:\n{}\nlisted but not red:\n{}",
        unexpected.join("\n\n"),
        stale.join("\n")
    );
}

// ---- the shapes

/// An import binds the name it states, an alias under the alias, and a wildcard
/// the declarations of its package; a name no import or package gives is not
/// guessed.

// ---- the shapes

fn target(binding: &Binding) -> &DefKey {
    match &binding.resolution {
        Resolution::Resolved { target, .. } => target,
        other => panic!("not resolved: {other:?}"),
    }
}

/// An import binds the type it names; a type written, constructed or called
/// through is that type; a name no import or package gives is not guessed.
#[test]
fn imports_and_packages_bind_types_and_static_calls() {
    let fx = snapshot(&[
        (
            "a/Util.java",
            "package a;\n\npublic class Util {\n    public static String slug(String s) {\n        return s;\n    }\n}\n",
        ),
        (
            "b/App.java",
            "package b;\n\nimport a.Util;\n\npublic class App {\n    String go() {\n        return Util.slug(\"x\");\n    }\n\n    Util make() {\n        return new Util();\n    }\n}\n",
        ),
    ]);
    let f = "b/App.java";
    assert_eq!(fx.lands_at(f, "slug", 7), "a/Util.java::Util.slug");
    assert_eq!(fx.at(f, "slug", 7).relation, Relation::Calls);
    assert_eq!(fx.lands_at(f, "Util", 11), "a/Util.java::Util");
    assert_eq!(fx.at(f, "Util", 11).relation, Relation::Instantiates);
    assert_eq!(fx.lands_at(f, "Util", 10), "a/Util.java::Util");
    assert_eq!(fx.at(f, "Util", 10).relation, Relation::TypeUses);
    let imports: Vec<String> = fx.imports_of(f).into_iter().map(|b| fx.lands(b)).collect();
    assert_eq!(imports, ["a/Util.java::Util"]);
}

/// A static import brings a member in by its name, an on-demand static import
/// every static member of the type; an on-demand import of a package brings its
/// types.
#[test]
fn static_and_on_demand_imports_bring_members_and_types() {
    let fx = snapshot(&[
        (
            "a/Util.java",
            "package a;\n\npublic class Util {\n    public static String slug(String s) {\n        return s;\n    }\n\n    public static int twice(int x) {\n        return x * 2;\n    }\n}\n",
        ),
        (
            "b/Static.java",
            "package b;\n\nimport static a.Util.slug;\nimport static a.Util.*;\n\nclass Static {\n    String go() {\n        return slug(\"x\");\n    }\n\n    int more() {\n        return twice(2);\n    }\n}\n",
        ),
        (
            "app/Main.java",
            "package app;\n\nimport a.*;\n\nclass Main {\n    int run() {\n        return Util.twice(2);\n    }\n}\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("b/Static.java", "slug", 8),
        "a/Util.java::Util.slug"
    );
    assert_eq!(
        fx.lands_at("b/Static.java", "twice", 12),
        "a/Util.java::Util.twice"
    );
    assert_eq!(
        evidence(fx.at("b/Static.java", "twice", 12)).0,
        EvidenceKind::StarImport
    );
    assert_eq!(
        fx.lands_at("app/Main.java", "twice", 7),
        "a/Util.java::Util.twice"
    );
}

/// A receiver is typed by what is written: a parameter, a local, a field (by
/// name, through `this`), `var` and what it is initialised with; a method of a
/// base class is found through `extends`.
#[test]
fn receivers_are_typed_by_declarations_and_initialisers() {
    let fx = snapshot(&[(
        "t/Typing.java",
        "package t;\n\nclass Repo {\n    String find(int id) {\n        return \"r\";\n    }\n}\n\nclass Base {\n    int shared() {\n        return 1;\n    }\n}\n\nclass Child extends Base {\n    private final Repo repo = new Repo();\n\n    String use(Repo given) {\n        Repo local = new Repo();\n        var made = new Repo();\n        String a = given.find(1);\n        String b = local.find(2);\n        String c = made.find(3);\n        String d = repo.find(4);\n        String e = this.repo.find(5);\n        return a + b + c + d + e + shared();\n    }\n}\n",
    )]);
    let f = "t/Typing.java";
    for line in [21, 22, 23, 24, 25] {
        assert_eq!(
            fx.lands_at(f, "find", line),
            "t/Typing.java::Repo.find",
            "line {line}"
        );
    }
    assert_eq!(
        evidence(fx.at(f, "find", 21)).0,
        EvidenceKind::TypedReceiver
    );
    assert_eq!(evidence(fx.at(f, "find", 23)).0, EvidenceKind::Constructed);
    assert_eq!(evidence(fx.at(f, "find", 25)).0, EvidenceKind::SelfReceiver);
    assert_eq!(fx.lands_at(f, "shared", 26), "t/Typing.java::Base.shared");
    assert_eq!(evidence(fx.at(f, "shared", 26)).0, EvidenceKind::Inherited);
}

/// Overloads the argument count settles are one definition; ones it does not
/// are a candidate each.
#[test]
fn overloads_are_settled_by_the_argument_count_or_candidates() {
    let fx = snapshot(&[(
        "o/Over.java",
        "package o;\n\nclass Over {\n    int add(int a) {\n        return a;\n    }\n\n    int add(int a, int b) {\n        return a + b;\n    }\n\n    void use() {\n        add(1);\n        add(1, 2);\n        add(1, 2, 3);\n    }\n}\n",
    )]);
    let f = "o/Over.java";
    assert_eq!(target(fx.at(f, "add", 13)).local.disambig, None);
    assert_eq!(
        target(fx.at(f, "add", 14)).local.disambig,
        Some(Disambig::Overload(1))
    );
    assert_eq!(
        fx.lands_at(f, "add", 15),
        "candidate(Overloaded) o/Over.java::Over.add, o/Over.java::Over.add"
    );
}

/// A call through a supertype may run in an implementation; through a type
/// nothing below it replaces, it is the member. `@Override` binds to the member
/// it replaces.
#[test]
fn a_call_through_a_supertype_is_a_candidate_on_its_implementations() {
    let fx = snapshot(&[(
        "d/Disp.java",
        "package d;\n\ninterface Repo {\n    String find(int id);\n}\n\nclass Mem implements Repo {\n    @Override public String find(int id) {\n        return \"m\";\n    }\n}\n\nclass Sql implements Repo {\n    public String find(int id) {\n        return \"s\";\n    }\n}\n\nclass Use {\n    String through(Repo repo) {\n        return repo.find(1);\n    }\n\n    String direct(Sql sql) {\n        return sql.find(2);\n    }\n}\n",
    )]);
    assert_eq!(
        fx.lands_at("d/Disp.java", "find", 21),
        "candidate(Dispatch) d/Disp.java::Mem.find, d/Disp.java::Repo.find, d/Disp.java::Sql.find"
    );
    assert_eq!(
        fx.lands_at("d/Disp.java", "find", 25),
        "d/Disp.java::Sql.find"
    );
    let overrides: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.relation == Relation::Overrides)
        .collect();
    assert_eq!(overrides.len(), 1, "{overrides:#?}");
    assert_eq!(fx.lands(overrides[0]), "d/Disp.java::Repo.find");
}

/// A call through a receiver nothing types is a candidate for the methods of
/// that name the file can hold a value of.
#[test]
fn untyped_receivers_are_candidates() {
    let fx = snapshot(&[(
        "u/Untyped.java",
        "package u;\n\nclass Box {\n    void open() {\n    }\n}\n\nclass Use {\n    void run(java.util.List<Object> items) {\n        items.forEach(x -> x.open());\n    }\n}\n",
    )]);
    assert_eq!(
        fx.lands_at("u/Untyped.java", "open", 10),
        "candidate(UntypedReceiver) u/Untyped.java::Box.open"
    );
}

/// `private` stops at the file and a package-private member at its package.
#[test]
fn private_and_package_private_stop_where_java_stops_them() {
    let fx = snapshot(&[
        (
            "p/A.java",
            "package p;\n\npublic class A {\n    private int secret() {\n        return 1;\n    }\n\n    int pkg() {\n        return 2;\n    }\n}\n",
        ),
        (
            "q/B.java",
            "package q;\n\nimport p.A;\n\nclass B {\n    int go(A a) {\n        int x = a.pkg();\n        return a.secret() + x;\n    }\n}\n",
        ),
    ]);
    assert_eq!(fx.lands_at("q/B.java", "pkg", 7), "unresolved(NoMember)");
    assert_eq!(fx.lands_at("q/B.java", "secret", 8), "unresolved(NoMember)");
}

/// Kotlin reads Java as it reads its own: a static member is a member of the
/// type, a getter `isX()` is the property, `getX()` and `setX(v)` are the property
/// `x` read and written (a write is a use of the setter).
#[test]
fn kotlin_calls_java_statics_and_accessors() {
    let java = "package shop;\n\npublic class Pricing {\n    public static int total(int[] items) {\n        return items.length;\n    }\n\n    public boolean isActive() {\n        return true;\n    }\n\n    public String getName() {\n        return \"p\";\n    }\n\n    public void setName(String name) {\n    }\n}\n";
    let kotlin = "package shop\n\nfun charge(items: IntArray, p: Pricing): Int {\n    val t = Pricing.total(items)\n    val a = p.isActive\n    val n = p.name\n    p.name = \"x\"\n    return t\n}\n";
    let fx = snapshot(&[
        ("src/main/java/shop/Pricing.java", java),
        ("src/main/kotlin/shop/Checkout.kt", kotlin),
    ]);
    let (call, jv) = (
        "src/main/kotlin/shop/Checkout.kt",
        "src/main/java/shop/Pricing.java",
    );
    let expected = [
        ("total", 4, "Pricing.total"),
        ("isActive", 5, "Pricing.isActive"),
        ("name", 6, "Pricing.getName"),
        ("name", 7, "Pricing.setName"),
    ];
    for (name, line, lands) in expected {
        assert_eq!(
            fx.lands_at(call, name, line),
            format!("{jv}::{lands}"),
            "{name} at line {line}"
        );
    }
    assert_eq!(fx.at(call, "name", 7).relation, Relation::Writes);
}

/// A Kotlin import names a Java static through its type, as Java writes it, and
/// the name it brings into the file is the alias when it states one.
#[test]
fn kotlin_imports_a_java_static_by_its_name_or_under_an_alias() {
    let java = "package shop;\n\npublic class Pricing {\n    public static int total(int[] items) {\n        return items.length;\n    }\n\n    public static int roundedTotal(int amount) {\n        return amount;\n    }\n}\n";
    let kotlin = "package app\n\nimport shop.Pricing.roundedTotal as rt\nimport shop.Pricing.total\n\nfun charge(items: IntArray): Int {\n    val t = total(items)\n    return rt(t)\n}\n";
    let fx = snapshot(&[
        ("src/main/java/shop/Pricing.java", java),
        ("src/main/kotlin/app/Checkout.kt", kotlin),
    ]);
    let (call, jv) = (
        "src/main/kotlin/app/Checkout.kt",
        "src/main/java/shop/Pricing.java",
    );
    assert_eq!(
        fx.lands_at(call, "total", 7),
        format!("{jv}::Pricing.total")
    );
    assert_eq!(
        fx.lands_at(call, "rt", 8),
        format!("{jv}::Pricing.roundedTotal")
    );
}

/// A Java definition used only by Kotlin is no unused one.
#[test]
fn a_java_definition_has_its_kotlin_callers() {
    let fx = snapshot(&[
        (
            "src/main/java/shop/Pricing.java",
            "package shop;\n\npublic class Pricing {\n    public static int total(int[] items) {\n        return items.length;\n    }\n}\n",
        ),
        (
            "src/main/kotlin/shop/Checkout.kt",
            "package shop\n\nfun charge(items: IntArray): Int = Pricing.total(items)\n",
        ),
    ]);
    let total = fx
        .head
        .definitions()
        .iter()
        .find(|d| d.key.local.name.as_str() == "total")
        .expect("the Java definition");
    let callers: Vec<(String, Relation)> = fx
        .head
        .references_to(&total.key)
        .iter()
        .map(|b| (b.file.to_string(), b.relation))
        .collect();
    assert_eq!(
        callers,
        [(
            "src/main/kotlin/shop/Checkout.kt".to_string(),
            Relation::Calls
        )]
    );
}
