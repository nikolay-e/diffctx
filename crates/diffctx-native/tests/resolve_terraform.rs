//! The Terraform resolver on real git repositories: every Terraform case of the
//! impact corpus read by the callers it expects, and the shapes of a module's
//! addresses. The expectations come from the case files and from reading each
//! fixture; nothing here runs diffctx or terraform to learn an answer.
//!
//! The corpus judge pairs the resolver's relations to a case's target with the
//! case's own, by resolution and, where the case states it, the relation kind,
//! and compares only what the resolver answers: `tests_reach`, `references_path`
//! and the like belong to the layers above it. A caller that is itself a changed
//! definition may be listed or left out.

mod snapshot_fixture;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name, Scope, semantic_changes};
use _diffctx::resolve::{Binding, Relation, Resolution, TerraformResolver};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag, working_tree};
use serde::Deserialize;
use snapshot_fixture::Repo;
use walkdir::WalkDir;

// ---- the fixture

struct Fixture {
    _repo: Repo,
    ctx: AnalysisContext,
    head: TerraformResolver,
    base: TerraformResolver,
}

fn open(repo: Repo, range: RangeSpec) -> Fixture {
    let ctx = AnalysisContext::new(repo.root.clone(), range, RunBudget::default())
        .expect("an analysis context");
    let cache = FactsCache::disabled();
    let head = TerraformResolver::open(&ctx, SideTag::Head, &cache).expect("the head resolves");
    let base = TerraformResolver::open(&ctx, SideTag::Base, &cache).expect("the base resolves");
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
) -> Vec<(&'f TerraformResolver, DefKey)> {
    let named = |resolver: &'f TerraformResolver| -> Vec<DefKey> {
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
    let mut found: Vec<(&TerraformResolver, DefKey)> =
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

/// The Terraform cases the resolver cannot answer yet, each with the reason. The list
/// is held both ways: a listed case that turns green fails the run, as does a
/// red case that is not listed.
const RED: &[(&str, &str)] = &[];

#[test]
fn the_terraform_corpus_is_answered_as_the_list_says() {
    let root = cases_root();
    let mut cases: Vec<(String, PathBuf)> = Vec::new();
    for mode in ["impact"] {
        let dir = root.join(mode).join("terraform");
        for entry in WalkDir::new(&dir).sort_by_file_name() {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "yaml") {
                let stem = path.file_stem().expect("a stem").to_string_lossy();
                cases.push((format!("{mode}/terraform/{stem}"), path.to_path_buf()));
            }
        }
    }
    assert!(cases.len() >= 11, "the walk found {} cases", cases.len());
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
        "terraform corpus: {} cases, {green} green, {} red as listed, {idle} assert nothing the \
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

#[test]
fn the_root_of_a_traversal_says_what_it_names() {
    let fx = snapshot(&[(
        "main.tf",
        "variable \"name\" {}\n\nlocals {\n  name = \"l\"\n}\n\nresource \"terraform_data\" \"name\" {\n  input = \"r\"\n}\n\ndata \"terraform_remote_state\" \"name\" {\n  backend = \"local\"\n}\n\noutput \"all\" {\n  value = [var.name, local.name, terraform_data.name.id, data.terraform_remote_state.name.outputs]\n}\n",
    )]);
    let site = 16;
    let read = |name: &str, nth: usize| {
        let found: Vec<&Binding> = fx
            .head
            .bindings()
            .iter()
            .filter(|b| b.name.as_str() == name && b.site.lines.0 == site)
            .collect();
        fx.lands(found[nth])
    };
    assert_eq!(read("name", 0), "main.tf::var.name");
    assert_eq!(read("name", 1), "main.tf::local.name");
    assert_eq!(read("name", 2), "main.tf::terraform_data.name");
    assert_eq!(read("name", 3), "main.tf::data.terraform_remote_state.name");
}

#[test]
fn a_module_is_a_directory_and_the_same_name_in_another_is_another_definition() {
    let fx = snapshot(&[
        (
            "main.tf",
            "variable \"region\" {}\n\noutput \"r\" {\n  value = var.region\n}\n",
        ),
        (
            "modules/net/main.tf",
            "variable \"region\" {}\n\noutput \"r\" {\n  value = var.region\n}\n",
        ),
        (
            "modules/net/more.tf",
            "output \"again\" {\n  value = var.region\n}\n",
        ),
        ("lone.tf", "output \"lost\" {\n  value = var.region\n}\n"),
    ]);
    assert_eq!(fx.lands_at("main.tf", "region", 4), "main.tf::var.region");
    assert_eq!(
        fx.lands_at("modules/net/main.tf", "region", 4),
        "modules/net/main.tf::var.region"
    );
    // A file of the same directory, though not the same file.
    assert_eq!(
        fx.lands_at("modules/net/more.tf", "region", 2),
        "modules/net/main.tf::var.region"
    );
    // `lone.tf` is in the root's directory: it reaches the root's variable.
    assert_eq!(fx.lands_at("lone.tf", "region", 2), "main.tf::var.region");
}

const CALLER: &str = "module \"net\" {\n  source = \"./modules/net\"\n  cidr   = \"10.0.0.0/16\"\n  gone   = 1\n}\n\nmodule \"reg\" {\n  source = \"hashicorp/consul/aws\"\n}\n\nmodule \"missing\" {\n  source = \"./modules/none\"\n}\n\nmodule \"dyn\" {\n  source = \"${path.module}/x\"\n}\n\noutput \"o\" {\n  value = [module.net.vpc, module.net.undeclared, module.reg.addr, module.missing.x, module.dyn.y, module.absent.z, module.net]\n}\n";
const NET: &str = "variable \"cidr\" {}\n\noutput \"vpc\" {\n  value = var.cidr\n}\n";

#[test]
fn a_module_call_reaches_its_directory_and_a_source_that_is_no_path_reaches_nothing() {
    let fx = snapshot(&[("main.tf", CALLER), ("modules/net/main.tf", NET)]);
    let value = 20;
    assert_eq!(
        fx.lands_at("main.tf", "vpc", value),
        "modules/net/main.tf::output.vpc"
    );
    assert_eq!(
        fx.lands_at("main.tf", "undeclared", value),
        "unresolved(NoMember)"
    );
    assert_eq!(
        fx.lands_at("main.tf", "addr", value),
        "unresolved(External)"
    );
    assert_eq!(
        fx.lands_at("main.tf", "x", value),
        "unresolved(MissingModule)"
    );
    assert_eq!(fx.lands_at("main.tf", "y", value), "unresolved(Opaque)");
    assert_eq!(fx.lands_at("main.tf", "z", value), "unresolved(NoBinding)");
    // The call itself, read whole.
    assert_eq!(fx.lands_at("main.tf", "net", value), "main.tf::module.net");
}

#[test]
fn an_argument_of_a_call_is_a_write_of_the_variable_of_the_called_module() {
    let fx = snapshot(&[("main.tf", CALLER), ("modules/net/main.tf", NET)]);
    let written = fx.at("main.tf", "cidr", 3);
    assert_eq!(written.relation, Relation::Writes);
    assert_eq!(fx.lands(written), "modules/net/main.tf::var.cidr");
    assert_eq!(fx.lands_at("main.tf", "gone", 4), "unresolved(NoMember)");
}

#[test]
fn a_variable_file_writes_the_variable_of_its_directory() {
    let fx = snapshot(&[
        ("main.tf", "variable \"region\" {}\n"),
        ("terraform.tfvars", "region = \"eu\"\nmissing = 1\n"),
        ("modules/net/vars.tfvars", "region = \"us\"\n"),
    ]);
    let written = fx.at("terraform.tfvars", "region", 1);
    assert_eq!(written.relation, Relation::Writes);
    assert_eq!(fx.lands(written), "main.tf::var.region");
    assert_eq!(
        fx.lands_at("terraform.tfvars", "missing", 2),
        "unresolved(NoBinding)"
    );
    assert_eq!(
        fx.lands_at("modules/net/vars.tfvars", "region", 1),
        "unresolved(NoBinding)"
    );
}

#[test]
fn the_sources_outside_the_repository_are_counted() {
    let fx = snapshot(&[("main.tf", CALLER), ("modules/net/main.tf", NET)]);
    // The registry address only: a local path that is missing, a computed one
    // and a local one are not outside.
    assert_eq!(fx.head.external_sources(), 1);
}
