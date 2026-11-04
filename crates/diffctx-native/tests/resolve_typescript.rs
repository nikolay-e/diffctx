//! The TypeScript and JavaScript resolver on real git repositories: every
//! TypeScript case of the impact and symbol corpus read by the callers it
//! expects, and the shapes of the issues the resolver was written for. The
//! expectations come from the case files and from reading each fixture; nothing
//! here runs diffctx to learn an answer.
//!
//! The corpus judge pairs the resolver's relations to a case's target with the
//! case's own, by relation kind and resolution, and compares only what the
//! resolver answers: `tests_reach`, `references_path` and the like belong to the
//! layers above it. Readings fixed here and listed in the report:
//! - an `imports` or `reexports` binding is no caller: it is matched when a case
//!   asks for it and is never an extra;
//! - a type use of a definition is a read of it (gh#558 expects `reads`);
//! - a caller that is itself a changed definition may be listed or left out;
//! - in impact mode a target that no change touched is not judged: the cases
//!   assert what the answer reports about the changed definitions.

mod snapshot_fixture;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name, Scope, semantic_changes};
use _diffctx::resolve::{
    Binding, CandidateReason, EvidenceKind, Relation, Resolution, TypeScriptResolver,
};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: TypeScriptResolver,
    base: TypeScriptResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = TypeScriptResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = TypeScriptResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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
    resolver: &'f TypeScriptResolver,
    key: DefKey,
    file: String,
}

