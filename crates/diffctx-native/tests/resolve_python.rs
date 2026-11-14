//! The Python resolver on real git repositories: every Python case of the impact
//! and symbol corpus read by the callers it expects, and the shapes of the issues
//! the resolver was written for. The expectations come from the case files and
//! from reading each fixture; nothing here runs diffctx to learn an answer.
//!
//! The corpus judge pairs the resolver's relations to a case's target with the
//! case's own, by relation kind and resolution, and compares only what the
//! resolver answers: `tests_reach`, `references_path` and the like belong to
//! the layers above it. Two readings are fixed here and listed in the report:
//! an `imports` or `reexports` binding is no caller, and a caller that is itself
//! a changed definition may be listed or left out (cases disagree whether the
//! engine lists it; the resolver must find it).

mod snapshot_fixture;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name, Scope, semantic_changes};
use _diffctx::resolve::{
    Binding, CandidateReason, EvidenceKind, PythonResolver, Relation, Resolution, UnresolvedReason,
};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: PythonResolver,
    base: PythonResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = PythonResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = PythonResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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
    #[serde(default)]
    tags: Vec<String>,
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

/// Whether a node written `Class.member` names the member of that class: the
/// bare name `Node::covers` compares is no tell between `Base.x` and `Sub.x`, and
/// a use that lands on the one is not a use of the other. A node with no
/// container, or a definition at module level under one, is judged by the bare
/// name alone.
fn in_the_class_the_node_names(node: &Node, local: &LocalKey) -> bool {
    let Some(symbol) = node
        .symbol
        .as_deref()
        .filter(|_| !local.container.is_empty())
    else {
        return true;
    };
    let segments: Vec<&str> = symbol.split(['.', ':']).filter(|s| !s.is_empty()).collect();
    match segments.len().checked_sub(2).map(|at| segments[at]) {
        Some(container) => local
            .container
            .last()
            .is_some_and(|c| c.as_str() == container),
        None => true,
    }
}

