//! The C++ cases of the C-family resolver on real git repositories: every C++ case of the impact
//! and symbol corpus read by the callers it expects, and the shapes of the
//! model it states. The
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
use _diffctx::resolve::{Binding, CResolver, CandidateReason, Relation, Resolution};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: CResolver,
    base: CResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = CResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = CResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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

/// The definitions the range changed by file and dotted name, and by file and key
/// (a twin carries its `#n`).
struct Changed(HashSet<(String, String)>, HashSet<(String, String)>);

fn twin_key(local: &LocalKey) -> String {
    match &local.disambig {
        Some(Disambig::Overload(n)) => format!("{}#{n}", dotted(local)),
        _ => dotted(local),
    }
}

fn changed_in(fixture: &Fixture) -> Changed {
    let cache = FactsCache::disabled();
    let set = semantic_changes(&fixture.ctx, &cache).expect("semantic changes");
    let mut changed = HashSet::new();
    let mut keyed = HashSet::new();
    for def in set.defs {
        for path in [def.base.as_ref(), def.head.as_ref()].into_iter().flatten() {
            changed.insert((path.as_str().to_string(), dotted(&def.key.local)));
            keyed.insert((path.as_str().to_string(), twin_key(&def.key.local)));
        }
    }
    Changed(changed, keyed)
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
    resolver: &'f CResolver,
    key: DefKey,
    file: String,
}