fn definitions_of<'f>(fixture: &'f Fixture, node: &Node) -> Vec<Named<'f>> {
    let named = |resolver: &'f TypeScriptResolver| -> Vec<Named<'f>> {
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
            // A type use reads the type it names.
            let relation = match binding.relation {
                Relation::TypeUses => Relation::Reads,
                other => other,
            };
            if !relation.uses() && !matches!(relation, Relation::Imports | Relation::Reexports) {
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
        if !taken[i] && exact && !group.in_diff && !group.by_glob && group.kind.uses() {
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

/// The definitions of the symbol the case asks for.
fn judge_definitions(fixture: &Fixture, case: &Case, failures: &mut Vec<String>) {
    let (Some(expected), Some(symbol)) = (&case.expect.definitions, &case.query.symbol) else {
        return;
    };
    let found: BTreeSet<String> = fixture
        .head
        .definitions()
        .iter()
        .filter(|d| d.key.local.name.as_str() == symbol)
        .map(|d| format!("{}::{}", d.file, d.key.local.name))
        .collect();
    let wanted: BTreeSet<String> = expected
        .iter()
        .map(|d| {
            let node = Node::parse(d);
            format!("{}::{}", node.path, node.symbol.unwrap_or_default())
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

/// The TypeScript and JavaScript cases the resolver cannot answer yet, each with
/// the reason. The list is held both ways: a listed case that turns green fails
/// the run, as does a red case that is not listed.
const RED: &[(&str, &str)] = &[(
    "impact/typescript/ts_classic_script_global_is_called_by_the_pages_later_scripts",
    "gh#602: a classic script's global is bound by the page that loads the scripts in order; \
         the facts carry nothing of HTML, and a bare name no file binds is `NoBinding`",
)];

#[test]
fn the_typescript_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("typescript");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/typescript/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 35, "the walk found {} cases", cases.len());
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
        "typescript corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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

/// Every import form binds the names it states, a type import included; a
/// namespace import offers the module's names; an interface-typed receiver is a
/// candidate on the declaration.
#[test]
fn es_imports_bind_in_every_form() {
    let fx = snapshot(&[
        (
            "src/lib.ts",
            "export default function main() { return 1; }\nexport function helper() { return 2; }\nexport const LIMIT = 3;\nexport interface Shape { area(): number }\n",
        ),
        (
            "src/app.ts",
            "import main, { helper as h, LIMIT } from './lib';\nimport * as lib from './lib';\nimport type { Shape } from './lib';\n\nexport function run(s: Shape): number {\n  return main() + h() + LIMIT + lib.helper() + s.area();\n}\n",
        ),
    ]);
    let default = fx.at("src/app.ts", "main", 6);
    assert_eq!(default.relation, Relation::Calls);
    assert_eq!(fx.lands(default), "src/lib.ts::main");
    assert_eq!(evidence(default).0, EvidenceKind::ImportedName);
    assert_eq!(fx.lands_at("src/app.ts", "h", 6), "src/lib.ts::helper");
    let limit = fx.at("src/app.ts", "LIMIT", 6);
    assert_eq!(limit.relation, Relation::Reads);
    assert_eq!(fx.lands(limit), "src/lib.ts::LIMIT");
    let through_namespace = fx.at("src/app.ts", "helper", 6);
    assert_eq!(fx.lands(through_namespace), "src/lib.ts::helper");
    assert_eq!(evidence(through_namespace).0, EvidenceKind::ModuleAttribute);
    let ty = fx.at("src/app.ts", "Shape", 5);
    assert_eq!(ty.relation, Relation::TypeUses);
    assert_eq!(fx.lands(ty), "src/lib.ts::Shape");
    assert_eq!(
        fx.lands_at("src/app.ts", "area", 6),
        "candidate(UntypedReceiver) src/lib.ts::Shape.area"
    );
    let imports: Vec<String> = fx
        .imports_of("src/app.ts")
        .iter()
        .map(|b| format!("{} {}", b.name, fx.lands(b)))
        .collect();
    assert_eq!(
        imports,
        [
            "main src/lib.ts::main",
            "helper src/lib.ts::helper",
            "LIMIT src/lib.ts::LIMIT",
            "lib module src/lib.ts",
            "Shape src/lib.ts::Shape",
        ]
    );
}

/// A barrel's `export *` and `export { a as b } from` are followed, and the
/// barrel is the route in the evidence; the barrel's own lines are re-exports.
#[test]
fn barrels_are_followed_and_named_in_the_evidence() {
    let fx = snapshot(&[
        ("src/core/a.ts", "export function alpha() { return 1; }\n"),
        ("src/core/b.ts", "export function beta() { return 2; }\n"),
        (
            "src/core/index.ts",
            "export * from './a';\nexport { beta as gamma } from './b';\n",
        ),
        (
            "src/app.ts",
            "import { alpha, gamma } from './core';\n\nexport function go() {\n  return alpha() + gamma();\n}\n",
        ),
    ]);
    let alpha = fx.at("src/app.ts", "alpha", 4);
    assert_eq!(fx.lands(alpha), "src/core/a.ts::alpha");
    assert_eq!(
        evidence(alpha),
        (
            EvidenceKind::ImportedName,
            vec!["src/core/index.ts".to_string()]
        )
    );
    let gamma = fx.at("src/app.ts", "gamma", 4);
    assert_eq!(fx.lands(gamma), "src/core/b.ts::beta");
    assert_eq!(
        evidence(gamma),
        (
            EvidenceKind::ImportedName,
            vec!["src/core/index.ts".to_string()]
        )
    );
    let offered: Vec<(Relation, String)> = fx
        .imports_of("src/core/index.ts")
        .iter()
        .map(|b| (b.relation, fx.lands(b)))
        .collect();
    assert_eq!(
        offered,
        [
            (Relation::Reexports, "module src/core/a.ts".to_string()),
            (Relation::Reexports, "src/core/b.ts::beta".to_string()),
        ]
    );
}

/// `paths`, `baseUrl` and an exact alias land through the project the manifest
/// roots; `.js` for a `.ts` source and an `index` file are the same module.
#[test]
fn tsconfig_paths_and_base_url_land_through_the_project() {
    let fx = snapshot(&[
        (
            "package.json",
            "{\"name\": \"app\", \"version\": \"1.0.0\"}\n",
        ),
        (
            "tsconfig.json",
            "{ \"compilerOptions\": { \"baseUrl\": \"src\", \"paths\": { \"@/*\": [\"*\"], \"~lib\": [\"util/fmt.ts\"] } } }\n",
        ),
        (
            "src/pages/Direct.tsx",
            "export function Direct() { return null; }\n",
        ),
        (
            "src/util/fmt.ts",
            "export function fmt(x: number) { return String(x); }\n",
        ),
        ("src/ui/index.ts", "export function badge() { return 1; }\n"),
        (
            "src/App.tsx",
            "import { Direct } from '@/pages/Direct';\nimport { fmt } from 'util/fmt';\nimport { fmt as same } from '~lib';\nimport { badge } from './ui/index.js';\nimport { gone } from './missing';\n\nexport function App() {\n  return <p>{fmt(1)}{same(2)}{badge()}{gone()}<Direct /></p>;\n}\n",
        ),
    ]);
    let direct = fx.at("src/App.tsx", "Direct", 8);
    assert_eq!(direct.relation, Relation::Instantiates);
    assert_eq!(fx.lands(direct), "src/pages/Direct.tsx::Direct");
    assert_eq!(fx.lands_at("src/App.tsx", "fmt", 8), "src/util/fmt.ts::fmt");
    assert_eq!(
        fx.lands_at("src/App.tsx", "same", 8),
        "src/util/fmt.ts::fmt"
    );
    assert_eq!(
        fx.lands_at("src/App.tsx", "badge", 8),
        "src/ui/index.ts::badge"
    );
    assert_eq!(
        fx.lands_at("src/App.tsx", "gone", 8),
        "unresolved(NoBinding)"
    );
}

/// A package is found by the name its manifest gives it, through `exports`, and
/// only by a project that declares the dependency.
#[test]
fn workspace_packages_land_by_name_through_a_declared_dependency() {
    let fx = snapshot(&[
        (
            "package.json",
            "{\"name\": \"mono\", \"private\": true, \"workspaces\": [\"packages/*\"]}\n",
        ),
        (
            "packages/core/package.json",
            "{\"name\": \"@acme/core\", \"version\": \"1.0.0\", \"exports\": {\".\": \"./src/index.ts\", \"./util\": \"./src/util.ts\"}}\n",
        ),
        (
            "packages/core/src/index.ts",
            "export { make } from './make';\n",
        ),
        (
            "packages/core/src/make.ts",
            "export function make() { return 1; }\n",
        ),
        (
            "packages/core/src/util.ts",
            "export function tidy() { return 2; }\n",
        ),
        (
            "packages/app/package.json",
            "{\"name\": \"@acme/app\", \"version\": \"1.0.0\", \"dependencies\": {\"@acme/core\": \"workspace:*\"}}\n",
        ),
        (
            "packages/app/src/main.ts",
            "import { make } from '@acme/core';\nimport { tidy } from '@acme/core/util';\n\nexport function run() {\n  return make() + tidy();\n}\n",
        ),
        (
            "packages/stray/package.json",
            "{\"name\": \"@acme/stray\", \"version\": \"1.0.0\"}\n",
        ),
        (
            "packages/stray/src/main.ts",
            "import { make } from '@acme/core';\n\nexport function run() {\n  return make();\n}\n",
        ),
    ]);
    let make = fx.at("packages/app/src/main.ts", "make", 5);
    assert_eq!(fx.lands(make), "packages/core/src/make.ts::make");
    assert_eq!(
        evidence(make),
        (
            EvidenceKind::ImportedName,
            vec!["packages/core/src/index.ts".to_string()]
        )
    );
    assert_eq!(
        fx.lands_at("packages/app/src/main.ts", "tidy", 5),
        "packages/core/src/util.ts::tidy"
    );
    assert_eq!(
        fx.lands_at("packages/stray/src/main.ts", "make", 4),
        "unresolved(OutsideProject)"
    );
}

/// A name nothing resolved never crosses a project root, and neither does a
/// relative import.
#[test]
fn nothing_crosses_a_project_root_without_a_declared_dependency() {
    let fx = snapshot(&[
        ("shop/package.json", "{\"name\": \"shop\"}\n"),
        (
            "shop/src/store.ts",
            "export class Store {\n  loadItem() { return 1; }\n}\n",
        ),
        ("legacy/package.json", "{\"name\": \"legacy\"}\n"),
        (
            "legacy/src/loose.ts",
            "import { Store } from '../../shop/src/store';\n\nexport function find(s: any) { return s.loadItem(); }\nexport const made = new Store();\n",
        ),
        (
            "shop/src/loose.ts",
            "export function find(s: any) { return s.loadItem(); }\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("legacy/src/loose.ts", "loadItem", 3),
        "unresolved(NoCandidates)"
    );
    assert_eq!(
        fx.lands_at("legacy/src/loose.ts", "Store", 4),
        "unresolved(OutsideProject)"
    );
    assert_eq!(
        fx.lands_at("shop/src/loose.ts", "loadItem", 1),
        "candidate(AnyReceiver) shop/src/store.ts::Store.loadItem"
    );
}

/// gh#611: a receiver is typed by `new`, by an annotation, by a promoted
/// parameter, by a function's declared return type, and by what a `let` is
/// assigned; `this` is the class.
#[test]
fn receivers_are_typed_by_new_annotations_fields_and_factories() {
    let fx = snapshot(&[
        (
            "src/items.ts",
            "export class ItemsService {\n  constructor(private base: string) {}\n  byIds(ids: string[]): string[] { return ids.map((i) => this.base + i); }\n  again() { return this.byIds([]); }\n}\nexport function make(): ItemsService { return new ItemsService('x'); }\nexport interface Clock { now(): number }\n",
        ),
        (
            "src/use.ts",
            "import { ItemsService, make } from './items';\nimport type { Clock } from './items';\n\nconst shared = new ItemsService('/x/');\nclass Page {\n  constructor(private readonly svc: ItemsService, private clock: Clock) {}\n  run() { return this.svc.byIds([]); }\n  tick() { return this.clock.now(); }\n}\nexport function viaFactory() { const s = make(); return s.byIds([]); }\nexport function viaNew() { let t = new ItemsService('/y/'); return t.byIds([]); }\nexport function viaModule() { return shared.byIds([]); }\nexport async function awaited() { const u = await make(); return u.byIds([]); }\n",
        ),
    ]);
    let own = fx.at("src/items.ts", "byIds", 4);
    assert_eq!(fx.lands(own), "src/items.ts::ItemsService.byIds");
    assert_eq!(evidence(own).0, EvidenceKind::SelfReceiver);
    let field = fx.at("src/use.ts", "byIds", 7);
    assert_eq!(fx.lands(field), "src/items.ts::ItemsService.byIds");
    assert_eq!(evidence(field).0, EvidenceKind::TypedReceiver);
    assert_eq!(
        fx.lands_at("src/use.ts", "now", 8),
        "candidate(UntypedReceiver) src/items.ts::Clock.now"
    );
    let factory = fx.at("src/use.ts", "byIds", 10);
    assert_eq!(fx.lands(factory), "src/items.ts::ItemsService.byIds");
    assert_eq!(evidence(factory).0, EvidenceKind::FactoryReturn);
    let assigned = fx.at("src/use.ts", "byIds", 11);
    assert_eq!(fx.lands(assigned), "src/items.ts::ItemsService.byIds");
    assert_eq!(evidence(assigned).0, EvidenceKind::Constructed);
    assert_eq!(
        fx.lands_at("src/use.ts", "byIds", 12),
        "src/items.ts::ItemsService.byIds"
    );
    assert_eq!(
        fx.lands_at("src/use.ts", "byIds", 13),
        "src/items.ts::ItemsService.byIds"
    );
}

/// gh#592: a class property holding an arrow is a method, found through `this`
/// and through an instance a test builds in a callback.
#[test]
fn a_class_property_arrow_is_a_method_of_its_class() {
    let fx = snapshot(&[
        (
            "src/manager.ts",
            "export class Manager {\n  private level = 0;\n  private readonly passes = (): boolean => this.level < 4;\n  submit = (correct: boolean): number => {\n    return correct && this.passes() ? this.level : 0;\n  };\n}\n",
        ),
        (
            "tests/manager.test.ts",
            "import { Manager } from '../src/manager';\n\nit('submits', () => {\n  const m = new Manager();\n  expect(m.submit(true)).toBe(1);\n});\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("src/manager.ts", "passes", 5),
        "src/manager.ts::Manager.passes"
    );
    let called = fx.at("tests/manager.test.ts", "submit", 5);
    assert_eq!(called.relation, Relation::Calls);
    assert_eq!(fx.lands(called), "src/manager.ts::Manager.submit");
    assert!(
        matches!(called.from, Scope::Callback { .. }),
        "{:?}",
        called.from
    );
}

/// gh#480: a call inside a callback argument belongs to the function that wrote
/// it; gh#706: a function an object literal hands to a call is passed as a
/// value, and a parameter passed is a local.
#[test]
fn callbacks_and_object_literal_properties_belong_to_their_function() {
    let fx = snapshot(&[
        (
            "web/lib.ts",
            "export function useQuery(opts: { queryFn: () => unknown }) { return opts; }\n",
        ),
        ("web/api.ts", "export function loadThing() { return 1; }\n"),
        (
            "web/query.ts",
            "import { useQuery } from './lib';\nimport { loadThing } from './api';\n\nexport function useThing(onDone: () => void) {\n  return useQuery({\n    queryFn: loadThing,\n    onSuccess: () => loadThing(),\n    done: onDone,\n  });\n}\n",
        ),
    ]);
    let passed = fx.at("web/query.ts", "loadThing", 6);
    assert_eq!(passed.relation, Relation::PassesAsValue);
    assert_eq!(fx.lands(passed), "web/api.ts::loadThing");
    let called = fx.at("web/query.ts", "loadThing", 7);
    assert_eq!(called.relation, Relation::Calls);
    assert_eq!(fx.lands(called), "web/api.ts::loadThing");
    assert!(matches!(called.from, Scope::Callback { .. }));
    assert_eq!(
        fx.lands_at("web/query.ts", "onDone", 8),
        "unresolved(Local)"
    );
}

/// gh#698: a receiver typed by an interface may be any implementation, so the
/// call is a candidate on the declaration and every member that replaces it; a
/// method of an object literal a function returns is a definition of that
/// function, and the object's receiver, which no type names, may be it.
#[test]
fn interface_dispatch_is_a_candidate_on_every_implementation() {
    let fx = snapshot(&[
        (
            "src/engine.ts",
            "export interface Engine { setQueue(items: string[]): number; }\nexport class NativeEngine implements Engine {\n  setQueue(items: string[]): number { return items.length; }\n}\nexport class WebEngine implements Engine {\n  setQueue(items: string[]): number { return 0; }\n}\n",
        ),
        (
            "src/factory.ts",
            "export function makeSys() {\n  return {\n    open(): string { return 'opened'; },\n  };\n}\n",
        ),
        (
            "src/use.ts",
            "import type { Engine } from './engine';\nimport { makeSys } from './factory';\n\nexport function play(engine: Engine): number { return engine.setQueue(['a']); }\nexport function start(): string {\n  const sys = makeSys();\n  return sys.open();\n}\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("src/use.ts", "setQueue", 4),
        "candidate(UntypedReceiver) src/engine.ts::Engine.setQueue, \
         src/engine.ts::NativeEngine.setQueue, src/engine.ts::WebEngine.setQueue"
    );
    assert_eq!(
        fx.lands_at("src/use.ts", "open", 7),
        "candidate(UntypedReceiver) src/factory.ts::makeSys.open"
    );
}

/// A name is found on the class itself, then on what it extends; `super` starts
/// at the base; an abstract member is run by a subclass; a method that replaces
/// a base's is an override.
#[test]
fn self_super_abstract_members_and_overrides_follow_the_bases() {
    let fx = snapshot(&[(
        "src/shapes.ts",
        "export abstract class Base {\n  run(): number { return this.helper(); }\n  helper(): number { return 1; }\n  abstract area(): number;\n}\nexport class Child extends Base {\n  run(): number { return super.run() + this.helper() + this.area(); }\n  area(): number { return 2; }\n}\nexport function use(b: Base): number { return b.area(); }\nexport function useChild(c: Child): number { return c.helper(); }\n",
    )]);
    let own = fx.at("src/shapes.ts", "helper", 2);
    assert_eq!(fx.lands(own), "src/shapes.ts::Base.helper");
    assert_eq!(evidence(own).0, EvidenceKind::SelfReceiver);
    let inherited = fx.at("src/shapes.ts", "helper", 7);
    assert_eq!(fx.lands(inherited), "src/shapes.ts::Base.helper");
    assert_eq!(evidence(inherited).0, EvidenceKind::Inherited);
    let up = fx.at("src/shapes.ts", "run", 7);
    assert_eq!(fx.lands(up), "src/shapes.ts::Base.run");
    assert_eq!(evidence(up).0, EvidenceKind::SuperCall);
    assert_eq!(
        fx.lands_at("src/shapes.ts", "area", 7),
        "src/shapes.ts::Child.area"
    );
    assert_eq!(
        fx.lands_at("src/shapes.ts", "area", 10),
        "candidate(UntypedReceiver) src/shapes.ts::Base.area, src/shapes.ts::Child.area"
    );
    assert_eq!(
        fx.lands_at("src/shapes.ts", "helper", 11),
        "src/shapes.ts::Base.helper"
    );
    let mut overrides: Vec<String> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.relation == Relation::Overrides)
        .map(|b| fx.lands(b))
        .collect();
    overrides.sort();
    assert_eq!(
        overrides,
        ["src/shapes.ts::Base.area", "src/shapes.ts::Base.run"]
    );
}

/// An untyped call is a candidate among the files the caller imports, any file
/// the caller's project reaches when the receiver is annotated `any`, and
/// nothing between files that do not know each other.
#[test]
fn untyped_calls_are_candidates_only_among_linked_files() {
    let fx = snapshot(&[
        (
            "a/x.ts",
            "export class Store {\n  load() { return 1; }\n}\n",
        ),
        (
            "a/user.ts",
            "import { Store } from './x';\n\nexport const keep = Store;\nexport function f(s) { return s.load(); }\n",
        ),
        ("a/other.ts", "export function g(s) { return s.load(); }\n"),
        (
            "a/any.ts",
            "export function h(s: any) { return s.load(); }\n",
        ),
        (
            "a/builtin.ts",
            "export function k(items: string[], m: Map<string, number>) { return items.map((i) => m.get(i)); }\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("a/user.ts", "load", 4),
        "candidate(UntypedReceiver) a/x.ts::Store.load"
    );
    assert_eq!(
        fx.lands_at("a/other.ts", "load", 1),
        "unresolved(NoCandidates)"
    );
    assert_eq!(
        fx.lands_at("a/any.ts", "load", 1),
        "candidate(AnyReceiver) a/x.ts::Store.load"
    );
    assert_eq!(fx.lands_at("a/builtin.ts", "get", 1), "unresolved(Builtin)");
}

/// fj#106: `lazy(() => import('./Card'))` imports the module's default export,
/// and the element stays the constant the file declares; gh#479: the member a
/// `.then` callback takes from the module is the import's.
#[test]
fn dynamic_imports_land_on_the_default_export_and_the_members_a_callback_takes() {
    let fx = snapshot(&[
        (
            "Card.tsx",
            "export default function Card() { return <div>Old</div>; }\n",
        ),
        (
            "App.tsx",
            "import { lazy, Suspense } from 'react';\nconst Card = lazy(() => import('./Card'));\nexport function App() { return <Suspense fallback={null}><Card /></Suspense>; }\n",
        ),
        (
            "Player.tsx",
            "export function Player() { return <b />; }\nexport function Other() { return <i />; }\n",
        ),
        (
            "Bar.tsx",
            "import { lazy } from 'react';\n\nconst Player = lazy(async () =>\n  import('./Player').then((m) => ({ default: m.Player }))\n);\n\nexport function Bar() {\n  return <Player />;\n}\n",
        ),
        (
            "load.ts",
            "export async function load() {\n  const m = await import('./Player');\n  return m.Other();\n}\n",
        ),
    ]);
    let lazy: Vec<&Binding> = fx
        .imports_of("App.tsx")
        .into_iter()
        .filter(|b| b.site.lines.0 == 2)
        .collect();
    assert_eq!(lazy.len(), 1, "{lazy:#?}");
    assert_eq!(fx.lands(lazy[0]), "Card.tsx::Card");
    assert_eq!(evidence(lazy[0]).0, EvidenceKind::DynamicImport);
    let element = fx.at("App.tsx", "Card", 3);
    assert_eq!(element.relation, Relation::Instantiates);
    assert_eq!(fx.lands(element), "App.tsx::Card");
    let then: Vec<String> = fx
        .imports_of("Bar.tsx")
        .into_iter()
        .filter(|b| b.site.lines.0 == 4)
        .map(|b| format!("{} {}", b.name, fx.lands(b)))
        .collect();
    assert_eq!(
        then,
        ["./Player module Player.tsx", "Player Player.tsx::Player"]
    );
    let awaited = fx.at("load.ts", "Other", 3);
    assert_eq!(fx.lands(awaited), "Player.tsx::Other");
    assert_eq!(evidence(awaited).0, EvidenceKind::ModuleAttribute);
}

/// CommonJS: `require` binds a name, a destructured member or the whole module,
/// and `module.exports` and `exports.x` offer names.
#[test]
fn commonjs_require_and_exports_bind_like_modules() {
    let fx = snapshot(&[
        (
            "lib/util.js",
            "function pad(s) { return ' ' + s; }\nfunction trim(s) { return s.trim(); }\nmodule.exports = { pad, trim: trim };\n",
        ),
        (
            "lib/main.js",
            "const { pad } = require('./util');\nconst util = require('./util');\nfunction run(x) { return pad(x) + util.trim(x); }\nexports.run = run;\n",
        ),
        (
            "app.js",
            "const { run } = require('./lib/main');\nrun(1);\n",
        ),
    ]);
    assert_eq!(fx.lands_at("lib/main.js", "pad", 3), "lib/util.js::pad");
    let trimmed = fx.at("lib/main.js", "trim", 3);
    assert_eq!(fx.lands(trimmed), "lib/util.js::trim");
    assert_eq!(evidence(trimmed).0, EvidenceKind::ModuleAttribute);
    assert_eq!(fx.lands_at("app.js", "run", 2), "lib/main.js::run");
}

/// JSX: a component rendered through a namespace import is instantiated, and an
/// intrinsic tag is nothing.
#[test]
fn jsx_components_are_instantiated_and_intrinsic_tags_are_nothing() {
    let fx = snapshot(&[
        ("src/ui.tsx", "export function Card() { return null; }\n"),
        (
            "src/page.tsx",
            "import * as ui from './ui';\n\nexport function Page(n: number) {\n  return n > 1 ? <ui.Card /> : <div />;\n}\n",
        ),
    ]);
    let card = fx.at("src/page.tsx", "Card", 4);
    assert_eq!(card.relation, Relation::Instantiates);
    assert_eq!(fx.lands(card), "src/ui.tsx::Card");
    assert_eq!(evidence(card).0, EvidenceKind::ModuleAttribute);
}

/// Enum members and namespace members are found through their container.
#[test]
fn enum_and_namespace_members_are_found_through_their_container() {
    let fx = snapshot(&[
        (
            "src/colors.ts",
            "export enum Color { Red, Green }\nexport namespace Util { export function twice(x: number) { return x * 2; } }\n",
        ),
        (
            "src/use.ts",
            "import { Color, Util } from './colors';\nexport const pick = Color.Red;\nexport const two = Util.twice(1);\n",
        ),
    ]);
    let member = fx.at("src/use.ts", "Red", 2);
    assert_eq!(member.relation, Relation::Reads);
    assert_eq!(fx.lands(member), "src/colors.ts::Color.Red");
    let owner = fx.at("src/use.ts", "Color", 2);
    assert_eq!(owner.relation, Relation::Reads);
    assert_eq!(fx.lands(owner), "src/colors.ts::Color");
    assert_eq!(
        fx.lands_at("src/use.ts", "twice", 3),
        "src/colors.ts::Util.twice"
    );
}

/// gh#602: the global of a classic script is bound by the page that loads it,
/// which the facts do not state; a classic script that names a global no file
/// of its own defines may mean any script's, a candidate and never a guess
/// stated as resolved.
#[test]
fn a_classic_script_global_is_a_candidate_not_a_resolution() {
    let fx = snapshot(&[
        (
            "lib.js",
            "function greet(name) {\n  return \"hi \" + name;\n}\n",
        ),
        (
            "app.js",
            "function boot() {\n  document.title = greet(\"app\");\n}\nboot();\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("app.js", "greet", 2),
        "candidate(Global) lib.js::greet"
    );
    assert_eq!(fx.lands_at("app.js", "boot", 4), "app.js::boot");
    assert_eq!(fx.lands_at("app.js", "title", 2), "unresolved(Builtin)");
}

/// #426: `self.x = ...` defines a global; a worker reads the scripts it loads
/// with `importScripts` and no others, and a script that loads none may mean any.
#[test]
fn a_worker_reads_the_globals_of_the_scripts_it_loads() {
    let fx = snapshot(&[
        ("kit/sw-kit.js", "self.serveShell = () => 1;\n"),
        ("other/sw-kit.js", "self.serveShell = () => 2;\n"),
        (
            "apps/a/sw.js",
            "importScripts('/kit/sw-kit.js');\nself.serveShell();\nserveShell();\nglobalThis.serveShell();\n",
        ),
        (
            "apps/b/sw.js",
            "importScripts('../../other/sw-kit.js');\nserveShell();\n",
        ),
        ("apps/c/sw.js", "self.serveShell();\n"),
    ]);
    for line in [2, 3, 4] {
        assert_eq!(
            fx.lands_at("apps/a/sw.js", "serveShell", line),
            "kit/sw-kit.js::serveShell"
        );
    }
    assert_eq!(
        fx.lands_at("apps/b/sw.js", "serveShell", 2),
        "other/sw-kit.js::serveShell"
    );
    assert_eq!(
        fx.lands_at("apps/c/sw.js", "serveShell", 1),
        "candidate(Global) kit/sw-kit.js::serveShell, other/sw-kit.js::serveShell"
    );
}

/// #357: the members asked of what a glob or a context loads are the
/// definitions of that name in the modules the pattern matches.
#[test]
fn a_glob_reaches_the_modules_its_pattern_matches() {
    let mount = "export function mountA(el) {\n  return el;\n}\n";
    let fx = snapshot(&[
        ("src/lib/live/a.ts", mount),
        ("src/lib/live/engine/core.ts", mount),
        ("src/lib/other/b.ts", mount),
        (
            "src/pages/Cards.ts",
            "const modules = import.meta.glob(['../lib/live/**/*.ts', '!../lib/live/engine/**']);\nexport async function mountAll(el: Element) {\n  for (const load of Object.values(modules)) {\n    const mod: any = await load();\n    mod.mountA(el);\n  }\n}\n",
        ),
        (
            "src/pages/Ctx.js",
            "const ctx = require.context('../lib/live', false, /\\.ts$/);\nexport function viaContext(el) {\n  return ctx.keys().map((k) => ctx(k).mountA(el));\n}\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("src/pages/Cards.ts", "mountA", 5),
        "candidate(ImportGlob) src/lib/live/a.ts::mountA"
    );
    assert_eq!(
        fx.lands_at("src/pages/Ctx.js", "mountA", 3),
        "candidate(ImportGlob) src/lib/live/a.ts::mountA"
    );
    let loaded: Vec<String> = fx
        .imports_of("src/pages/Cards.ts")
        .iter()
        .map(|b| fx.lands(b))
        .collect();
    assert_eq!(loaded, ["candidate(ImportGlob) src/lib/live/a.ts::mountA"]);
}

/// #372: a Vite alias is read as text and lands like a `paths` entry; one it
/// cannot read leaves the app's bare imports possible, never external.
#[test]
fn a_vite_alias_lands_and_an_unreadable_one_leaves_imports_possible() {
    let tel = "export function install(app: string) {\n  return app;\n}\n";
    let start = |n: &str, spec: &str| {
        format!(
            "import {{ install }} from '{spec}';\nexport function start{n}() {{\n  return install('{n}');\n}}\n"
        )
    };
    let fx = snapshot(&[
        ("shared/pkg/tel.ts", tel),
        ("apps/a/package.json", "{\"name\": \"a\"}\n"),
        (
            "apps/a/vite.config.ts",
            "import path from 'node:path';\nexport default defineConfig({\n  resolve: {\n    alias: {\n      '@pkg': path.resolve(__dirname, '../../shared/pkg'),\n    },\n  },\n});\n",
        ),
        ("apps/a/src/main.ts", &start("A", "@pkg/tel")),
        ("apps/a/src/npm.ts", &start("N", "telemetry-lib")),
        (
            "apps/d/vite.config.ts",
            "export default {\n  resolve: { alias: [{ find: /^@pkg\\//, replacement: '/shared/pkg/' }] },\n};\n",
        ),
        ("apps/d/src/main.ts", &start("D", "@pkg/tel")),
    ]);
    assert_eq!(
        fx.lands_at("apps/a/src/main.ts", "install", 3),
        "shared/pkg/tel.ts::install"
    );
    assert_eq!(
        fx.lands_at("apps/a/src/npm.ts", "install", 3),
        "unresolved(External)"
    );
    assert_eq!(
        fx.lands_at("apps/d/src/main.ts", "install", 3),
        "candidate(ImportUnresolved) shared/pkg/tel.ts::install"
    );
}

/// An alias no configuration places, and a default import of a module nothing
/// places, are possible: a named one by name, a default one for the default
/// exports; a bundler query names the file it queries.
#[test]
fn unplaced_imports_are_candidates_by_name_and_defaults_by_default_exports() {
    let fx = snapshot(&[
        (
            "web/tsconfig.json",
            "{ \"extends\": \"@tsconfig/strictest/tsconfig.json\" }\n",
        ),
        (
            "web/src/api.ts",
            "export type Event = { id: string };\nexport default function Banner() {\n  return 1;\n}\nexport function plain() {\n  return 2;\n}\n",
        ),
        (
            "web/src/page.ts",
            "import type { Event } from '@pkg/types';\nimport Banner from '@pkg/banner';\nimport raw from '../public/sw.js?raw';\nexport function show(e: Event) {\n  return [e.id, Banner(), raw];\n}\n",
        ),
        ("web/public/sw.js", "self.parse = (r) => r;\n"),
    ]);
    assert_eq!(
        fx.lands_at("web/src/page.ts", "Event", 4),
        "candidate(ImportUnresolved) web/src/api.ts::Event"
    );
    assert_eq!(
        fx.lands_at("web/src/page.ts", "Banner", 5),
        "candidate(ImportUnresolved) web/src/api.ts::Banner"
    );
    assert!(
        fx.imports_of("web/src/page.ts")
            .iter()
            .any(|b| b.name.as_str() == "raw"
                && matches!(
                    b.resolution,
                    Resolution::Resolved { .. } | Resolution::Module { .. }
                )),
        "{:#?}",
        fx.imports_of("web/src/page.ts")
    );
}

/// An import of a symlink holds the module it names.
#[cfg(unix)]
#[test]
fn an_import_of_a_symlink_reaches_the_module_it_names() {
    let repo = Repo::init();
    repo.write(
        "shared/tel.js",
        "export function install() {\n  return 1;\n}\n",
    );
    repo.write(
        "apps/d/main.js",
        "import { install } from '../a/public/tel.js';\nexport function boot() {\n  return install();\n}\n",
    );
    std::fs::create_dir_all(repo.path().join("apps/a/public")).expect("mkdir");
    std::os::unix::fs::symlink(
        "../../../shared/tel.js",
        repo.path().join("apps/a/public/tel.js"),
    )
    .expect("symlink");
    let head = repo.commit("links");
    let fx = open(repo, RangeSpec::At(Side::Tree(head)));
    assert_eq!(
        fx.lands_at("apps/d/main.js", "install", 3),
        "shared/tel.js::install"
    );
}

/// A default export wrapped in a call hands on the one name the call is given:
/// `export default memo(Badge)` is `Badge`.
#[test]
fn a_wrapped_default_export_is_the_component_it_wraps() {
    let fx = snapshot(&[
        (
            "src/Card.tsx",
            "import { memo } from 'react';\nfunction Badge() { return <b />; }\nexport default memo(Badge);\n",
        ),
        (
            "src/Page.tsx",
            "import Badge from './Card';\nexport function Page() {\n  return <Badge />;\n}\n",
        ),
    ]);
    let badge = fx.at("src/Page.tsx", "Badge", 3);
    assert_eq!(badge.relation, Relation::Instantiates);
    assert_eq!(fx.lands(badge), "src/Card.tsx::Badge");
}

/// A member a chain passes through is read; a data property of an interface
/// is read as declared, and a method is dispatched.
#[test]
fn chains_read_their_members_and_object_types_have_members() {
    let fx = snapshot(&[
        (
            "src/prefs.ts",
            "export interface User {\n  soft: string[];\n}\nexport type Other = {\n  soft: string[];\n};\n",
        ),
        (
            "src/read.ts",
            "import { User, Other } from './prefs';\nexport function a(p: User) {\n  return p.soft;\n}\nexport function b(o: Other) {\n  return o.soft.length;\n}\n",
        ),
    ]);
    let direct = fx.at("src/read.ts", "soft", 3);
    assert_eq!(direct.relation, Relation::Reads);
    assert_eq!(fx.lands(direct), "src/prefs.ts::User.soft");
    assert_eq!(
        fx.lands_at("src/read.ts", "soft", 6),
        "src/prefs.ts::Other.soft"
    );
}

/// gh#554: a function declared inside another is a definition, found by the
/// functions beside it.
#[test]
fn functions_declared_inside_a_function_call_each_other() {
    let fx = snapshot(&[(
        "src/store.ts",
        "export const useStore = create()((set) => {\n  function alpha() { return 1; }\n  function beta() { return alpha(); }\n  return { play: async () => { await beta(); } };\n});\nfunction alpha() { return 2; }\nexport function top() { return alpha(); }\n",
    )]);
    let inner = fx.at("src/store.ts", "alpha", 3);
    assert_eq!(fx.lands(inner), "src/store.ts::useStore.alpha");
    assert_eq!(evidence(inner).0, EvidenceKind::SameScope);
    assert!(matches!(&inner.from, Scope::Def(key) if key.name.as_str() == "beta"));
    assert_eq!(
        fx.lands_at("src/store.ts", "beta", 4),
        "src/store.ts::useStore.beta"
    );
    assert_eq!(
        fx.lands_at("src/store.ts", "alpha", 7),
        "src/store.ts::alpha"
    );
}

/// A tsconfig with no manifest above it still holds its `paths` (gh#372).
#[test]
fn a_tsconfig_without_a_manifest_holds_its_paths() {
    let fx = snapshot(&[
        (
            "app/tsconfig.json",
            "{ \"compilerOptions\": { \"baseUrl\": \".\", \"paths\": { \"@pkg/*\": [\"../shared/pkg/*\"] } } }\n",
        ),
        (
            "app/src/main.ts",
            "import { f } from '@pkg/lib';\n\nexport function boot() {\n  return f();\n}\n",
        ),
        ("shared/pkg/lib.js", "export function f() { return 1; }\n"),
    ]);
    assert_eq!(
        fx.lands_at("app/src/main.ts", "f", 4),
        "shared/pkg/lib.js::f"
    );
}

/// A method of the object literal a constant holds is called through the
/// constant; a store's action is called through whatever the store call
/// returns, and the interface that announces it is not a second callee.
#[test]
fn object_literal_methods_are_called_and_an_interface_declaration_is_not_a_second_callee() {
    let fx = snapshot(&[
        (
            "src/store.ts",
            "interface Actions {\n  save: (v: string) => void;\n}\nexport const cart = {\n  total() {\n    return 1;\n  },\n};\nexport const useStore = create<Actions>()((set) => ({\n  save: (v: string) => {\n    set({ v });\n  },\n}));\n",
        ),
        (
            "src/use.ts",
            "import { cart, useStore } from './store';\nexport function show() {\n  cart.total();\n  useStore.getState().save('x');\n}\n",
        ),
    ]);
    let total = fx.at("src/use.ts", "total", 3);
    assert_eq!(total.relation, Relation::Calls);
    assert_eq!(fx.lands(total), "src/store.ts::cart.total");
    assert_eq!(
        fx.lands_at("src/use.ts", "save", 4),
        "candidate(UntypedReceiver) src/store.ts::useStore.save"
    );
}
