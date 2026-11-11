//! The Rust resolver on real git repositories: every Rust case of the impact and
//! symbol corpus read by the callers it expects, and the shapes the old model
//! answered that the facts layer must keep. The expectations come from the case
//! files and from reading each fixture; nothing here runs diffctx to learn an
//! answer.
//!
//! The corpus judge pairs the resolver's relations to a case's target with the
//! case's own, by relation kind and resolution, and compares only what the
//! resolver answers: `tests_reach`, `references_path` and the like belong to
//! the layers above it. Two readings are fixed as in the Python judge: an
//! `imports` or `reexports` binding is no caller, and a caller that is itself a
//! changed definition may be listed or left out.

mod snapshot_fixture;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name, Scope, semantic_changes};
use _diffctx::resolve::{Binding, EvidenceKind, Relation, Resolution, RustResolver};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: RustResolver,
    base: RustResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = RustResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = RustResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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
                    && !matches!(b.relation, Relation::Imports | Relation::Reexports)
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

/// The definitions a node names in the head, and those only the base has. In a
/// range that changed something a node means the definitions of that name the
/// range changed: another module's definition of the same name, or a field
/// called like the function, is not the case's subject.
fn definitions_of<'f>(
    fixture: &'f Fixture,
    node: &Node,
    changed: &Changed,
) -> Vec<(&'f RustResolver, DefKey)> {
    let named = |resolver: &'f RustResolver| -> Vec<DefKey> {
        resolver
            .definitions()
            .iter()
            .filter(|d| {
                node.covers(&Node {
                    path: d.file.to_string(),
                    symbol: Some(d.key.local.name.to_string()),
                })
            })
            .filter(|d| {
                changed.0.is_empty()
                    || changed
                        .0
                        .contains(&(d.file.to_string(), dotted(&d.key.local)))
            })
            .map(|d| d.key.clone())
            .collect()
    };
    let in_head = named(&fixture.head);
    let mut found: Vec<(&RustResolver, DefKey)> =
        in_head.iter().map(|k| (&fixture.head, k.clone())).collect();
    for key in named(&fixture.base) {
        if !in_head.contains(&key) {
            found.push((&fixture.base, key));
        }
    }
    found
}