fn definitions_of<'f>(fixture: &'f Fixture, node: &Node) -> Vec<Named<'f>> {
    let named = |resolver: &'f CResolver| -> Vec<Named<'f>> {
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
    // Overloads share a name: of them, the ones the range changed are the target.
    let named = definitions_of(fixture, node);
    let touched = named
        .iter()
        .filter(|n| {
            changed
                .1
                .contains(&(n.file.clone(), twin_key(&n.key.local)))
        })
        .count();
    for named in named {
        if touched > 0
            && !changed
                .1
                .contains(&(named.file.clone(), twin_key(&named.key.local)))
        {
            continue;
        }
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

/// The C++ cases the resolver cannot answer yet, each with the reason. The
/// list is held both ways: a listed case that turns green fails the run, as does
/// a red case that is not listed.
const RED: &[(&str, &str)] = &[(
    "impact/cpp/cpp_removed_field_read_in_the_middle_of_a_chain_is_not_absent",
    "the candidate is the removal side's: a read through a receiver nothing types is `UntypedRead` for the resolver, and decide() names the surviving read of a removed member's name as a possible use; no binding states it",
)];

#[test]
fn the_cpp_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("cpp");
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/cpp/{stem}"), path.to_path_buf()));
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
        "cpp corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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

/// Every binding of `name` on a line, with its relation, in the resolver's order.
fn rel_at(fx: &Fixture, file: &str, name: &str, line: u32) -> Vec<(Relation, String)> {
    fx.head
        .bindings()
        .iter()
        .filter(|b| {
            b.file.as_str() == file
                && b.name.as_str() == name
                && b.site.lines.0 == line
                && b.relation != Relation::Imports
        })
        .map(|b| (b.relation, fx.lands(b)))
        .collect()
}

/// A name written with its namespace, one a `using` declaration brings and one
/// a directive brings land on the definition and on the declaration beside it.
#[test]
fn namespaces_and_using_bind_names() {
    let fx = snapshot(&[
        (
            "src/geo.hpp",
            "namespace geo {\nint dist(int a, int b);\n}\n",
        ),
        (
            "src/geo.cpp",
            "#include \"geo.hpp\"\n\nint geo::dist(int a, int b)\n{\n    return a - b;\n}\n",
        ),
        (
            "src/use.cpp",
            "#include \"geo.hpp\"\n\nint f()\n{\n    return geo::dist(1, 2);\n}\n\nnamespace app {\nusing geo::dist;\n\nint g()\n{\n    return dist(3, 4);\n}\n}\n",
        ),
    ]);
    let both = vec![
        (Relation::Calls, "src/geo.cpp::geo.dist".to_string()),
        (Relation::Calls, "src/geo.hpp::geo.dist".to_string()),
    ];
    assert_eq!(rel_at(&fx, "src/use.cpp", "dist", 5), both);
    assert_eq!(rel_at(&fx, "src/use.cpp", "dist", 13), both);
}

/// Members are found on the class, on the base a class lists, through the
/// implicit `this`; a virtual one reached through a base is also what overrides
/// it.
#[test]
fn members_are_found_on_classes_and_bases_and_virtual_calls_are_candidates() {
    let fx = snapshot(&[
        (
            "src/a.hpp",
            "class Base {\npublic:\n    int id() const { return 1; }\n    virtual int run() { return 2; }\n};\n\nclass Derived : public Base {\npublic:\n    int run() override { return id() + helper(); }\n    int helper() { return 3; }\n};\n",
        ),
        (
            "src/b.cpp",
            "#include \"a.hpp\"\n\nint go(Base *b)\n{\n    return b->run();\n}\n",
        ),
    ]);
    assert_eq!(fx.lands_at("src/a.hpp", "id", 9), "src/a.hpp::Base.id");
    assert_eq!(
        fx.lands_at("src/a.hpp", "helper", 9),
        "src/a.hpp::Derived.helper"
    );
    assert_eq!(
        fx.lands_at("src/b.cpp", "run", 5),
        "candidate(Dispatch) src/a.hpp::Base.run, src/a.hpp::Derived.run"
    );
}

/// Overloads are settled by the number of arguments where that settles them and
/// are candidates where it does not.
#[test]
fn overloads_are_settled_by_the_argument_count_or_are_candidates() {
    let fx = snapshot(&[(
        "src/o.cpp",
        "int f(int a) { return a; }\n\nint f(int a, int b = 2) { return a + b; }\n\nint x() { return f(1); }\n\nint y() { return f(1, 2); }\n\nint z() { return f(1, 2, 3); }\n",
    )]);
    let both = "candidate(Overloaded) src/o.cpp::f, src/o.cpp::f";
    assert_eq!(fx.lands_at("src/o.cpp", "f", 5), both);
    assert_eq!(fx.lands_at("src/o.cpp", "f", 7), "src/o.cpp::f");
    assert_eq!(fx.lands_at("src/o.cpp", "f", 9), both);
}

/// An object declared by value and one made with `new` are instantiations of its
/// class; a member is found through either.
#[test]
fn objects_are_instantiations_and_their_members_resolve() {
    let fx = snapshot(&[
        (
            "src/s.hpp",
            "class Shape {\npublic:\n    Shape(int n);\n    int sides() const { return n_; }\nprivate:\n    int n_;\n};\n",
        ),
        (
            "src/s.cpp",
            "#include \"s.hpp\"\n\nShape::Shape(int n) : n_(n) {}\n\nint make()\n{\n    Shape s(3);\n    Shape *p = new Shape(4);\n    return s.sides() + p->sides();\n}\n",
        ),
    ]);
    let class = "src/s.hpp::Shape".to_string();
    assert_eq!(
        rel_at(&fx, "src/s.cpp", "Shape", 7),
        vec![
            (Relation::TypeUses, class.clone()),
            (Relation::Instantiates, class.clone())
        ]
    );
    assert_eq!(
        rel_at(&fx, "src/s.cpp", "Shape", 8),
        vec![
            (Relation::TypeUses, class.clone()),
            (Relation::Instantiates, class)
        ]
    );
    let sides = (Relation::Calls, "src/s.hpp::Shape.sides".to_string());
    assert_eq!(
        rel_at(&fx, "src/s.cpp", "sides", 9),
        vec![sides.clone(), sides]
    );
}

/// A receiver typed by a name the resolver cannot follow (a namespace alias of
/// an alias) is no library type when the project defines that name: its member
/// is a candidate among the project's methods of that name.
#[test]
fn an_unfollowed_type_the_project_defines_is_no_library_receiver() {
    let fx = snapshot(&[
        (
            "src/vec.hpp",
            "namespace geo {\nclass Vec {\npublic:\n    int dot(const Vec& o) const { return 1; }\n};\n}\n",
        ),
        (
            "src/use.cpp",
            "#include \"vec.hpp\"\n\nnamespace g = geo;\nnamespace h = g;\nint run(const h::Vec& a) { return a.dot(a); }\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("src/use.cpp", "dot", 5),
        "candidate(UntypedReceiver) src/vec.hpp::geo.Vec.dot"
    );
}
