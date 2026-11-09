//! The Go resolver on real git repositories: every Go case of the impact and
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
use _diffctx::resolve::{Binding, EvidenceKind, GoResolver, Relation, Resolution};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: GoResolver,
    base: GoResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = GoResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = GoResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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
) -> Vec<(&'f GoResolver, DefKey)> {
    let named = |resolver: &'f GoResolver| -> Vec<DefKey> {
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
    let mut found: Vec<(&GoResolver, DefKey)> =
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

/// The Go cases the resolver cannot answer yet, each with the reason. The list
/// is held both ways: a listed case that turns green fails the run, as does a
/// red case that is not listed.
const RED: &[(&str, &str)] = &[];

#[test]
fn the_go_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("go");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/go/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 9, "the walk found {} cases", cases.len());
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
        "go corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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

const GO_MOD: &str = "module example.com/fx\n\ngo 1.22\n";

/// One module `example.com/fx` with `files` beside its `go.mod`.
fn module(files: &[(&str, &str)]) -> Fixture {
    let mut all: Vec<(&str, &str)> = vec![("go.mod", GO_MOD)];
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

const PRICING: &str = "package pricing\n\nfunc Total(n int) int {\n\treturn helper(n)\n}\n\nfunc helper(n int) int {\n\treturn n\n}\n";
const PRICING_TEST: &str =
    "package pricing\n\nimport \"testing\"\n\nfunc TestTotal(t *testing.T) {\n\tTotal(1)\n}\n";
const ALIASED: &str = "package app\n\nimport p \"example.com/fx/pricing\"\n\nfunc A() int {\n\treturn p.Total(1)\n}\n";
const PLAIN: &str = "package app\n\nimport (\n\t\"fmt\"\n\t\"example.com/fx/pricing\"\n)\n\nfunc B() int {\n\tfmt.Println(\"x\")\n\treturn pricing.Total(2)\n}\n\nfunc C() int {\n\treturn missing.Total(3)\n}\n";

/// A name reaches a function through the package its import binds, by alias or
/// by the package's own name, and through its own package, test files
/// included; an unknown qualifier and the standard library are not the
/// repository's.
#[test]
fn packages_are_reached_through_their_imports_and_their_directory() {
    let fx = module(&[
        ("pricing/pricing.go", PRICING),
        ("pricing/pricing_test.go", PRICING_TEST),
        ("app/aliased.go", ALIASED),
        ("app/plain.go", PLAIN),
    ]);
    let total = "pricing/pricing.go::Total";
    assert_eq!(
        fx.lands_at("app/aliased.go", "Total", line(ALIASED, "p.Total")),
        total
    );
    assert_eq!(
        fx.lands_at("app/plain.go", "Total", line(PLAIN, "pricing.Total")),
        total
    );
    assert_eq!(
        evidence(fx.at("app/plain.go", "Total", line(PLAIN, "pricing.Total"))).0,
        EvidenceKind::ModuleAttribute
    );
    assert_eq!(
        fx.lands_at("pricing/pricing.go", "helper", line(PRICING, "helper(n)")),
        "pricing/pricing.go::helper"
    );
    assert_eq!(
        fx.lands_at(
            "pricing/pricing_test.go",
            "Total",
            line(PRICING_TEST, "Total(1)")
        ),
        total
    );
    assert_eq!(
        fx.lands_at("app/plain.go", "Println", line(PLAIN, "fmt.Println")),
        "unresolved(External)"
    );
    assert_eq!(
        fx.lands_at("app/plain.go", "Total", line(PLAIN, "missing.Total")),
        "unresolved(NoBinding)"
    );
}

/// An import is a binding of its own: the package's first file, nothing for a
/// blank one.
#[test]
fn an_import_lands_on_its_package_and_a_blank_one_binds_nothing() {
    let blank = "package blank\n\nimport _ \"example.com/fx/pricing\"\n";
    let fx = module(&[
        ("pricing/pricing.go", PRICING),
        ("pricing/pricing_test.go", PRICING_TEST),
        ("app/aliased.go", ALIASED),
        ("blank/blank.go", blank),
    ]);
    let imports: Vec<String> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "app/aliased.go" && b.relation == Relation::Imports)
        .map(|b| format!("{} {}", b.name.as_str(), fx.lands(b)))
        .collect();
    assert_eq!(imports, ["p module pricing/pricing.go"]);
    assert!(
        !fx.head
            .bindings()
            .iter()
            .any(|b| b.file.as_str() == "blank/blank.go")
    );
}