/// The definitions a node names in the head, and those only the base has.
fn definitions_of<'f>(fixture: &'f Fixture, node: &Node) -> Vec<(&'f PythonResolver, DefKey)> {
    let named = |resolver: &'f PythonResolver| -> Vec<DefKey> {
        resolver
            .definitions()
            .iter()
            .filter(|d| {
                node.covers(&Node {
                    path: d.file.to_string(),
                    symbol: Some(d.key.local.name.to_string()),
                }) && in_the_class_the_node_names(node, &d.key.local)
            })
            .map(|d| d.key.clone())
            .collect()
    };
    let in_head = named(&fixture.head);
    let mut found: Vec<(&PythonResolver, DefKey)> =
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
    for (resolver, key) in definitions_of(fixture, node) {
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

/// Revision 12.3: a case tagged `local-of-definition` names a local of a
/// definition (a nested function that never leaves it). The resolver holds no
/// definition of the name, and each use the case lists answers
/// `unresolved(Local)`: the engine's `--symbol` lists those uses from the
/// facts, and the judge asserts what the resolver itself says of a local.
fn judge_local_of(fixture: &Fixture, case: &Case, failures: &mut Vec<String>) {
    for spec in &case.expect.relations {
        let (file, name, Some(line)) = (
            Node::parse(&spec.from).path,
            Node::parse(&spec.to).symbol.unwrap_or_default(),
            spec.site,
        ) else {
            continue;
        };
        let uses: Vec<String> = fixture
            .head
            .bindings()
            .iter()
            .filter(|b| {
                b.file.as_str() == file
                    && b.name.as_str() == name
                    && b.site.lines.0 == line
                    && !matches!(b.relation, Relation::Imports | Relation::Reexports)
            })
            .map(|b| fixture.lands(b))
            .collect();
        if uses != ["unresolved(Local)"] {
            failures.push(format!(
                "{file}:{line} `{name}`: expected a use of a local, found {uses:?}"
            ));
        }
    }
    if let Some(symbol) = &case.query.symbol {
        let defined = fixture
            .head
            .definitions()
            .iter()
            .filter(|d| d.key.local.name.as_str() == symbol)
            .count();
        if defined != 0 {
            failures.push(format!(
                "`{symbol}` is a local, but the resolver holds {defined} definition(s) of it"
            ));
        }
    }
}

enum Verdict {
    /// The case asserts nothing the resolver answers.
    NotApplicable,
    Green,
    Red(Vec<String>),
}

fn run_case(case: &Case) -> Verdict {
    if case.tags.iter().any(|t| t == "local-of-definition") {
        let mut failures = Vec::new();
        judge_local_of(&build(case), case, &mut failures);
        return if failures.is_empty() {
            Verdict::Green
        } else {
            Verdict::Red(failures)
        };
    }
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

/// The Python cases the resolver cannot answer yet, each with the reason. The
/// list is held both ways: a listed case that turns green fails the run, as
/// does a red case that is not listed.
const RED: &[(&str, &str)] = &[
    (
        "impact/python/py_removed_property_read_in_the_middle_of_an_untyped_chain_is_a_candidate",
        "the candidate is the removal side's: a read through a receiver nothing types is `UntypedRead` for the resolver, and decide() names the surviving read of a removed member's name as a possible use; no binding states it",
    ),
    (
        "impact/python/py_removed_property_read_through_an_external_call_result_is_a_candidate",
        "the candidate is the removal side's: a read through the result of a call into outside \
         code is `UntypedRead` for the resolver, and decide() names the surviving read of a \
         removed member's name as a possible use; no binding states it",
    ),
    (
        "impact/python/py_removed_field_lists_its_readers_only",
        "gh#400: the reader of a removed field is reported under the class, and the class's own \
         module-level instantiation is an extra caller; decide() maps a removed member to its \
         container, the resolver binds `settings.b` to the field",
    ),
    (
        "impact/python/py_pytest_fixture_is_used_by_the_tests_that_name_it",
        "gh#401: a fixture is used by naming it as a parameter; that is a framework model \
         (wave 7), not a binding the code states",
    ),
    (
        "impact/python/py_deleted_module_nothing_imports_lists_no_field_as_too_common_to_trace",
        "fj#151: open ticket; the case asserts what the engine does not yet answer",
    ),
    (
        "symbol/python/py_module_name_is_a_definition_and_its_importers_are_resolved",
        "gh#710: open ticket; the case asserts what the engine does not yet answer",
    ),
    (
        "impact/python/py_class_attribute_holding_a_changed_class_is_a_caller",
        "fj#186: open ticket; the case asserts what the engine does not yet answer",
    ),
    (
        "impact/python/gh293_fetch_call_site_links_to_the_route_handler_it_hits",
        "gh#293: a client `fetch` of a route path is a cross-language link to the handler that no \
         Python binding states; no resolver nor channel links them yet",
    ),
    (
        "symbol/python/gh482_dict_key_readers_across_languages_are_listed",
        "gh#482: a read of a dict key (`v[\"tone\"]`, `.get(\"verdict\")`, `s.verdict`) binds to \
         the function that builds the dict in no resolver; only the unrelated function of that \
         name is listed",
    ),
    (
        "symbol/python/gh708_keyword_argument_binds_the_callee_parameter",
        "gh#708: a keyword argument does not bind the callee's parameter, so the parameter is no \
         definition of the queried name and the callee is not listed",
    ),
    (
        "symbol/python/gh718_loop_variable_over_an_annotated_list_is_typed",
        "gh#718: a loop variable over a `list[Frame]` attribute stays untyped, so its reads are \
         not resolved to the member",
    ),
];

#[test]
fn the_python_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact", "symbol"] {
        let dir = root.join(mode).join("python");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/python/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 60, "the walk found {} cases", cases.len());
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
        "python corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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

/// gh#353: an attribute name binds across packages only when something links
/// the two; a read through an untyped receiver is any object's.
#[test]
fn attribute_names_do_not_bind_across_packages() {
    let fx = snapshot(&[
        (
            "pkg_b/src/bot.py",
            "class Bot:\n    def message(self, update):\n        return update\n\n    def callback(self, query):\n        return query\n",
        ),
        (
            "pkg_a/src/runtime.py",
            "def main(update, query, sent):\n    text = update.message\n    sent.callback(query)\n    return text\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("pkg_a/src/runtime.py", "message", 2),
        "unresolved(UntypedRead)"
    );
    assert_eq!(
        fx.lands_at("pkg_a/src/runtime.py", "callback", 3),
        "unresolved(NoCandidates)"
    );
}

/// gh#425: `Any` states no type, so the call is a candidate; a receiver built by
/// a factory annotated `-> "Model"` is that class, resolved.
#[test]
fn any_typed_receivers_are_candidates_and_factory_built_ones_are_resolved() {
    let fx = snapshot(&[
        (
            "models/model.py",
            "class Model:\n    @classmethod\n    async def from_config(cls, cfg) -> \"Model\":\n        return cls()\n\n    async def generate(self, prompt):\n        return prompt\n",
        ),
        (
            "workflow/nodes.py",
            "from typing import Any\n\n\nclass Research:\n    async def _research(self, key: str, model: Any, prompt: str):\n        return await model.generate(prompt)\n",
        ),
        (
            "tests/test_model.py",
            "from models.model import Model\n\n\nasync def test_search():\n    model = await Model.from_config({})\n    assert await model.generate(\"q\")\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("workflow/nodes.py", "generate", 6),
        "candidate(AnyReceiver) models/model.py::Model.generate"
    );
    let built = fx.at("tests/test_model.py", "generate", 6);
    assert_eq!(fx.lands(built), "models/model.py::Model.generate");
    assert_eq!(evidence(built).0, EvidenceKind::FactoryReturn);
    assert_eq!(built.relation, Relation::Calls);
}

/// gh#503, in Python: a module constant read by a function of its own file is
/// read, resolved, at the line of the read, and a same-named constant of another
/// module is not.
#[test]
fn a_module_constant_read_by_its_own_functions_is_resolved() {
    let fx = snapshot(&[
        (
            "panel.py",
            "import re\n\nPANEL_LINE = re.compile(\"Mixer panel:\")\nPANEL_KEYS = {\"tone\", \"mix\"}\n\n\ndef knobs(line):\n    return PANEL_LINE.match(line) and len(PANEL_KEYS)\n",
        ),
        (
            "other.py",
            "PANEL_LINE = \"other\"\n\n\ndef own():\n    return PANEL_LINE.upper()\n",
        ),
    ]);
    let head = fx.at("panel.py", "PANEL_LINE", 8);
    assert_eq!(head.relation, Relation::Reads);
    assert_eq!(fx.lands(head), "panel.py::PANEL_LINE");
    let keys = fx.at("panel.py", "PANEL_KEYS", 8);
    assert_eq!(keys.relation, Relation::Reads);
    assert_eq!(fx.lands(keys), "panel.py::PANEL_KEYS");
    assert_eq!(
        fx.lands_at("other.py", "PANEL_LINE", 5),
        "other.py::PANEL_LINE"
    );
}

/// gh#636: the name of a keyword argument is no reference, the call that is its
/// value is; a relative import lands on the sibling module.
#[test]
fn a_call_as_the_value_of_a_same_named_keyword_is_a_call() {
    let fx = snapshot(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/agree.py",
            "def agreement(night, values):\n    return {\"night\": night, \"n\": len(values)}\n",
        ),
        (
            "pkg/types.py",
            "from dataclasses import dataclass\n\n\n@dataclass\nclass Report:\n    agreement: dict\n",
        ),
        (
            "pkg/service.py",
            "from .agree import agreement\nfrom .types import Report\n\n\ndef build(night, values):\n    return Report(\n        agreement=agreement(\n            night=night,\n            values=values,\n        ),\n    )\n",
        ),
    ]);
    let call = fx.at("pkg/service.py", "agreement", 7);
    assert_eq!(call.relation, Relation::Calls);
    assert_eq!(fx.lands(call), "pkg/agree.py::agreement");
    assert_eq!(evidence(call).0, EvidenceKind::ImportedName);
    let made = fx.at("pkg/service.py", "Report", 6);
    assert_eq!(made.relation, Relation::Instantiates);
    assert_eq!(fx.lands(made), "pkg/types.py::Report");
    let same_named: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "pkg/service.py" && b.name.as_str() == "agreement")
        .filter(|b| b.relation != Relation::Imports)
        .collect();
    assert_eq!(same_named.len(), 1, "{same_named:#?}");
}

/// gh#677: a call through the module is the module's function, whatever the
/// calling function is named.
#[test]
fn a_module_qualified_call_inside_a_same_named_function_is_the_modules() {
    let fx = snapshot(&[
        ("pkg/__init__.py", ""),
        (
            "pkg/store.py",
            "def reviews(conn):\n    return conn.fetch()\n",
        ),
        (
            "pkg/api.py",
            "from pkg import store\n\n\ndef reviews(conn):\n    return {\"reviews\": store.reviews(conn)}\n\n\ndef today(conn):\n    return store.reviews(conn)[0]\n",
        ),
    ]);
    let wrapped = fx.at("pkg/api.py", "reviews", 5);
    assert_eq!(fx.lands(wrapped), "pkg/store.py::reviews");
    assert_eq!(evidence(wrapped).0, EvidenceKind::ModuleAttribute);
    assert_eq!(
        fx.lands_at("pkg/api.py", "reviews", 9),
        "pkg/store.py::reviews"
    );
    let api = fx
        .head
        .definitions()
        .iter()
        .find(|d| d.file.as_str() == "pkg/api.py" && d.key.local.name.as_str() == "reviews")
        .expect("api.reviews");
    assert!(
        fx.head.references_to(&api.key).is_empty(),
        "nothing calls api.reviews"
    );
}

/// gh#696: a model's fields are members of its class, not unresolved callees;
/// what the class derives from outside the repository is not an unknown.
#[test]
fn model_fields_are_members_of_their_class() {
    let fx = snapshot(&[(
        "app.py",
        "from pydantic import BaseModel\n\n\nclass Order(BaseModel):\n    url: str\n    title: str\n    channel: str = \"\"\n\n\nclass Reply(BaseModel):\n    url: str\n    title: str\n    verdict: str\n\n\ndef handle(order: Order) -> Reply:\n    return Reply(url=order.url, title=order.title, verdict=\"kept\")\n",
    )]);
    assert_eq!(fx.lands_at("app.py", "url", 17), "app.py::Order.url");
    assert_eq!(fx.lands_at("app.py", "title", 17), "app.py::Order.title");
    let made = fx.at("app.py", "Reply", 17);
    assert_eq!(made.relation, Relation::Instantiates);
    assert_eq!(fx.lands(made), "app.py::Reply");
    let unknown: Vec<String> = fx
        .head
        .bindings()
        .iter()
        .filter(
            |b| matches!(&b.resolution, Resolution::Unresolved { reason } if reason.is_unknown()),
        )
        .map(|b| format!("{} line {}: {}", b.name, b.site.lines.0, fx.lands(b)))
        .collect();
    assert!(unknown.is_empty(), "{unknown:#?}");
}

/// fj#106, in Python: a dynamic import by a literal lands on the module, relative
/// to the package of the importing file.
#[test]
fn a_literal_dynamic_import_lands_on_its_module() {
    let fx = snapshot(&[
        ("pkg/__init__.py", ""),
        ("pkg/card.py", "def render():\n    return \"old\"\n"),
        (
            "pkg/app.py",
            "import importlib\n\n\ndef card():\n    return importlib.import_module(\".card\", __package__)\n",
        ),
    ]);
    let dynamic: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "pkg/app.py" && b.relation == Relation::Imports)
        .collect();
    let landed: Vec<(String, EvidenceKind)> = dynamic
        .iter()
        .filter(|b| matches!(b.resolution, Resolution::Module { .. }))
        .map(|b| (fx.lands(b), evidence(b).0))
        .collect();
    assert!(
        landed.contains(&(
            "module pkg/card.py".to_string(),
            EvidenceKind::DynamicImport
        )),
        "{dynamic:#?}"
    );
}

/// A project reaches another through a declared dependency and no other way:
/// the same import lands in one project and nowhere in the other.
#[test]
fn imports_cross_a_project_root_only_through_a_declared_dependency() {
    let fx = snapshot(&[
        (
            "lib/pyproject.toml",
            "[project]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        ),
        ("lib/src/lib/__init__.py", ""),
        ("lib/src/lib/tools.py", "def helper():\n    return 1\n"),
        (
            "user/pyproject.toml",
            "[project]\nname = \"user\"\nversion = \"0.1.0\"\ndependencies = [\"lib\"]\n\n[tool.uv.sources]\nlib = { path = \"../lib\" }\n",
        ),
        (
            "user/src/user/main.py",
            "from lib.tools import helper\n\n\ndef run():\n    return helper()\n",
        ),
        (
            "stray/pyproject.toml",
            "[project]\nname = \"stray\"\nversion = \"0.1.0\"\n",
        ),
        (
            "stray/src/stray/main.py",
            "from lib.tools import helper\n\n\ndef run():\n    return helper()\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("user/src/user/main.py", "helper", 5),
        "lib/src/lib/tools.py::helper"
    );
    assert_eq!(
        fx.lands_at("stray/src/stray/main.py", "helper", 5),
        "unresolved(External)"
    );
}

/// A name takes the route of the `__init__` that re-exports it, and the route is
/// the evidence; the `__init__`'s own import is a re-export, not a use.
#[test]
fn a_reexport_is_followed_and_named_in_the_evidence() {
    let fx = snapshot(&[
        ("pkg/__init__.py", "from .core import run\n"),
        ("pkg/core.py", "def run():\n    return 1\n"),
        (
            "app.py",
            "from pkg import run\n\n\ndef go():\n    return run()\n",
        ),
    ]);
    let call = fx.at("app.py", "run", 5);
    assert_eq!(fx.lands(call), "pkg/core.py::run");
    assert_eq!(
        evidence(call),
        (
            EvidenceKind::ImportedName,
            vec!["pkg/__init__.py".to_string()]
        )
    );
    let offered: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "pkg/__init__.py")
        .collect();
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].relation, Relation::Reexports);
    assert_eq!(fx.lands(offered[0]), "pkg/core.py::run");
}

/// `self` finds the class's own member, then its bases'; `super()` starts at the
/// bases; a method that replaces a base's is an override.
#[test]
fn self_super_and_overrides_follow_the_bases() {
    let fx = snapshot(&[(
        "shapes.py",
        "class Base:\n    def run(self):\n        return self.helper()\n\n    def helper(self):\n        return 1\n\n\nclass Child(Base):\n    def run(self):\n        return super().run() + self.helper()\n",
    )]);
    let own = fx.at("shapes.py", "helper", 3);
    assert_eq!(fx.lands(own), "shapes.py::Base.helper");
    assert_eq!(evidence(own).0, EvidenceKind::SelfReceiver);
    let inherited = fx.at("shapes.py", "helper", 11);
    assert_eq!(fx.lands(inherited), "shapes.py::Base.helper");
    assert_eq!(evidence(inherited).0, EvidenceKind::Inherited);
    let up = fx.at("shapes.py", "run", 11);
    assert_eq!(fx.lands(up), "shapes.py::Base.run");
    assert_eq!(evidence(up).0, EvidenceKind::SuperCall);
    let overrides: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.relation == Relation::Overrides)
        .collect();
    assert_eq!(overrides.len(), 1, "{overrides:#?}");
    assert_eq!(fx.lands(overrides[0]), "shapes.py::Base.run");
}

