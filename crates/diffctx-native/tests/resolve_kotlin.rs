//! The Kotlin resolver on real git repositories: every Kotlin case of the impact
//! and symbol corpus read by the callers it expects, and the shapes of the
//! model it ports (#335, #526, #561, #562, #566, #649, #664, #687). The
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

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name, Scope, semantic_changes};
use _diffctx::resolve::{
    Binding, CandidateReason, EvidenceKind, KotlinResolver, Relation, Resolution,
};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: KotlinResolver,
    base: KotlinResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = KotlinResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = KotlinResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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
    resolver: &'f KotlinResolver,
    key: DefKey,
    file: String,
}

fn definitions_of<'f>(fixture: &'f Fixture, node: &Node) -> Vec<Named<'f>> {
    let named = |resolver: &'f KotlinResolver| -> Vec<Named<'f>> {
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

/// The definitions of the symbol the case asks for, by file and name. The query
/// reads as `--symbol` does: `PATH:NAME` names the definitions of that file
/// alone, `Owner.NAME` those whose innermost container is `Owner`, a bare name
/// every definition of it, and a Kotlin name may be written in backticks. The
/// JVM allows none of `:` and `.` in a name, so the separators split it whole.
fn judge_definitions(fixture: &Fixture, case: &Case, failures: &mut Vec<String>) {
    let (Some(expected), Some(symbol)) = (&case.expect.definitions, &case.query.symbol) else {
        return;
    };
    let (only_in, rest) = match symbol.rsplit_once(':') {
        Some((path, rest)) => (Some(path), rest),
        None => (None, symbol.as_str()),
    };
    let (owner, name) = match rest.rsplit_once('.') {
        Some((owner, name)) => (Some(owner.trim_matches('`')), name),
        None => (None, rest),
    };
    let asked = name.trim_matches('`');
    let found: BTreeSet<String> = fixture
        .head
        .definitions()
        .iter()
        .filter(|d| d.key.local.name.as_str() == asked)
        .filter(|d| only_in.is_none_or(|path| d.file.as_str() == path))
        .filter(|d| {
            owner.is_none_or(|owner| {
                d.key
                    .local
                    .container
                    .last()
                    .is_some_and(|c| c.as_str() == owner)
            })
        })
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

/// The Kotlin cases the resolver cannot answer yet, each with the reason. The
/// list is held both ways: a listed case that turns green fails the run, as does
/// a red case that is not listed.
const RED: &[(&str, &str)] = &[];

#[test]
fn the_kotlin_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("kotlin");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/kotlin/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 14, "the walk found {} cases", cases.len());
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
        "kotlin corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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
#[test]
fn imports_bind_by_name_alias_and_wildcard() {
    let fx = snapshot(&[
        (
            "a/Util.kt",
            "package a\n\nfun slug(s: String): String = s\nfun helper(): Int = 1\nclass Box\n",
        ),
        (
            "b/App.kt",
            "package b\n\nimport a.slug as makeSlug\nimport a.Box\nimport a.*\n\nfun run(): Int {\n    makeSlug(\"x\")\n    val box = Box()\n    return helper()\n}\n",
        ),
    ]);
    assert_eq!(fx.lands_at("b/App.kt", "makeSlug", 8), "a/Util.kt::slug");
    assert_eq!(fx.lands_at("b/App.kt", "Box", 9), "a/Util.kt::Box");
    assert_eq!(fx.at("b/App.kt", "Box", 9).relation, Relation::Instantiates);
    assert_eq!(fx.lands_at("b/App.kt", "helper", 10), "a/Util.kt::helper");
    let imports: Vec<String> = fx
        .imports_of("b/App.kt")
        .iter()
        .map(|b| fx.lands(b))
        .collect();
    assert_eq!(imports, ["a/Util.kt::slug", "a/Util.kt::Box"]);
}

/// An object is its own instance; a class's companion answers for the class,
/// from inside it by the bare name and from outside by the class's name.
#[test]
fn object_and_companion_members_answer_for_their_holder() {
    let fx = snapshot(&[(
        "shop/Pricing.kt",
        "package shop\n\nobject Pricing {\n    const val RATE = 3\n    fun total(n: Int): Int = n * RATE\n}\n\nclass Cart {\n    fun charge(n: Int): Int = Pricing.total(n)\n    companion object {\n        const val LIMIT = 10\n        fun make() = Cart()\n    }\n    fun limit(): Int = LIMIT\n}\n\nfun build(): Cart = Cart.make()\n",
    )]);
    let f = "shop/Pricing.kt";
    assert_eq!(fx.lands_at(f, "RATE", 5), "shop/Pricing.kt::Pricing.RATE");
    assert_eq!(fx.at(f, "RATE", 5).relation, Relation::Reads);
    assert_eq!(fx.lands_at(f, "total", 9), "shop/Pricing.kt::Pricing.total");
    assert_eq!(
        fx.lands_at(f, "LIMIT", 14),
        "shop/Pricing.kt::Cart.Companion.LIMIT"
    );
    assert_eq!(
        fx.lands_at(f, "make", 17),
        "shop/Pricing.kt::Cart.Companion.make"
    );
    assert_eq!(fx.lands_at(f, "Cart", 12), "shop/Pricing.kt::Cart");
}

/// An extension is a member from outside: found through a receiver the
/// repository does not declare, through a subtype of its receiver, and as a
/// property; what the repository's type itself has wins over it.
#[test]
fn extensions_add_members_to_their_receiver_type() {
    let fx = snapshot(&[(
        "x/Ext.kt",
        "package x\n\nclass Box(val size: Int)\nclass Crate : Box(1)\n\nfun String.shout(): String = this\nfun Box.double(): Int = size * 2\nval Box.label: String get() = \"b\"\n\nfun use(b: Box, c: Crate, s: String): Int {\n    s.shout()\n    c.double()\n    return b.label.length\n}\n",
    )]);
    let f = "x/Ext.kt";
    assert_eq!(fx.lands_at(f, "shout", 11), "x/Ext.kt::String.shout");
    assert_eq!(fx.lands_at(f, "double", 12), "x/Ext.kt::Box.double");
    assert_eq!(fx.lands_at(f, "label", 13), "x/Ext.kt::Box.label");
    assert_eq!(
        fx.lands_at(f, "size", 7),
        "x/Ext.kt::Box.size",
        "the body of an extension sees the members of its receiver"
    );
    assert_eq!(fx.lands_at(f, "length", 13), "unresolved(External)");
}

/// A receiver is typed by what is written (a parameter, a property), by the
/// call that initialises it, and by what a function's expression body returns
/// (#562); one nothing types stays unknown.
#[test]
fn receivers_are_typed_by_declarations_and_initialisers() {
    let fx = snapshot(&[(
        "t/Typing.kt",
        "package t\n\nclass Repo { fun find(): String = \"\" }\nclass Service(private val repo: Repo) {\n    private val cache = Repo()\n    fun client() = Repo()\n    fun a(): String = repo.find()\n    fun b(): String = cache.find()\n    fun c(): String = client().find()\n    fun d(r: Repo): String {\n        val local = Repo()\n        val typed: Repo = r\n        val one = local.find()\n        return one + typed.find()\n    }\n}\n",
    )]);
    let f = "t/Typing.kt";
    for line in [7, 8, 9, 13, 14] {
        assert_eq!(
            fx.lands_at(f, "find", line),
            "t/Typing.kt::Repo.find",
            "line {line}"
        );
    }
}

/// A member is found on the class or on a type it inherits from, by the bare
/// name or through `this`; an `inner` class also sees the instance around it,
/// a nested one does not.
#[test]
fn inherited_members_and_the_instances_a_bare_name_reaches() {
    let fx = snapshot(&[(
        "h/Inherit.kt",
        "package h\n\nopen class Base {\n    fun shared(): Int = 1\n    open fun hook(): Int = 2\n}\n\nclass Child : Base() {\n    fun run(): Int = shared()\n    fun viaThis(): Int = this.shared()\n    inner class Deep {\n        fun go(): Int = shared()\n    }\n    class Nested {\n        fun go(): Int = shared()\n    }\n}\n\nfun top(): Int = shared()\n",
    )]);
    let f = "h/Inherit.kt";
    assert_eq!(fx.lands_at(f, "shared", 9), "h/Inherit.kt::Base.shared");
    assert_eq!(fx.lands_at(f, "shared", 10), "h/Inherit.kt::Base.shared");
    assert_eq!(fx.lands_at(f, "shared", 12), "h/Inherit.kt::Base.shared");
    assert_eq!(fx.lands_at(f, "shared", 15), "unresolved(NoBinding)");
    assert_eq!(fx.lands_at(f, "shared", 19), "unresolved(NoBinding)");
}

/// An `override` is bound to the member it replaces in the repository's
/// supertypes, and to nothing when the supertype is outside it.
#[test]
fn an_override_is_bound_to_the_member_it_replaces() {
    let fx = snapshot(&[(
        "o/Over.kt",
        "package o\n\ninterface Shape { fun area(): Int }\nopen class Square : Shape {\n    override fun area(): Int = 4\n}\nclass Cube : Square() {\n    override fun area(): Int = 8\n    override fun toString(): String = \"\"\n}\n",
    )]);
    let overrides: Vec<String> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.relation == Relation::Overrides)
        .map(|b| fx.lands(b))
        .collect();
    assert_eq!(
        overrides,
        [
            "o/Over.kt::Shape.area",
            "o/Over.kt::Square.area",
            "unresolved(External)"
        ]
    );
}