const CART: &str = "package cart\n\ntype Cart struct{ n int }\n\nfunc New() *Cart {\n\treturn &Cart{}\n}\n\nfunc (c *Cart) Add(k int) {\n\tc.n += k\n}\n\nfunc (c Cart) Count() int {\n\treturn c.n\n}\n";
const FILL: &str = "package fill\n\nimport \"example.com/fx/cart\"\n\nfunc Fill(c *cart.Cart) {\n\tc.Add(1)\n\tx := cart.New()\n\tx.Add(2)\n\tcart.New().Count()\n\ty := stranger()\n\ty.Add(4)\n}\n";

/// A method is reached through the type the syntax states: a typed parameter,
/// what a called function declares it returns, directly or as a local; a value
/// nothing types is a candidate among the types the file can name. A field a
/// method assigns is written, and a literal instantiates its type.
#[test]
fn methods_are_reached_through_typed_receivers() {
    let fx = module(&[("cart/cart.go", CART), ("fill/fill.go", FILL)]);
    let add = "cart/cart.go::Cart.Add";
    let at = |needle: &str| line(FILL, needle);
    assert_eq!(fx.lands_at("fill/fill.go", "Add", at("c.Add(1)")), add);
    assert_eq!(
        evidence(fx.at("fill/fill.go", "Add", at("c.Add(1)"))).0,
        EvidenceKind::TypedReceiver
    );
    assert_eq!(fx.lands_at("fill/fill.go", "Add", at("x.Add(2)")), add);
    assert_eq!(
        evidence(fx.at("fill/fill.go", "Add", at("x.Add(2)"))).0,
        EvidenceKind::FactoryReturn
    );
    assert_eq!(
        fx.lands_at("fill/fill.go", "Count", at("cart.New().Count()")),
        "cart/cart.go::Cart.Count"
    );
    assert_eq!(
        fx.lands_at("fill/fill.go", "Add", at("y.Add(4)")),
        format!("candidate(UntypedReceiver) {add}")
    );
    let write = fx.at("cart/cart.go", "n", line(CART, "c.n += k"));
    assert_eq!(fx.lands(write), "cart/cart.go::Cart.n");
    assert_eq!(write.relation, Relation::Writes);
    let literal = fx.at("cart/cart.go", "Cart", line(CART, "&Cart{}"));
    assert_eq!(fx.lands(literal), "cart/cart.go::Cart");
    assert_eq!(literal.relation, Relation::Instantiates);
}

const SHAPES: &str = "package shapes\n\ntype Namer interface {\n\tName() string\n}\n\ntype Base struct{}\n\nfunc (Base) Name() string {\n\treturn \"b\"\n}\n\ntype Circle struct {\n\tBase\n\tr int\n}\n\ntype Other struct{}\n\nfunc (Other) Name() string {\n\treturn \"o\"\n}\n\nfunc (Other) Extra() {}\n\nfunc Show(c Circle, n Namer) string {\n\ta := c.Name()\n\tb := n.Name()\n\treturn a + b\n}\n";

/// An embedded struct's methods are promoted and resolved; a call through an
/// interface is every type's method that has all the interface's, as
/// candidates, and never a resolution.
#[test]
fn promotion_is_resolved_and_an_interface_call_is_a_candidate() {
    let fx = module(&[("shapes/shapes.go", SHAPES)]);
    let promoted = fx.at("shapes/shapes.go", "Name", line(SHAPES, "a := c.Name()"));
    assert_eq!(fx.lands(promoted), "shapes/shapes.go::Base.Name");
    assert_eq!(evidence(promoted).0, EvidenceKind::Inherited);
    assert_eq!(
        fx.lands_at("shapes/shapes.go", "Name", line(SHAPES, "b := n.Name()")),
        "candidate(Dispatch) shapes/shapes.go::Base.Name, shapes/shapes.go::Namer.Name, \
         shapes/shapes.go::Other.Name"
    );
}

const SECRET: &str = "package secret\n\nfunc Key() string {\n\treturn \"k\"\n}\n";
const INSIDE: &str = "package use\n\nimport \"example.com/fx/lib/internal/secret\"\n\nfunc Get() string {\n\treturn secret.Key()\n}\n";
const OUTSIDE: &str = "package other\n\nimport \"example.com/fx/lib/internal/secret\"\n\nfunc Get() string {\n\treturn secret.Key()\n}\n";
const DOT: &str = "package dot\n\nfunc Helper() int {\n\treturn 1\n}\n";
const DOT_USER: &str =
    "package dotuser\n\nimport . \"example.com/fx/dot\"\n\nfunc F() int {\n\treturn Helper()\n}\n";