fn groups(fixture: &Fixture, node: &Node, changed: &Changed) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (resolver, key) in definitions_of(fixture, node, changed) {
        for binding in resolver.references_to(&key) {
            // An import or a re-export moves the name; a type use names it; the
            // guard of a script is the run of the script. None is a caller.
            if !binding.relation.uses() || binding.entry_guard {
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
            let twin = groups.iter_mut().find(|g| {
                g.from == from && g.kind == binding.relation && g.resolution == resolution
            });
            match twin {
                Some(twin) => {
                    twin.lines.insert(line);
                }
                None => groups.push(Group {
                    from,
                    kind: binding.relation,
                    resolution,
                    lines: BTreeSet::from([line]),
                    in_diff,
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
        // A caller that is itself changed may be listed or left out.
        if !taken[i] && exact && !group.in_diff {
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

/// The definitions of the symbol the case asks for: its name, in the file the
/// query names when it names one (`PATH:NAME`).
fn judge_definitions(fixture: &Fixture, case: &Case, failures: &mut Vec<String>) {
    let (Some(expected), Some(symbol)) = (&case.expect.definitions, &case.query.symbol) else {
        return;
    };
    let (file, name) = match symbol.rsplit_once(':') {
        Some((file, name)) => (Some(file), name),
        None => (None, symbol.as_str()),
    };
    let last = |text: &str| text.rsplit(['.', ':']).next().unwrap_or(text).to_string();
    let found: BTreeSet<String> = fixture
        .head
        .definitions()
        .iter()
        .filter(|d| d.key.local.name.as_str() == name)
        .filter(|d| file.is_none_or(|f| d.file.as_str() == f))
        .map(|d| format!("{}::{}", d.file, d.key.local.name))
        .collect();
    let wanted: BTreeSet<String> = expected
        .iter()
        .map(|d| {
            let node = Node::parse(d);
            format!("{}::{}", node.path, last(&node.symbol.unwrap_or_default()))
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
    let mut failures = Vec::new();
    for (target, list) in &wants {
        judge_target(
            &fixture,
            target,
            list,
            &changed,
            case.expect.exact,
            &mut failures,
        );
    }
    // A decoy key that is not also a listed target asserts only that its decoys are
    // unrelated (`judge_decoys`): other callers of it are no one's concern here.
    judge_decoys(&fixture, &case.expect, &changed, &mut failures);
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

/// The Rust cases the resolver cannot answer yet, each with the reason. The
/// list is held both ways: a listed case that turns green fails the run, as
/// does a red case that is not listed.
const RED: &[(&str, &str)] = &[
    (
        "impact/rust/rust_removed_field_read_in_the_middle_of_a_chain_is_not_absent",
        "the candidate is the removal side's: a read through a receiver nothing types is `UntypedRead` for the resolver, and decide() names the surviving read of a removed member's name as a possible use; no binding states it",
    ),
    (
        "impact/rust/rust_removed_private_duplicate_has_no_callers",
        "gh#519: the decoys are the base's own callers of the removed private `absorb`; the \
     resolver binds the base as the base is, `classify` calls it there, and which of the base's \
     callers still reach a definition at the head is the cell's stale-use rule (decide.rs)",
    ),
];

#[test]
fn the_rust_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("rust");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/rust/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 20, "the walk found {} cases", cases.len());
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
        "rust corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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

const MANIFEST: &str = "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// One package `fx` with `files` beside its manifest.
fn package(files: &[(&str, &str)]) -> Fixture {
    let mut all: Vec<(&str, &str)> = vec![("Cargo.toml", MANIFEST)];
    all.extend_from_slice(files);
    snapshot(&all)
}

/// The 1-based line of the first `needle` in `text`.
fn line(text: &str, needle: &str) -> u32 {
    let at = text
        .find(needle)
        .unwrap_or_else(|| panic!("`{needle}` is not in the fixture"));
    text[..at].matches('\n').count() as u32 + 1
}

fn imports_at(fx: &Fixture, file: &str, name: &str, line: u32) -> Vec<String> {
    fx.head
        .bindings()
        .iter()
        .filter(|b| {
            b.file.as_str() == file
                && b.name.as_str() == name
                && b.site.lines.0 == line
                && matches!(b.relation, Relation::Imports | Relation::Reexports)
        })
        .map(|b| format!("{} {}", b.relation.name(), fx.lands(b)))
        .collect()
}

const UTIL: &str =
    "pub fn helper() -> u32 {\n    1\n}\n\npub fn other() -> u32 {\n    helper()\n}\n";
const APP: &str = "use crate::util::helper;\nuse crate::util as u;\n\npub fn a() -> u32 {\n    helper()\n}\n\npub fn b() -> u32 {\n    u::helper() + crate::util::other()\n}\n\npub fn c() -> u32 {\n    super::util::helper()\n}\n";

/// A name reaches an item through its `use`, a renamed module, `crate::`,
/// `super::` and the module around it.
#[test]
fn paths_reach_items_through_imports_crate_and_super() {
    let fx = package(&[
        ("src/lib.rs", "pub mod app;\npub mod util;\n"),
        ("src/util.rs", UTIL),
        ("src/app.rs", APP),
    ]);
    let helper = "src/util.rs::helper";
    assert_eq!(
        fx.lands_at("src/util.rs", "helper", line(UTIL, "    helper()")),
        helper
    );
    assert_eq!(
        fx.lands_at("src/app.rs", "helper", line(APP, "    helper()")),
        helper
    );
    assert_eq!(
        fx.lands_at("src/app.rs", "helper", line(APP, "u::helper")),
        helper
    );
    assert_eq!(
        fx.lands_at("src/app.rs", "other", line(APP, "crate::util::other")),
        "src/util.rs::other"
    );
    assert_eq!(
        fx.lands_at("src/app.rs", "helper", line(APP, "super::util::helper")),
        helper
    );
    assert_eq!(
        evidence(fx.at("src/app.rs", "helper", line(APP, "    helper()"))).0,
        EvidenceKind::ImportedName
    );
    assert_eq!(
        imports_at(&fx, "src/app.rs", "helper", 1),
        ["imports src/util.rs::helper"]
    );
}

const SHAPES: &str =
    "pub struct Square(pub u32);\n\npub fn area(s: &Square) -> u32 {\n    s.0 * s.0\n}\n";
const PRELUDE_RS: &str = "pub use crate::shapes::{area, Square};\n";
const USER: &str = "use crate::prelude::*;\nuse crate::shapes::area as measure;\n\npub fn f(s: &Square) -> u32 {\n    area(s) + measure(s)\n}\n";

/// A glob, a renamed import and a `pub use` chain all land on the one item.
#[test]
fn globs_renames_and_reexports_land_on_the_item() {
    let fx = package(&[
        (
            "src/lib.rs",
            "pub mod prelude;\npub mod shapes;\npub mod user;\n",
        ),
        ("src/shapes.rs", SHAPES),
        ("src/prelude.rs", PRELUDE_RS),
        ("src/user.rs", USER),
    ]);
    let five = line(USER, "area(s) + measure(s)");
    assert_eq!(
        fx.lands_at("src/user.rs", "area", five),
        "src/shapes.rs::area"
    );
    assert_eq!(
        evidence(fx.at("src/user.rs", "area", five)).0,
        EvidenceKind::StarImport
    );
    assert_eq!(
        fx.lands_at("src/user.rs", "measure", five),
        "src/shapes.rs::area"
    );
    assert_eq!(
        fx.lands_at("src/user.rs", "Square", line(USER, "s: &Square")),
        "src/shapes.rs::Square"
    );
    assert_eq!(
        imports_at(&fx, "src/prelude.rs", "area", 1),
        ["reexports src/shapes.rs::area"]
    );
}

const NESTED: &str = "pub fn top() -> u32 {\n    1\n}\n\npub fn caller() -> u32 {\n    fn top() -> u32 {\n        2\n    }\n    top()\n}\n\npub mod inner {\n    pub fn deep() -> u32 {\n        super::top()\n    }\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn uses_top() {\n        assert_eq!(top(), 1);\n    }\n}\n";

/// An item of the function around a call shadows the module's; `super::` leaves
/// an inline module; a test module takes the module's names with `use super::*`.
#[test]
fn local_items_shadow_and_inline_modules_see_their_parent() {
    let fx = package(&[("src/lib.rs", "pub mod m;\n"), ("src/m.rs", NESTED)]);
    assert_eq!(
        fx.lands_at("src/m.rs", "top", line(NESTED, "    top()\n}")),
        "src/m.rs::caller.top"
    );
    assert_eq!(
        fx.lands_at("src/m.rs", "top", line(NESTED, "super::top()")),
        "src/m.rs::top"
    );
    assert_eq!(
        fx.lands_at("src/m.rs", "top", line(NESTED, "assert_eq!(top()")),
        "src/m.rs::top"
    );
}

const CART: &str = "pub struct Engine {\n    pub gear: u32,\n}\n\nimpl Engine {\n    pub fn new() -> Self {\n        Engine { gear: 1 }\n    }\n\n    pub fn shift(&mut self) {\n        self.gear += 1;\n    }\n\n    pub fn twice(&mut self) {\n        self.shift();\n    }\n}\n\npub struct Car {\n    engine: Engine,\n}\n\nimpl Car {\n    pub fn drive(&mut self) {\n        self.engine.shift();\n    }\n}\n\npub fn build() -> Engine {\n    Engine::new()\n}\n\npub fn run(e: &mut Engine) -> u32 {\n    e.shift();\n    let mut made = Engine::new();\n    made.shift();\n    let mut built = build();\n    built.shift();\n    stranger().shift();\n    e.gear\n}\n";

/// A method is reached through the value whose type the syntax states: `self`,
/// a field of it, a typed parameter, a constructor, a function's declared return.
#[test]
fn methods_are_reached_through_typed_receivers() {
    let fx = package(&[("src/lib.rs", "pub mod cart;\n"), ("src/cart.rs", CART)]);
    let shift = "src/cart.rs::Engine.shift";
    let at = |needle: &str| line(CART, needle);
    assert_eq!(
        fx.lands_at("src/cart.rs", "gear", at("self.gear += 1")),
        "src/cart.rs::Engine.gear"
    );
    assert_eq!(
        fx.at("src/cart.rs", "gear", at("self.gear += 1")).relation,
        Relation::Writes
    );
    assert_eq!(
        fx.lands_at("src/cart.rs", "shift", at("self.shift()")),
        shift
    );
    assert_eq!(
        evidence(fx.at("src/cart.rs", "shift", at("self.shift()"))).0,
        EvidenceKind::SelfReceiver
    );
    assert_eq!(
        fx.lands_at("src/cart.rs", "shift", at("self.engine.shift()")),
        shift
    );
    assert_eq!(fx.lands_at("src/cart.rs", "shift", at("e.shift()")), shift);
    assert_eq!(
        fx.lands_at("src/cart.rs", "shift", at("made.shift()")),
        shift
    );
    assert_eq!(
        fx.lands_at("src/cart.rs", "shift", at("built.shift()")),
        shift
    );
    assert_eq!(
        fx.lands_at("src/cart.rs", "shift", at("stranger().shift()")),
        format!("candidate(UntypedReceiver) {shift}")
    );
    assert_eq!(
        fx.lands_at("src/cart.rs", "gear", at("    e.gear\n")),
        "src/cart.rs::Engine.gear"
    );
    assert_eq!(
        fx.at("src/cart.rs", "gear", at("    e.gear\n")).relation,
        Relation::Reads
    );
    assert_eq!(
        fx.lands_at("src/cart.rs", "new", at("    Engine::new()")),
        "src/cart.rs::Engine.new"
    );
    // `Engine::new()` also reads the type it is written through.
    assert_eq!(
        fx.lands_at("src/cart.rs", "Engine", at("    Engine::new()")),
        "src/cart.rs::Engine"
    );
}

const SHAPE_RS: &str = "pub trait Shape {\n    fn area(&self) -> f64;\n    fn describe(&self) -> String {\n        format!(\"{}\", self.area())\n    }\n}\n\npub struct Circle(pub f64);\n\nimpl Shape for Circle {\n    fn area(&self) -> f64 {\n        self.0\n    }\n}\n\npub struct Square(pub f64);\n\nimpl Shape for Square {\n    fn area(&self) -> f64 {\n        self.0 * self.0\n    }\n}\n\npub fn via_dyn(s: &dyn Shape) -> f64 {\n    s.area()\n}\n\npub fn via_generic<T: Shape>(s: T) -> f64 {\n    s.area()\n}\n\npub fn via_impl(s: impl Shape) -> f64 {\n    s.area()\n}\n\npub fn direct(c: &Circle) -> f64 {\n    c.area()\n}\n\npub fn defaulted(c: &Circle) -> String {\n    c.describe()\n}\n";

/// A trait object, `impl Tr` and a generic with a bound are every implementer's
/// method, as candidates; a concrete type's own is resolved; a trait impl
/// overrides the trait's method; a default method is found through the impl.
#[test]
fn trait_dispatch_is_candidates_and_a_concrete_type_is_resolved() {
    let fx = package(&[
        ("src/lib.rs", "pub mod shape;\n"),
        ("src/shape.rs", SHAPE_RS),
    ]);
    let all = "candidate(Dispatch) src/shape.rs::Circle.area, src/shape.rs::Shape.area, src/shape.rs::Square.area";
    let at = |needle: &str| line(SHAPE_RS, needle);
    for needle in [
        "    s.area()\n}\n\npub fn via_generic",
        "    s.area()\n}\n\npub fn via_impl",
        "    s.area()\n}\n\npub fn direct",
    ] {
        assert_eq!(
            fx.lands_at("src/shape.rs", "area", at(needle)),
            all,
            "{needle}"
        );
    }
    assert_eq!(
        fx.lands_at("src/shape.rs", "area", at("self.area()")),
        all,
        "a default method does not know its implementer"
    );
    assert_eq!(
        fx.lands_at("src/shape.rs", "area", at("    c.area()")),
        "src/shape.rs::Circle.area"
    );
    assert_eq!(
        fx.lands_at("src/shape.rs", "describe", at("c.describe()")),
        "src/shape.rs::Shape.describe"
    );
    assert_eq!(
        evidence(fx.at("src/shape.rs", "describe", at("c.describe()"))).0,
        EvidenceKind::Inherited
    );
    let method = at("impl Shape for Circle") + 1;
    let overrides = fx.at("src/shape.rs", "area", method);
    assert_eq!(overrides.relation, Relation::Overrides);
    assert_eq!(fx.lands(overrides), "src/shape.rs::Shape.area");
    let header = fx.at("src/shape.rs", "Shape", at("impl Shape for Circle"));
    assert_eq!(header.relation, Relation::Overrides);
    assert_eq!(fx.lands(header), "src/shape.rs::Shape");
}

const KINDS: &str = "pub enum Mode {\n    Fast,\n    Slow(u32),\n}\n\npub struct Unit;\n\nmacro_rules! twice {\n    ($e:expr) => {\n        $e + $e\n    };\n}\n\npub fn pick(m: Mode) -> u32 {\n    let _u = Unit;\n    match m {\n        Mode::Fast => 1,\n        Mode::Slow(n) => twice!(n),\n    }\n}\n\npub fn make() -> Mode {\n    Mode::Slow(3)\n}\n\npub fn printed() {\n    println!(\"x\");\n}\n";

/// Variants, unit structs and macros are definitions; a constructor call
/// instantiates; the standard library's macros are not the repository's.
#[test]
fn variants_unit_structs_and_macros_are_found() {
    let fx = package(&[("src/lib.rs", "pub mod kinds;\n"), ("src/kinds.rs", KINDS)]);
    let at = |needle: &str| line(KINDS, needle);
    assert_eq!(
        fx.lands_at("src/kinds.rs", "Fast", at("Mode::Fast")),
        "src/kinds.rs::Mode.Fast"
    );
    assert_eq!(
        fx.lands_at("src/kinds.rs", "Slow", at("Mode::Slow(n)")),
        "src/kinds.rs::Mode.Slow"
    );
    assert_eq!(
        fx.at("src/kinds.rs", "Slow", at("Mode::Slow(3)")).relation,
        Relation::Instantiates
    );
    assert_eq!(
        fx.lands_at("src/kinds.rs", "Unit", at("= Unit;")),
        "src/kinds.rs::Unit"
    );
    assert_eq!(
        fx.lands_at("src/kinds.rs", "twice", at("twice!(n)")),
        "src/kinds.rs::twice"
    );
    assert_eq!(
        fx.lands_at("src/kinds.rs", "println", at("println!")),
        "unresolved(Builtin)"
    );
    // The enum is read where one of its variants is named.
    assert_eq!(
        fx.lands_at("src/kinds.rs", "Mode", at("Mode::Fast")),
        "src/kinds.rs::Mode"
    );
}

/// A path dependency the manifest declares is a crate this one may name; a
/// package nothing declares is not, and an outside crate is no repository code.
#[test]
fn a_declared_path_dependency_is_found_and_an_undeclared_one_is_not() {
    let app = "use shop_core::total;\n\npub fn charge() -> u32 {\n    total()\n}\n\npub fn again() -> u32 {\n    shop_core::total()\n}\n";
    let stray = "pub fn report() -> u32 {\n    shop_core::total()\n}\n";
    let fx = snapshot(&[
        (
            "core/Cargo.toml",
            "[package]\nname = \"shop-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("core/src/lib.rs", "pub fn total() -> u32 {\n    1\n}\n"),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nshop-core = { path = \"../core\" }\n",
        ),
        ("app/src/lib.rs", app),
        (
            "stray/Cargo.toml",
            "[package]\nname = \"stray\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        ("stray/src/lib.rs", stray),
    ]);
    let total = "core/src/lib.rs::total";
    assert_eq!(
        fx.lands_at("app/src/lib.rs", "total", line(app, "    total()")),
        total
    );
    assert_eq!(
        fx.lands_at("app/src/lib.rs", "total", line(app, "shop_core::total()")),
        total
    );
    assert_eq!(
        fx.lands_at("stray/src/lib.rs", "total", 2),
        "unresolved(External)"
    );
}

/// `mod` declarations and `#[path]` place a file in its crate's module tree.
#[test]
fn the_module_tree_follows_mod_declarations_and_path_attributes() {
    let leaf = "pub fn go() -> u32 {\n    crate::engine::start() + super::sibling()\n}\n";
    let fx = package(&[
        (
            "src/lib.rs",
            "#[path = \"impl/engine.rs\"]\nmod engine;\npub mod outer;\n",
        ),
        ("src/impl/engine.rs", "pub fn start() -> u32 {\n    1\n}\n"),
        (
            "src/outer/mod.rs",
            "pub mod leaf;\n\npub fn sibling() -> u32 {\n    2\n}\n",
        ),
        ("src/outer/leaf.rs", leaf),
    ]);
    assert_eq!(
        fx.lands_at("src/outer/leaf.rs", "start", 2),
        "src/impl/engine.rs::start"
    );
    assert_eq!(
        fx.lands_at("src/outer/leaf.rs", "sibling", 2),
        "src/outer/mod.rs::sibling"
    );
}

/// A crate outside the repository, the standard library and the prelude name
/// nothing the repository defines.
#[test]
fn std_and_outside_crates_are_not_the_repositorys() {
    let src = "use std::collections::HashMap;\nuse serde::Serialize;\n\npub fn f() -> usize {\n    let m: HashMap<u32, u32> = HashMap::new();\n    let v: Vec<u8> = Vec::new();\n    let n = m.len();\n    n + v.len()\n}\n";
    let fx = package(&[("src/lib.rs", "pub mod s;\n"), ("src/s.rs", src)]);
    let at = |needle: &str| line(src, needle);
    assert_eq!(
        fx.lands_at("src/s.rs", "len", at("m.len()")),
        "unresolved(ExternalBase)"
    );
    assert_eq!(
        fx.lands_at("src/s.rs", "new", at("HashMap::new()")),
        "unresolved(External)"
    );
    assert_eq!(
        imports_at(&fx, "src/s.rs", "HashMap", 1),
        ["imports unresolved(External)"]
    );
    assert_eq!(
        imports_at(&fx, "src/s.rs", "Serialize", 2),
        ["imports unresolved(External)"]
    );
}

/// A call on a value nothing types is a candidate among the methods of the
/// types the file names, and no other file's.
#[test]
fn an_untyped_receiver_is_a_candidate_among_the_types_the_file_names() {
    let user = "use crate::engine::Engine;\n\npub fn f(x: Mystery) {\n    x.shift();\n}\n";
    let fx = package(&[
        (
            "src/lib.rs",
            "pub mod engine;\npub mod other;\npub mod user;\n",
        ),
        (
            "src/engine.rs",
            "pub struct Engine;\n\nimpl Engine {\n    pub fn shift(&self) {}\n}\n",
        ),
        (
            "src/other.rs",
            "pub struct Other;\n\nimpl Other {\n    pub fn shift(&self) {}\n}\n",
        ),
        ("src/user.rs", user),
    ]);
    assert_eq!(
        fx.lands_at("src/user.rs", "shift", 4),
        "candidate(UntypedReceiver) src/engine.rs::Engine.shift"
    );
}

/// An enum's variants come in with `use Enum::*`.
#[test]
fn a_glob_of_an_enum_brings_its_variants() {
    let src = "pub enum Mode {\n    Fast,\n    Slow,\n}\n\nuse Mode::*;\n\npub fn pick(m: Mode) -> u32 {\n    match m {\n        Fast => 1,\n        Slow => 2,\n    }\n}\n";
    let fx = package(&[("src/lib.rs", "pub mod m;\n"), ("src/m.rs", src)]);
    assert_eq!(fx.lands_at("src/m.rs", "Fast", 10), "src/m.rs::Mode.Fast");
}

/// A module is public when every `mod` from the crate's root down declares it
/// `pub`; a binary's modules are not.
#[test]
fn a_module_is_public_when_every_mod_to_it_is() {
    let fx = package(&[
        ("src/lib.rs", "pub mod open;\nmod hidden;\n"),
        ("src/open.rs", "pub mod deep;\n\npub fn f() {}\n"),
        ("src/open/deep.rs", "pub fn g() {}\n"),
        ("src/hidden.rs", "pub fn h() {}\n"),
    ]);
    for (file, public) in [
        ("src/lib.rs", true),
        ("src/open.rs", true),
        ("src/open/deep.rs", true),
        ("src/hidden.rs", false),
    ] {
        assert_eq!(fx.head.is_public_module(file), public, "{file}");
    }
}

/// A private method is named only in its module and the ones below it: a value
/// nothing types does not reach it from a sibling.
#[test]
fn a_private_method_is_no_candidate_outside_its_module() {
    let user = "use crate::engine::Engine;\n\npub fn f(x: Mystery) {\n    x.shift();\n}\n";
    let fx = package(&[
        ("src/lib.rs", "pub mod engine;\npub mod user;\n"),
        (
            "src/engine.rs",
            "pub struct Engine;\n\nimpl Engine {\n    fn shift(&self) {}\n}\n",
        ),
        ("src/user.rs", user),
    ]);
    assert_eq!(
        fx.lands_at("src/user.rs", "shift", 4),
        "unresolved(NoCandidates)"
    );
}