/// A call through a supertype that something overrides may run in the
/// override: a candidate on the declaration and every override. Through the
/// class itself it is the class's own.
#[test]
fn a_call_through_a_supertype_is_a_candidate_on_its_overrides() {
    let fx = snapshot(&[(
        "d/Disp.kt",
        "package d\n\ninterface Repo { fun find(): Int }\nclass Sql : Repo { override fun find(): Int = 1 }\nclass Mem : Repo { override fun find(): Int = 2 }\n\nfun viaInterface(r: Repo): Int = r.find()\nfun viaClass(s: Sql): Int = s.find()\n",
    )]);
    assert_eq!(
        fx.lands_at("d/Disp.kt", "find", 7),
        "candidate(Dispatch) d/Disp.kt::Mem.find, d/Disp.kt::Repo.find, d/Disp.kt::Sql.find"
    );
    assert_eq!(fx.lands_at("d/Disp.kt", "find", 8), "d/Disp.kt::Sql.find");
}

/// Overloads are a candidate each, and a receiver nothing types is a candidate
/// among the types the file can hold, for a call, and says nothing for a read.
#[test]
fn overloads_and_untyped_receivers_are_candidates() {
    let fx = snapshot(&[(
        "u/Untyped.kt",
        "package u\n\nclass Store {\n    fun put(a: Int): Int = a\n    fun put(a: Int, b: Int): Int = b\n}\nclass Other { fun put(): Int = 2 }\n\nfun typed(s: Store): Int = s.put(1)\nfun untyped(xs: List<Store>): Int = xs[0].put()\nfun read(xs: List<Store>): Int = xs[0].size\n",
    )]);
    let f = "u/Untyped.kt";
    assert_eq!(
        fx.lands_at(f, "put", 9),
        "candidate(Overloaded) u/Untyped.kt::Store.put, u/Untyped.kt::Store.put"
    );
    assert_eq!(
        fx.lands_at(f, "put", 10),
        "candidate(UntypedReceiver) u/Untyped.kt::Other.put, u/Untyped.kt::Store.put, u/Untyped.kt::Store.put"
    );
    assert_eq!(fx.lands_at(f, "size", 11), "unresolved(UntypedRead)");
}