/// Every binding of `name` on `line` of `file`, imports apart, as the lines
/// `lands` writes, sorted.
fn all_at(fx: &Fixture, file: &str, name: &str, line: u32) -> Vec<String> {
    let mut lands: Vec<String> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| {
            b.file.as_str() == file
                && b.name.as_str() == name
                && b.site.lines.0 == line
                && !matches!(b.relation, Relation::Imports | Relation::Reexports)
        })
        .map(|b| fx.lands(b))
        .collect();
    lands.sort();
    lands
}

/// A base class that reads `self.x` reads, in a subclass that defines `x`, the
/// subclass's: found on the class itself it is resolved, and any subclass that
/// replaces it is a possible reader's target. The verdict of a changed subclass
/// attribute then has a reader and is not claimed unused (rich `956bfa5f`: the
/// `highlights` of `ReprHighlighter`, read by `RegexHighlighter.highlight`).
#[test]
fn a_base_class_read_through_self_may_reach_every_subclass_definition() {
    let fx = snapshot(&[(
        "hl.py",
        "class Base:\n    highlights = []\n\n    def run(self):\n        for h in self.highlights:\n            print(h)\n        return self.render()\n\n    def render(self):\n        return 1\n\n\nclass Child(Base):\n    highlights = [\"a\"]\n\n    def render(self):\n        return 2\n\n\nclass Grandchild(Child):\n    highlights = [\"b\"]\n\n\nclass Other:\n    highlights = [\"c\"]\n",
    )]);
    // Never resolved to a subclass, and never to a class that is no subclass.
    assert_eq!(
        all_at(&fx, "hl.py", "highlights", 5),
        [
            "candidate(Dispatch) hl.py::Child.highlights, hl.py::Grandchild.highlights",
            "hl.py::Base.highlights",
        ]
    );
    assert_eq!(
        all_at(&fx, "hl.py", "render", 7),
        [
            "candidate(Dispatch) hl.py::Child.render",
            "hl.py::Base.render"
        ]
    );
    let candidates: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| {
            matches!(
                b.resolution,
                Resolution::Candidate {
                    reason: CandidateReason::Dispatch,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(candidates.len(), 2, "{candidates:#?}");
}

/// `cls.x` in a class method is the same read, and a write through `self` is
/// no read of any subclass.
#[test]
fn a_classmethod_read_and_a_write_through_the_receiver() {
    let fx = snapshot(&[(
        "k.py",
        "class Maker:\n    kind = \"x\"\n\n    @classmethod\n    def make(cls):\n        return cls.kind\n\n    def reset(self):\n        self.kind = \"z\"\n\n\nclass SubMaker(Maker):\n    kind = \"y\"\n",
    )]);
    assert_eq!(
        all_at(&fx, "k.py", "kind", 6),
        [
            "candidate(Dispatch) k.py::SubMaker.kind",
            "k.py::Maker.kind"
        ]
    );
    let writes: Vec<String> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.relation == Relation::Writes)
        .map(|b| fx.lands(b))
        .collect();
    assert!(writes.iter().all(|w| !w.contains("Dispatch")), "{writes:?}");
}

/// A call through an untyped receiver is a candidate within a package and
/// nothing between loose scripts; a parameter that shadows a function is no use
/// of it; a script's guard is flagged.
#[test]
fn untyped_calls_candidates_locals_and_script_guards() {
    let fx = snapshot(&[
        ("shop/__init__.py", ""),
        (
            "shop/pricing.py",
            "class Cart:\n    def total(self):\n        return 0\n",
        ),
        (
            "shop/checkout.py",
            "def charge(cart):\n    return cart.total()\n",
        ),
        (
            "tools/device.py",
            "class Device:\n    def call(self):\n        return 0\n",
        ),
        (
            "tools/migrate.py",
            "def fixture(client):\n    return client.call()\n\n\ndef prompt(text):\n    return text\n\n\ndef ask(prompt):\n    return prompt(1)\n\n\nif __name__ == \"__main__\":\n    fixture(None)\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("shop/checkout.py", "total", 2),
        "candidate(UntypedReceiver) shop/pricing.py::Cart.total"
    );
    assert_eq!(
        fx.lands_at("tools/migrate.py", "call", 2),
        "unresolved(NoCandidates)"
    );
    assert_eq!(
        fx.lands_at("tools/migrate.py", "prompt", 10),
        "unresolved(Local)"
    );
    let guarded = fx.at("tools/migrate.py", "fixture", 14);
    assert!(guarded.entry_guard);
    assert_eq!(fx.lands(guarded), "tools/migrate.py::fixture");
    assert!(!fx.at("tools/migrate.py", "call", 2).entry_guard);
}

/// A definition under `if TYPE_CHECKING:` is a twin of the real one; the name is
/// the real one, and a star import offers the module's public names.
#[test]
fn twins_stand_behind_the_primary_and_star_imports_offer_public_names() {
    let fx = snapshot(&[
        (
            "twin.py",
            "from typing import TYPE_CHECKING\n\nif TYPE_CHECKING:\n    def convert(x: int) -> int: ...\n\n\ndef convert(x):\n    return x\n\n\ndef use():\n    return convert(1)\n",
        ),
        (
            "wide.py",
            "def shown():\n    return 1\n\n\ndef _hidden():\n    return 2\n",
        ),
        (
            "user.py",
            "from wide import *\n\n\ndef go():\n    return shown() + _hidden()\n",
        ),
    ]);
    let call = fx.at("twin.py", "convert", 12);
    match &call.resolution {
        Resolution::Resolved { target, .. } => assert_eq!(target.local.disambig, None),
        other => panic!("{other:?}"),
    }
    let shown = fx.at("user.py", "shown", 5);
    assert_eq!(fx.lands(shown), "wide.py::shown");
    assert_eq!(evidence(shown).0, EvidenceKind::StarImport);
    assert_eq!(
        fx.lands_at("user.py", "_hidden", 5),
        "unresolved(NoBinding)"
    );
}

/// `with Cls(...) as v` binds `v` to an instance of the class, also when the
/// arguments span lines; a class of a package outside the repository is not a
/// class of it, and its `v` is nobody's receiver.
#[test]
fn a_with_target_is_an_instance_of_the_call() {
    let fx = snapshot(&[
        ("server/src/srv/__init__.py", ""),
        (
            "server/src/srv/app.py",
            "class Server:\n    def call_tool(self, name):\n        return name\n",
        ),
        (
            "server/tests/test_ctx.py",
            "from srv.app import Server\n\n\ndef test_in_context():\n    with Server() as srv:\n        result = srv.call_tool(\"echo\")\n    assert result\n\n\ndef test_wrapped():\n    with Server(\n        1,\n    ) as srv:\n        return srv.call_tool(\"echo\")\n",
        ),
        (
            "client/run.py",
            "from mcp import ClientSession\n\n\nasync def ask(read, write):\n    async with ClientSession(read, write) as session:\n        return await session.call_tool(\"x\")\n",
        ),
    ]);
    for line in [6, 14] {
        assert_eq!(
            fx.lands_at("server/tests/test_ctx.py", "call_tool", line),
            "server/src/srv/app.py::Server.call_tool"
        );
    }
    assert_eq!(
        fx.lands_at("client/run.py", "call_tool", 6),
        "unresolved(External)"
    );
}

/// A `tests/` or `test/` directory is run from its parent, and from the parent's
/// `src/` layout; a module of another directory of the same name is not reached.
#[test]
fn a_test_suite_imports_from_the_directory_above_it() {
    let fx = snapshot(&[
        ("tools/metrics.py", "def render():\n    return 1\n"),
        (
            "tools/tests/test_tools.py",
            "import metrics\n\n\ndef test_tools():\n    assert metrics.render()\n",
        ),
        ("other/metrics.py", "def render():\n    return 2\n"),
        ("svc/src/engine.py", "def start():\n    return 1\n"),
        (
            "svc/test/test_engine.py",
            "import engine\n\n\ndef test_start():\n    assert engine.start()\n",
        ),
        (
            "elsewhere/test_loose.py",
            "import metrics\n\n\ndef test_loose():\n    assert metrics.render()\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("tools/tests/test_tools.py", "render", 5),
        "tools/metrics.py::render"
    );
    assert_eq!(
        fx.lands_at("svc/test/test_engine.py", "start", 5),
        "svc/src/engine.py::start"
    );
    assert_eq!(
        fx.lands_at("elsewhere/test_loose.py", "render", 5),
        "unresolved(External)"
    );
}

/// A `conftest.py` puts its directories on `sys.path` for every test in its
/// directory and below, and for no test elsewhere.
#[test]
fn a_conftest_sys_path_applies_to_the_tests_below_it() {
    let fx = snapshot(&[
        (
            "child-a/transcribe/transcribe.py",
            "def recognise_track(path):\n    return path\n",
        ),
        ("child-b/src/pkg/__init__.py", ""),
        (
            "child-b/src/pkg/transcribe.py",
            "def recognise_track(path):\n    return None\n",
        ),
        (
            "child-a/tests/conftest.py",
            "import sys\nfrom pathlib import Path\n\nROOT = Path(__file__).resolve().parents[1]\nif str(ROOT / \"transcribe\") not in sys.path:\n    sys.path.insert(0, str(ROOT / \"transcribe\"))\n",
        ),
        (
            "child-a/tests/unit/test_languages.py",
            "import transcribe\n\n\ndef test_monologue():\n    assert transcribe.recognise_track(\"a\")\n",
        ),
        (
            "child-b/tests/test_other.py",
            "import transcribe\n\n\ndef test_other():\n    assert transcribe.recognise_track(\"a\")\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("child-a/tests/unit/test_languages.py", "recognise_track", 5),
        "child-a/transcribe/transcribe.py::recognise_track"
    );
    assert_eq!(
        fx.lands_at("child-b/tests/test_other.py", "recognise_track", 5),
        "unresolved(External)"
    );
}

/// A module a test imports first may put a directory on `sys.path` for the
/// imports after it (the `tests/rig.py` pattern); a test that does not import
/// the rig does not get the directory.
#[test]
fn a_rig_a_test_imports_puts_its_directory_on_the_path() {
    let fx = snapshot(&[
        ("transcribe/jobs.py", "def run(x):\n    return x\n"),
        (
            "tests/rig.py",
            "import sys\nfrom pathlib import Path\n\nREPO = Path(__file__).resolve().parent.parent\nsys.path.insert(0, str(REPO / \"transcribe\"))\n",
        ),
        (
            "tests/test_crash.py",
            "import rig\nimport jobs\n\n\ndef test_run():\n    assert jobs.run(1) == 1\n",
        ),
        (
            "tests/test_plain.py",
            "import jobs\n\n\ndef test_plain():\n    assert jobs.run(1) == 1\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("tests/test_crash.py", "run", 6),
        "transcribe/jobs.py::run"
    );
    assert_eq!(
        fx.lands_at("tests/test_plain.py", "run", 5),
        "unresolved(External)"
    );
}

/// `pythonpath` of `[tool:pytest]` in `setup.cfg` (and `[pytest]` in
/// `pytest.ini`) names import roots of the directory holding the file.
#[test]
fn pytest_configuration_files_name_import_roots() {
    let fx = snapshot(&[
        (
            "job/setup.cfg",
            "[metadata]\nname = job\n\n[tool:pytest]\npythonpath =\n    lib\n    .\n",
        ),
        ("job/lib/report.py", "def render_report():\n    return 1\n"),
        ("other/report.py", "def render_report():\n    return 2\n"),
        (
            "job/checks/test_report.py",
            "import report\n\n\ndef test_report():\n    assert report.render_report()\n",
        ),
        ("p/pytest.ini", "[pytest]\npythonpath = lib\n"),
        ("p/lib/shapes.py", "def area():\n    return 1\n"),
        (
            "p/checks/test_shapes.py",
            "import shapes\n\n\ndef test_area():\n    assert shapes.area()\n",
        ),
    ]);
    assert_eq!(
        fx.lands_at("job/checks/test_report.py", "render_report", 5),
        "job/lib/report.py::render_report"
    );
    assert_eq!(
        fx.lands_at("p/checks/test_shapes.py", "area", 5),
        "p/lib/shapes.py::area"
    );
}

/// A function with one `return Cls(...)` and nothing else to return makes an
/// instance of `Cls`, a proof (gh#717); one that returns two different
/// constructions makes one of them, a possibility.
#[test]
fn a_sole_construction_returns_its_instance_and_a_choice_a_possible_one() {
    let fx = snapshot(&[
        ("app/__init__.py", ""),
        (
            "app/client.py",
            "class Client:\n    def token(self):\n        return \"t\"\n",
        ),
        (
            "app/other.py",
            "class Other:\n    def token(self):\n        return \"o\"\n",
        ),
        (
            "tests/helpers.py",
            "from app.client import Client\nfrom app.other import Other\n\n\ndef make():\n    return Client()\n\n\ndef either(flag):\n    if flag:\n        return Client()\n    return Other()\n",
        ),
        ("tests/__init__.py", ""),
        (
            "tests/test_api.py",
            "from tests import helpers\nfrom tests.helpers import either, make\nfrom app.client import Client\nfrom app.other import Other\n\n\ndef test_token():\n    client = make()\n    assert client.token()\n\n\ndef test_qualified():\n    c = helpers.make()\n    assert c.token()\n\n\ndef test_either():\n    e = either(1)\n    assert e.token()\n\n\ndef test_other():\n    o = Other()\n    assert o.token()\n",
        ),
    ]);
    let instance = "app/client.py::Client.token";
    assert_eq!(fx.lands_at("tests/test_api.py", "token", 9), instance);
    assert_eq!(fx.lands_at("tests/test_api.py", "token", 14), instance);
    assert_eq!(
        fx.lands_at("tests/test_api.py", "token", 19),
        "candidate(UntypedReceiver) app/client.py::Client.token, app/other.py::Other.token"
    );
    assert_eq!(
        fx.lands_at("tests/test_api.py", "token", 24),
        "app/other.py::Other.token"
    );
}

/// A requirement by name in a uv workspace member links to nothing without a
/// `[tool.uv.sources]` entry: its imports of the workspace's package are possible
/// only. A project that is no uv project keeps the link by name.
#[test]
fn a_uv_requirement_without_a_source_is_an_ambiguous_module() {
    let consumer = |name: &str, tail: &str| {
        format!(
            "[project]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [\"pkg\"]\n{tail}"
        )
    };
    let user = "from pkg import client_ip\n\n\ndef f():\n    return client_ip()\n";
    let fx = snapshot(&[
        (
            "pyproject.toml",
            "[tool.uv.workspace]\nmembers = [\"apps/*\", \"shared/*\"]\n",
        ),
        (
            "shared/pkg/pyproject.toml",
            "[project]\nname = \"pkg\"\nversion = \"0.1.0\"\n",
        ),
        (
            "shared/pkg/src/pkg/__init__.py",
            "from pkg.ip import client_ip\n",
        ),
        (
            "shared/pkg/src/pkg/ip.py",
            "def client_ip():\n    return 1\n",
        ),
        (
            "apps/svc/pyproject.toml",
            &consumer(
                "svc",
                "\n[tool.uv.sources]\npkg = { path = \"../../shared/pkg\" }\n",
            ),
        ),
        ("apps/svc/src/svc/api.py", user),
        ("apps/loose/pyproject.toml", &consumer("loose", "")),
        ("apps/loose/src/loose/log.py", user),
        ("legacy/pyproject.toml", &consumer("legacy", "")),
        ("legacy/src/legacy/log.py", user),
    ]);
    let ip = "shared/pkg/src/pkg/ip.py::client_ip";
    assert_eq!(fx.lands_at("apps/svc/src/svc/api.py", "client_ip", 5), ip);
    assert_eq!(
        fx.lands_at("apps/loose/src/loose/log.py", "client_ip", 5),
        format!("candidate(AmbiguousModule) {ip}")
    );
    assert_eq!(fx.lands_at("legacy/src/legacy/log.py", "client_ip", 5), ip);
}

/// `X | None`, `Optional[X]` and `Union[X, None]` state the type of the receiver;
/// a choice between two classes states none.
#[test]
fn an_optional_annotation_types_the_receiver() {
    let fx = snapshot(&[(
        "cap.py",
        "from typing import Optional, Union\n\n\nclass Capture:\n    def start(self):\n        return 1\n\n\ndef piped(stream: Capture | None):\n    stream.start()\n\n\ndef old_style(stream: Optional[Capture]):\n    stream.start()\n\n\ndef union(stream: Union[Capture, None]):\n    stream.start()\n\n\ndef assigned():\n    held: Capture | None = None\n    held.start()\n\n\ndef either(stream: Capture | int):\n    stream.start()\n",
    )]);
    let start = "cap.py::Capture.start";
    for line in [10, 14, 18, 23] {
        assert_eq!(fx.lands_at("cap.py", "start", line), start, "line {line}");
    }
    assert_eq!(
        fx.lands_at("cap.py", "start", 27),
        format!("candidate(UntypedReceiver) {start}")
    );
}

/// A snapshot with no manifest is one project rooted at the repository, and a
/// `src/` directory without an `__init__.py` is its layout: `shop.timing` is
/// `src/shop/timing.py`, from a module beside it and from a test below `tests/`.
#[test]
fn a_src_layout_without_a_manifest_is_an_import_root() {
    let fx = snapshot(&[
        (
            "src/shop/timing.py",
            "def elapsed(start):\n    return start\n",
        ),
        (
            "src/shop/report.py",
            "from shop.timing import elapsed\n\n\ndef is_slow(start):\n    return elapsed(start) > 3\n",
        ),
        (
            "tests/test_timing.py",
            "from shop import timing\n\n\ndef test_elapsed():\n    assert timing.elapsed(1)\n",
        ),
    ]);
    let elapsed = "src/shop/timing.py::elapsed";
    assert_eq!(fx.lands_at("src/shop/report.py", "elapsed", 5), elapsed);
    assert_eq!(fx.lands_at("tests/test_timing.py", "elapsed", 5), elapsed);
}

/// A parameter annotated with an alias that is then rebound to a construction
/// is read under the constructed class, also after `x = x.method()`.
#[test]
fn a_rebound_parameter_is_read_under_the_class_it_is_rebound_to() {
    let fx = snapshot(&[(
        "alias.py",
        "URLTypes = str\n\n\nclass URL:\n    def __init__(self, text):\n        self.text = text\n\n    def copy_with(self, path) -> \"URL\":\n        return URL(path)\n\n    @property\n    def username(self):\n        return \"\"\n\n\ndef proxy(url: URLTypes):\n    url = URL(url)\n    if url.username:\n        url = url.copy_with(path=\"x\")\n    return url.username\n",
    )]);
    assert_eq!(
        fx.lands_at("alias.py", "username", 18),
        "alias.py::URL.username"
    );
    assert_eq!(
        fx.lands_at("alias.py", "copy_with", 19),
        "alias.py::URL.copy_with"
    );
    assert_eq!(
        fx.lands_at("alias.py", "username", 20),
        "alias.py::URL.username"
    );
}

/// The members a chain passes through are read: `self.decoder` in
/// `self.decoder.decode()`; a function whose attribute is used (`cli.command`)
/// is read as well.
#[test]
fn the_members_in_the_middle_of_a_chain_and_a_function_head_are_read() {
    let fx = snapshot(&[
        (
            "chain.py",
            "class Decoder:\n    def decode(self, chunk):\n        return chunk\n\n\nclass Response:\n    @property\n    def decoder(self) -> Decoder:\n        return Decoder()\n\n    def iter_bytes(self, chunk):\n        yield self.decoder.decode(chunk)\n",
        ),
        (
            "fn.py",
            "def cli(f):\n    return f\n\n\n@cli.command(\"strip\")\ndef make_strip():\n    return 1\n",
        ),
    ]);
    let decode = fx.at("chain.py", "decode", 12);
    assert_eq!(fx.lands(decode), "chain.py::Decoder.decode");
    let property = fx.at("chain.py", "decoder", 12);
    assert_eq!(property.relation, Relation::Reads);
    assert_eq!(fx.lands(property), "chain.py::Response.decoder");
    let head = fx.at("fn.py", "cli", 5);
    assert_eq!(head.relation, Relation::Reads);
    assert_eq!(fx.lands(head), "fn.py::cli");
}

#[test]
fn candidate_reasons_and_unresolved_reasons_are_told_apart() {
    assert!(UnresolvedReason::UntypedRead.is_unknown());
    assert!(UnresolvedReason::NoBinding.is_unknown());
    assert!(!UnresolvedReason::External.is_unknown());
    assert!(!UnresolvedReason::Local.is_unknown());
    assert!(!UnresolvedReason::ExternalBase.is_unknown());
    assert_ne!(
        CandidateReason::AnyReceiver,
        CandidateReason::UntypedReceiver
    );
}

/// W5-R1 (R3): a variable assigned an attribute chain holds what the chain
/// holds, so a read through it is a read of the member of that type, resolved
/// as aliased; the alias of an untyped name stays an untyped read, and a module
/// variable assigned a chain is an alias too.
#[test]
fn a_variable_assigned_an_attribute_chain_reads_through_the_type_of_that_chain() {
    let fx = snapshot(&[
        (
            "pkg/opts.py",
            "class Internals:\n    dns_lookup = None\n\n\nclass Options:\n    internals: Internals = Internals()\n",
        ),
        (
            "pkg/native.py",
            "from pkg.opts import Options\n\n\ndef create(options: Options):\n    internals = options.internals\n    return internals.dns_lookup\n\n\ndef loose(other):\n    internals = other.internals\n    return internals.dns_lookup\n",
        ),
        (
            "pkg/shared.py",
            "from pkg.opts import Options\n\nopts = Options()\nshared = opts.internals\n\n\ndef read():\n    return shared.dns_lookup\n",
        ),
    ]);
    let typed = fx.at("pkg/native.py", "dns_lookup", 6);
    assert_eq!(fx.lands(typed), "pkg/opts.py::Internals.dns_lookup");
    assert_eq!(typed.relation, Relation::Reads);
    assert_eq!(evidence(typed).0, EvidenceKind::Aliased);
    assert_eq!(
        fx.lands_at("pkg/native.py", "dns_lookup", 11),
        "unresolved(UntypedRead)"
    );
    assert_eq!(
        fx.lands_at("pkg/shared.py", "dns_lookup", 8),
        "pkg/opts.py::Internals.dns_lookup"
    );
}

/// W5-R1 (R4): a function handed to `functools.partial` is passed as a value,
/// resolved, from the function that builds the partial; the same name of
/// another module is not it.
#[test]
fn a_function_handed_to_functools_partial_is_passed_as_a_value() {
    let fx = snapshot(&[
        (
            "pkg/hooks.py",
            "def print_headers(request, http2=False):\n    return request\n",
        ),
        (
            "pkg/main.py",
            "import functools\n\nfrom pkg.hooks import print_headers\n\n\ndef main(http2):\n    hooks = {}\n    hooks[\"request\"] = [functools.partial(print_headers, http2=http2)]\n    return hooks\n",
        ),
        (
            "pkg/cli.py",
            "import functools\n\n\ndef print_local(request):\n    return request\n\n\ndef run(items):\n    return list(map(functools.partial(print_local), items))\n",
        ),
    ]);
    let imported = fx.at("pkg/main.py", "print_headers", 8);
    assert_eq!(imported.relation, Relation::PassesAsValue);
    assert_eq!(fx.lands(imported), "pkg/hooks.py::print_headers");
    let local = fx.at("pkg/cli.py", "print_local", 9);
    assert_eq!(local.relation, Relation::PassesAsValue);
    assert_eq!(fx.lands(local), "pkg/cli.py::print_local");
}

/// W5-R1 (R6): `from .decorators import command as command` in a package's
/// `__init__` is a re-export of the definition, resolved through the overloads
/// to the one name; it is no import and no caller.
#[test]
fn a_package_init_reexports_an_overloaded_function() {
    let fx = snapshot(&[
        (
            "pkg/__init__.py",
            "from .decorators import command as command\n",
        ),
        (
            "pkg/decorators.py",
            "from typing import overload\n\n\n@overload\ndef command(name: None) -> int: ...\n\n\n@overload\ndef command(name: str) -> str: ...\n\n\ndef command(name=None):\n    return name\n",
        ),
    ]);
    let offered: Vec<&Binding> = fx
        .head
        .bindings()
        .iter()
        .filter(|b| b.file.as_str() == "pkg/__init__.py" && b.relation == Relation::Reexports)
        .collect();
    assert_eq!(offered.len(), 1, "{offered:#?}");
    assert_eq!(fx.lands(offered[0]), "pkg/decorators.py::command");
    assert!(!offered[0].relation.uses());
}