/// A package under `internal/` is imported from the tree its parent holds and
/// nowhere else; a dot import brings the exported names in unqualified.
#[test]
fn internal_packages_and_dot_imports() {
    let fx = module(&[
        ("lib/internal/secret/secret.go", SECRET),
        ("lib/use/use.go", INSIDE),
        ("other/use.go", OUTSIDE),
        ("dot/dot.go", DOT),
        ("dotuser/user.go", DOT_USER),
    ]);
    assert_eq!(
        fx.lands_at("lib/use/use.go", "Key", line(INSIDE, "secret.Key")),
        "lib/internal/secret/secret.go::Key"
    );
    assert_eq!(
        fx.lands_at("other/use.go", "Key", line(OUTSIDE, "secret.Key")),
        "unresolved(OutsideProject)"
    );
    let brought = fx.at("dotuser/user.go", "Helper", line(DOT_USER, "Helper()"));
    assert_eq!(fx.lands(brought), "dot/dot.go::Helper");
    assert_eq!(evidence(brought).0, EvidenceKind::StarImport);
    assert!(fx.head.is_public_package("dot/dot.go"));
    assert!(!fx.head.is_public_package("lib/internal/secret/secret.go"));
}

const CONFIG: &str = "package config\n\ntype Settings struct {\n\tPort int\n}\n\nvar Current = Settings{Port: 80}\n\nvar Typed Settings\n\nfunc Double(n int) int {\n\treturn n * 2\n}\n\nfunc Describe(s Settings) int {\n\treturn s.Port\n}\n";
const USE: &str = "package use\n\nimport \"example.com/fx/config\"\n\nfunc Port() int {\n\treturn config.Typed.Port\n}\n\nfunc Apply() int {\n\treturn run(config.Double)\n}\n\nfunc run(f func(int) int) int {\n\treturn f(1)\n}\n";

/// A package variable is typed by its declaration; a key of a literal writes the
/// field; a name handed to a call is a function passed by name; a name in a type
/// position is a type use.
#[test]
fn package_variables_keys_and_names_handed_on() {
    let fx = module(&[("config/config.go", CONFIG), ("use/use.go", USE)]);
    let read = fx.at("use/use.go", "Port", line(USE, "config.Typed.Port"));
    assert_eq!(fx.lands(read), "config/config.go::Settings.Port");
    assert_eq!(read.relation, Relation::Reads);
    let key = fx.at("config/config.go", "Port", line(CONFIG, "Port: 80"));
    assert_eq!(fx.lands(key), "config/config.go::Settings.Port");
    assert_eq!(key.relation, Relation::Writes);
    let handed = fx.at("use/use.go", "Double", line(USE, "config.Double"));
    assert_eq!(fx.lands(handed), "config/config.go::Double");
    assert_eq!(handed.relation, Relation::PassesAsValue);
    let typed = fx.at(
        "config/config.go",
        "Settings",
        line(CONFIG, "Describe(s Settings)"),
    );
    assert_eq!(fx.lands(typed), "config/config.go::Settings");
    assert_eq!(typed.relation, Relation::TypeUses);
}

/// A module the importer does not declare a dependency on is not reached; one it
/// does, by `require` or a local `replace`, is.
#[test]
fn another_module_is_reached_only_through_a_declared_dependency() {
    let cart = "package cart\n\nfunc Total() int {\n\treturn 1\n}\n";
    let tools =
        "package main\n\nimport \"example.com/fx/cart\"\n\nfunc main() {\n\t_ = cart.Total()\n}\n";
    let stray = "package stray\n\nimport \"example.com/fx/cart\"\n\nfunc Use() int {\n\treturn cart.Total()\n}\n";
    let fx = snapshot(&[
        ("go.mod", GO_MOD),
        ("cart/cart.go", cart),
        (
            "tools/go.mod",
            "module example.com/tools\n\ngo 1.22\n\nrequire example.com/fx v0.0.0\n\nreplace example.com/fx => ../\n",
        ),
        ("tools/main.go", tools),
        ("stray/go.mod", "module example.com/stray\n\ngo 1.22\n"),
        ("stray/use.go", stray),
    ]);
    assert_eq!(
        fx.lands_at("tools/main.go", "Total", line(tools, "cart.Total")),
        "cart/cart.go::Total"
    );
    assert_eq!(
        fx.lands_at("stray/use.go", "Total", line(stray, "cart.Total")),
        "unresolved(OutsideProject)"
    );
}