/// `private` stops at the file; `internal` is open to the package of one project.
#[test]
fn private_stops_at_the_file() {
    let fx = snapshot(&[
        (
            "v/A.kt",
            "package v\n\nprivate fun hidden(): Int = 1\ninternal fun shown(): Int = 2\nclass K { private fun secret(): Int = 3 }\n",
        ),
        (
            "v/B.kt",
            "package v\n\nfun a(): Int = hidden()\nfun b(): Int = shown()\nfun c(k: K): Int = k.secret()\n",
        ),
    ]);
    assert_eq!(fx.lands_at("v/B.kt", "hidden", 3), "unresolved(NoMember)");
    assert_eq!(fx.lands_at("v/B.kt", "shown", 4), "v/A.kt::shown");
    assert_eq!(fx.lands_at("v/B.kt", "secret", 5), "unresolved(NoMember)");
}

/// A fully qualified name walks the package.
#[test]
fn a_qualified_name_walks_the_package() {
    let fx = snapshot(&[
        ("p/Q.kt", "package p.q\n\nfun ping(): Int = 1\n"),
        ("r/R.kt", "package r\n\nfun call(): Int = p.q.ping()\n"),
    ]);
    assert_eq!(fx.lands_at("r/R.kt", "ping", 3), "p/Q.kt::ping");
}

/// A project reaches another through a declared dependency alone, and
/// `internal` stops at the project even then.
#[test]
fn a_module_reaches_another_through_a_declared_dependency() {
    let fx = snapshot(&[
        (
            "settings.gradle.kts",
            "rootProject.name = \"shop\"\ninclude(\":lib\", \":app\", \":stray\")\n",
        ),
        ("lib/build.gradle.kts", "plugins { kotlin(\"jvm\") }\n"),
        (
            "lib/src/main/kotlin/l/Lib.kt",
            "package l\n\ninternal fun secret(): Int = 1\nfun ping(): Int = 2\n",
        ),
        (
            "app/build.gradle.kts",
            "dependencies {\n    implementation(project(\":lib\"))\n}\n",
        ),
        (
            "app/src/main/kotlin/a/App.kt",
            "package a\n\nimport l.secret\nimport l.ping\n\nfun f(): Int = secret()\nfun g(): Int = ping()\n",
        ),
        ("stray/build.gradle.kts", "plugins { kotlin(\"jvm\") }\n"),
        (
            "stray/src/main/kotlin/s/Stray.kt",
            "package s\n\nimport l.ping\n\nfun h(): Int = ping()\n",
        ),
    ]);
    let app = "app/src/main/kotlin/a/App.kt";
    assert_eq!(fx.lands_at(app, "secret", 6), "unresolved(OutsideProject)");
    assert_eq!(
        fx.lands_at(app, "ping", 7),
        "lib/src/main/kotlin/l/Lib.kt::ping"
    );
    assert_eq!(
        fx.lands_at("stray/src/main/kotlin/s/Stray.kt", "ping", 5),
        "unresolved(OutsideProject)"
    );
}

/// Java reads Kotlin under the names the compiler gives it: a top-level function
/// and property are members of `PricingKt` (`@JvmName` renames one), a property is
/// its accessors, an `object` is reached through `INSTANCE`, a companion through
/// `Companion`, and `@JvmStatic` makes a static member (of the class around a
/// companion too). Each call is a link to the Kotlin definition.
#[test]
fn java_calls_kotlin_under_the_names_the_compiler_gives_it() {
    let kotlin = "package shop\n\nfun total(items: List<Int>): Int = items.sum()\n\nval RATE: Int = 3\n\nvar mode: String = \"a\"\n\n@JvmName(\"rounded\")\nfun round(n: Int): Int = n\n\nobject Tax {\n    fun rate(): Int = 1\n    @JvmStatic fun flat(): Int = 2\n}\n\nclass Cart {\n    companion object {\n        @JvmStatic fun make(): Cart = Cart()\n        fun empty(): Cart = Cart()\n    }\n}\n";
    let java = "package shop;\n\npublic class Checkout {\n    public int charge(java.util.List<Integer> items) {\n        int a = PricingKt.total(items);\n        int b = PricingKt.getRATE();\n        PricingKt.setMode(\"b\");\n        String m = PricingKt.getMode();\n        int r = PricingKt.rounded(1);\n        int c = Tax.INSTANCE.rate();\n        int d = Tax.flat();\n        Cart e = Cart.Companion.empty();\n        Cart f = Cart.make();\n        return a;\n    }\n}\n";
    let fx = snapshot(&[
        ("src/main/kotlin/shop/Pricing.kt", kotlin),
        ("src/main/java/shop/Checkout.java", java),
    ]);
    let (call, kt) = (
        "src/main/java/shop/Checkout.java",
        "src/main/kotlin/shop/Pricing.kt",
    );
    let expected = [
        ("total", 5, "total"),
        ("getRATE", 6, "RATE"),
        ("setMode", 7, "mode"),
        ("getMode", 8, "mode"),
        ("rounded", 9, "round"),
        ("rate", 10, "Tax.rate"),
        ("flat", 11, "Tax.flat"),
        ("empty", 12, "Cart.Companion.empty"),
        ("make", 13, "Cart.Companion.make"),
    ];
    for (name, line, lands) in expected {
        assert_eq!(
            fx.lands_at(call, name, line),
            format!("{kt}::{lands}"),
            "{name} at line {line}"
        );
    }
}

/// `@file:JvmName("X")` renames the class Java reads a file's top-level
/// declarations from; a file without it keeps `FKt`.
#[test]
fn java_calls_a_file_under_the_class_its_jvmname_gives() {
    let fx = snapshot(&[
        (
            "src/main/kotlin/shop/Text.kt",
            "@file:JvmName(\"TextUtil\")\npackage shop\n\nfun shout(text: String): String = text\n",
        ),
        (
            "src/main/kotlin/shop/Other.kt",
            "package shop\n\nfun plain(): String = \"p\"\n",
        ),
        (
            "src/main/java/shop/Checkout.java",
            "package shop;\n\npublic class Checkout {\n    public String announce(String text) {\n        String a = OtherKt.plain();\n        return TextUtil.shout(a + text);\n    }\n}\n",
        ),
    ]);
    let call = "src/main/java/shop/Checkout.java";
    assert_eq!(
        fx.lands_at(call, "plain", 5),
        "src/main/kotlin/shop/Other.kt::plain"
    );
    assert_eq!(
        fx.lands_at(call, "shout", 6),
        "src/main/kotlin/shop/Text.kt::shout"
    );
}

/// A Kotlin definition used only by Java is no unused one: the resolver of the
/// Kotlin cell holds the Java file's call among the references to it.
#[test]
fn a_kotlin_definition_has_its_java_callers() {
    let fx = snapshot(&[
        (
            "src/main/kotlin/shop/Pricing.kt",
            "package shop\n\nfun total(items: List<Int>): Int = items.sum()\n",
        ),
        (
            "src/main/java/shop/Checkout.java",
            "package shop;\n\npublic class Checkout {\n    public int charge(java.util.List<Integer> items) {\n        return PricingKt.total(items);\n    }\n}\n",
        ),
    ]);
    let total = fx
        .head
        .definitions()
        .iter()
        .find(|d| d.key.local.name.as_str() == "total")
        .expect("the Kotlin definition");
    let callers: Vec<(String, Relation)> = fx
        .head
        .references_to(&total.key)
        .iter()
        .map(|b| (b.file.to_string(), b.relation))
        .collect();
    assert_eq!(
        callers,
        [(
            "src/main/java/shop/Checkout.java".to_string(),
            Relation::Calls
        )]
    );
    assert!(
        fx.head
            .read_paths()
            .iter()
            .any(|p| p.as_str() == "src/main/java/shop/Checkout.java"),
        "the Java file is read by the Kotlin resolver"
    );
}
