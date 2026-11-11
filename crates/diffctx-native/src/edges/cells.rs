//! The ranking graph's edges for the languages that have a cell: read off the
//! cell's bindings, not off the text of the fragments. One builder per family;
//! it opens the family's head snapshot (`decide::open_head`, once per run), maps
//! each binding to the fragment that owns its site and the fragment it lands on,
//! and weights it with the table the language's old builder read, times the
//! coefficients of `CELL_EDGES`. A binding that lands on nothing, on a file with
//! no fragment, or (unless a switch of `CELL_EDGES` says otherwise) on several
//! definitions or on none draws no edge: an edge is never invented.
//!
//! The old text builders stay selectable: `CELL_EDGES.draws(group)` says which
//! groups of languages take their edges from here (`replaces` names the old
//! builders it silences).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::edge_weights::{
    C_FAMILY_SEMANTIC, CELL_EDGES, CellEdgeWeights, DOCKER, GO_SEMANTIC, HELM, JAVASCRIPT_SEMANTIC,
    KUBERNETES, PYTHON_SEMANTIC, SEMANTIC_DISCOVERY, TERRAFORM_SEMANTIC,
};
use crate::config::weights::{EDGE_WEIGHTS, EdgeWeightConfig, LANG_WEIGHTS};
use crate::decide::{self, Family, HeadFamily};
use crate::facts::{DefKey, Language};
use crate::resolve::{Binding, Relation, Resolution, UnresolvedReason};
use crate::resource::{LimitReason, current_source, note_current};
use crate::snapshot::RepoPath;
use crate::types::{Fragment, FragmentId};

use super::EdgeDict;
use super::base::{self, EdgeBuilder, add_edge};

/// The groups of languages a switch of `CELL_EDGES` moves together, with the
/// families each reads and the old builders (by knife name) it replaces.
static GROUPS: [(&str, &[Family], &[&str]); 12] = [
    ("python", &[Family::Python], &["python"]),
    ("typescript", &[Family::TypeScript], &["javascript"]),
    ("rust", &[Family::Rust], &["rust"]),
    ("jvm", &[Family::Kotlin, Family::Java], &["jvm"]),
    ("go", &[Family::Go], &["go"]),
    ("php", &[Family::Php], &["php"]),
    ("c", &[Family::C], &["cfamily"]),
    ("terraform", &[Family::Terraform], &["terraform"]),
    ("shell", &[Family::Shell], &["shell"]),
    ("swift", &[Family::Swift], &["swift"]),
    ("sql", &[Family::Sql], &["sql"]),
    (
        "config",
        &[Family::Dockerfile, Family::Config],
        &["docker", "helm", "kubernetes"],
    ),
];

/// One builder per family of every group drawn from the cells.
pub fn builders() -> Vec<Box<dyn EdgeBuilder>> {
    GROUPS
        .iter()
        .filter(|(group, _, _)| CELL_EDGES.draws(group))
        .flat_map(|(_, families, _)| families.iter())
        .map(|family| Box::new(CellEdgeBuilder { family: *family }) as Box<dyn EdgeBuilder>)
        .collect()
}

/// An old text builder kept beside the cells of its group: it keeps every edge
/// that has an end in a file no cell of the group covers (a Scala file next to
/// Kotlin, a Vue file next to TypeScript) and every edge when no repository root
/// is known, and drops the edges the cells draw between files they cover. Its
/// discovery starts from the changed files the cells do not cover.
struct Beside {
    old: Box<dyn EdgeBuilder>,
    families: &'static [Family],
}

impl Beside {
    fn covers(&self, root: &Path, path: &str, scripts: &FxHashSet<String>) -> bool {
        let rel = rel_of(root, path);
        self.families.iter().any(|f| concerns(*f, &rel)) || scripts.contains(&rel)
    }
}

/// `builder` as the cells leave it: unchanged when no group drawn from the cells
/// replaces it, else narrowed to the files they do not cover.
pub fn beside(builder: Box<dyn EdgeBuilder>) -> Box<dyn EdgeBuilder> {
    let name = builder.knife_name();
    match GROUPS
        .iter()
        .find(|(group, _, old)| CELL_EDGES.draws(group) && old.contains(&name.as_str()))
    {
        Some((_, families, _)) => Box::new(Beside {
            old: builder,
            families,
        }),
        None => builder,
    }
}

impl EdgeBuilder for Beside {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        self.build_deciding(fragments, repo_root).0
    }

    fn build_deciding(
        &self,
        fragments: &[Fragment],
        repo_root: Option<&Path>,
    ) -> (EdgeDict, Vec<(String, String)>) {
        let (mut edges, decided) = self.old.build_deciding(fragments, repo_root);
        let Some(root) = repo_root else {
            return (edges, decided);
        };
        let scripts: FxHashSet<String> = self
            .families
            .iter()
            .flat_map(|family| CellEdgeBuilder { family: *family }.scripts(root, fragments))
            .collect();
        let mut covered: FxHashMap<Arc<str>, bool> = FxHashMap::default();
        let mut is_covered = |path: &Arc<str>| {
            *covered
                .entry(Arc::clone(path))
                .or_insert_with(|| self.covers(root, path, &scripts))
        };
        edges.retain(|(from, to), _| !(is_covered(&from.path) && is_covered(&to.path)));
        (edges, decided)
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let Some(root) = repo_root else {
            return self
                .old
                .discover_related_files(changed, candidates, repo_root, file_cache);
        };
        let uncovered: Vec<PathBuf> = changed
            .iter()
            .filter(|p| !self.covers(root, &p.to_string_lossy(), &FxHashSet::default()))
            .cloned()
            .collect();
        if uncovered.is_empty() {
            return Vec::new();
        }
        self.old
            .discover_related_files(&uncovered, candidates, repo_root, file_cache)
    }

    fn category_label(&self) -> Option<&str> {
        self.old.category_label()
    }

    fn knife_name(&self) -> String {
        self.old.knife_name()
    }

    fn is_fallback(&self) -> bool {
        self.old.is_fallback()
    }

    fn is_key_table(&self) -> bool {
        self.old.is_key_table()
    }
}

pub struct CellEdgeBuilder {
    pub family: Family,
}

fn pair(w: EdgeWeightConfig) -> (f64, f64) {
    (w.forward, w.reverse_factor)
}

fn is_typescript(rel: &str) -> bool {
    [".ts", ".tsx", ".mts", ".cts"]
        .iter()
        .any(|ext| rel.ends_with(ext))
}

/// The forward weight and reverse factor the old builder of `family` gave an edge
/// of this relation. `module` is a binding that lands on a whole file.
fn weight(family: Family, rel: &str, relation: Relation, module: bool) -> (f64, f64) {
    use Relation::{
        Calls, Imports, Instantiates, Overrides, PassesAsValue, Reads, Reexports, Writes,
    };
    let imports = matches!(relation, Imports | Reexports);
    let uses = matches!(
        relation,
        Calls | Instantiates | Reads | Writes | PassesAsValue
    );
    match family {
        Family::Python => {
            let w = LANG_WEIGHTS.get("python").expect("python weights");
            let forward = if imports {
                PYTHON_SEMANTIC.import_weight
            } else if matches!(relation, Calls | Instantiates) {
                w.call
            } else if uses {
                w.symbol_ref
            } else {
                w.type_ref
            };
            (forward, PYTHON_SEMANTIC.reverse_factor)
        }
        Family::TypeScript => {
            let w = LANG_WEIGHTS
                .get(if is_typescript(rel) {
                    "typescript"
                } else {
                    "javascript"
                })
                .expect("typescript weights");
            let forward = if imports {
                JAVASCRIPT_SEMANTIC.import_weight
            } else if matches!(relation, Calls | Instantiates) {
                w.call
            } else if uses {
                w.symbol_ref
            } else {
                w.type_ref
            };
            (forward, JAVASCRIPT_SEMANTIC.reverse_factor)
        }
        Family::Rust => {
            if module && imports {
                pair(EDGE_WEIGHTS["rust_mod"])
            } else if imports {
                pair(EDGE_WEIGHTS["rust_use"])
            } else if uses {
                pair(EDGE_WEIGHTS["rust_fn"])
            } else {
                pair(EDGE_WEIGHTS["rust_type"])
            }
        }
        Family::Kotlin | Family::Java => {
            if imports {
                pair(EDGE_WEIGHTS["jvm_import"])
            } else if relation == Overrides {
                pair(EDGE_WEIGHTS["jvm_inheritance"])
            } else if uses {
                pair(EDGE_WEIGHTS["jvm_member"])
            } else {
                pair(EDGE_WEIGHTS["jvm_type"])
            }
        }
        Family::Go => {
            if imports {
                pair(EDGE_WEIGHTS["go_import"])
            } else if uses {
                pair(EDGE_WEIGHTS["go_func"])
            } else {
                pair(EDGE_WEIGHTS["go_type"])
            }
        }
        Family::Php => {
            if imports && module {
                pair(EDGE_WEIGHTS["php_require"])
            } else if imports {
                pair(EDGE_WEIGHTS["php_use"])
            } else if relation == Overrides {
                pair(EDGE_WEIGHTS["php_inheritance"])
            } else {
                pair(EDGE_WEIGHTS["php_type"])
            }
        }
        Family::C => {
            if imports {
                pair(EDGE_WEIGHTS["c_include"])
            } else if relation == Overrides {
                pair(EDGE_WEIGHTS["c_inheritance"])
            } else if uses {
                pair(EDGE_WEIGHTS["c_call"])
            } else {
                pair(EDGE_WEIGHTS["c_type"])
            }
        }
        Family::Shell => {
            if imports {
                pair(EDGE_WEIGHTS["shell_source"])
            } else if module {
                pair(EDGE_WEIGHTS["shell_script"])
            } else {
                pair(EDGE_WEIGHTS["shell_call"])
            }
        }
        Family::Swift => {
            if imports {
                pair(EDGE_WEIGHTS["swift_import"])
            } else if relation == Overrides {
                pair(EDGE_WEIGHTS["swift_conformance"])
            } else {
                pair(EDGE_WEIGHTS["swift_type"])
            }
        }
        Family::Sql => pair(EDGE_WEIGHTS["sql_table_ref"]),
        Family::Terraform => {
            let forward = if module {
                TERRAFORM_SEMANTIC.weight * TERRAFORM_SEMANTIC.module_source_modifier
            } else {
                TERRAFORM_SEMANTIC.weight
            };
            (forward, TERRAFORM_SEMANTIC.reverse_factor)
        }
        Family::Dockerfile => {
            let forward = if module {
                DOCKER.copy_weight
            } else {
                DOCKER.weight
            };
            (forward, DOCKER.reverse_factor)
        }
        Family::Config => {
            let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
            if name.starts_with("docker-compose") || name.starts_with("compose.") {
                (DOCKER.compose_weight, DOCKER.reverse_factor)
            } else if language_of(rel)
                .is_some_and(|l| matches!(l, Language::HelmTemplate | Language::HelmValues))
            {
                (HELM.weight, HELM.reverse_factor)
            } else {
                (KUBERNETES.weight, KUBERNETES.reverse_factor)
            }
        }
    }
}

fn slug(family: Family) -> &'static str {
    match family {
        Family::Python => "python",
        Family::TypeScript => "typescript",
        Family::Rust => "rust",
        Family::Kotlin => "kotlin",
        Family::Java => "java",
        Family::Go => "go",
        Family::Php => "php",
        Family::C => "c",
        Family::Config => "config",
        Family::Terraform => "terraform",
        Family::Dockerfile => "dockerfile",
        Family::Shell => "shell",
        Family::Swift => "swift",
        Family::Sql => "sql",
    }
}

fn language_of(rel: &str) -> Option<Language> {
    RepoPath::new(rel)
        .ok()
        .and_then(|p| Language::from_path(&p))
}

/// Whether a repo-relative file is one of the family's.
fn concerns(family: Family, rel: &str) -> bool {
    language_of(rel).is_some_and(|language| family.has(language))
}

fn rel_of(root: &Path, path: &str) -> String {
    crate::paths::display_rel_or_abs(root, Path::new(path))
}

impl CellEdgeBuilder {
    /// The extensionless files of the family: a script is the language its shebang
    /// names, and only a shell cell reads one.
    fn scripts(&self, root: &Path, fragments: &[Fragment]) -> FxHashSet<String> {
        if self.family != Family::Shell {
            return FxHashSet::default();
        }
        fragments
            .iter()
            .filter(|f| f.id.start_line == 1)
            .filter_map(|f| {
                let rel = rel_of(root, f.path());
                let path = RepoPath::new(&rel).ok()?;
                (Language::needs_shebang(&path)
                    && Language::from_shebang(f.content.as_bytes())
                        .is_some_and(|l| self.family.has(l)))
                .then_some(rel)
            })
            .collect()
    }

    fn open(&self, root: &Path) -> Option<Arc<HeadFamily>> {
        let source = current_source();
        match decide::open_head(root, source.range(), self.family) {
            Ok(head) => Some(head),
            Err(error) => {
                tracing::warn!(
                    "edges: the {} cell was not resolved ({error}); it draws no edge",
                    self.family.name()
                );
                note_current(LimitReason::ReadFailed);
                None
            }
        }
    }
}

/// The smallest named fragment of `frags` around `line`, as `impact` owns a use.
fn owner<'a>(frags: &[&'a Fragment], line: u32) -> Option<&'a Fragment> {
    frags
        .iter()
        .copied()
        .filter(|f| {
            f.start_line() <= line
                && line <= f.end_line()
                && f.kind != crate::types::FragmentKind::Excerpt
                && !f.kind.is_signature()
        })
        .min_by_key(|f| (f.symbol_name.is_none(), f.line_count()))
}

/// The directory of a repo-relative file, and its lower-case stem and extension.
fn split(rel: &str) -> (&str, String, String) {
    let path = Path::new(rel);
    (
        rel.rsplit_once('/').map_or("", |(dir, _)| dir),
        base::file_stem_lower(path),
        base::file_ext(path),
    )
}

const HEADER_EXTENSIONS: [&str; 5] = [".h", ".hpp", ".hh", ".hxx", ".h++"];
const IMPL_EXTENSIONS: [&str; 7] = [".c", ".cpp", ".cc", ".cxx", ".c++", ".m", ".mm"];

impl CellEdgeBuilder {
    /// The relations between files the bindings do not state, that the old
    /// builders drew from a file's place: files of one package or directory (the
    /// marker weights, below the naming floor, that admit a sibling through the
    /// declared channel), a Rust crate or module root and the files beside it, a
    /// C header and its implementation. A relation of place, so by place: no text.
    fn units(
        &self,
        representative: &FxHashMap<&str, &FragmentId>,
        head: &HeadFamily,
        edges: &mut EdgeDict,
    ) {
        let mut files: Vec<&str> = representative.keys().copied().collect();
        files.sort_unstable();
        let mut dirs: FxHashMap<&str, Vec<&str>> = FxHashMap::default();
        for file in &files {
            dirs.entry(split(file).0).or_default().push(file);
        }
        let mut pairs = 0usize;
        let mut link =
            |edges: &mut EdgeDict, a: &str, b: &str, forward: f64, reverse: f64| -> bool {
                pairs += 1;
                if !crate::resource::poll_current_every(pairs, 4096) {
                    return false;
                }
                if let (Some(x), Some(y)) = (representative.get(a), representative.get(b)) {
                    add_edge(edges, x, y, forward, reverse);
                }
                true
            };
        match self.family {
            Family::Go => {
                let init: FxHashSet<&str> = head
                    .definitions()
                    .iter()
                    .filter(|d| {
                        d.key.local.name.as_str() == "init" && d.key.local.container.is_empty()
                    })
                    .map(|d| d.file.as_str())
                    .collect();
                let (plain, reverse) = pair(EDGE_WEIGHTS["go_same_package"]);
                for group in dirs.values() {
                    for a in group {
                        let forward = if init.contains(a) {
                            GO_SEMANTIC.init_same_package_weight
                        } else {
                            plain
                        };
                        for b in group.iter().filter(|b| *b != a) {
                            if !link(edges, a, b, forward, reverse) {
                                return;
                            }
                        }
                    }
                }
            }
            Family::Kotlin | Family::Java => {
                let (forward, reverse) = pair(EDGE_WEIGHTS["jvm_same_package"]);
                for group in dirs.values() {
                    for a in group {
                        for b in group.iter().filter(|b| *b != a) {
                            if !link(edges, a, b, forward, reverse) {
                                return;
                            }
                        }
                    }
                }
            }
            Family::Rust => {
                let (forward, reverse) = pair(EDGE_WEIGHTS["rust_same_crate"]);
                for group in dirs.values() {
                    for root in group
                        .iter()
                        .filter(|f| matches!(split(f).1.as_str(), "lib" | "mod"))
                    {
                        for b in group.iter().filter(|b| *b != root) {
                            if !link(edges, root, b, forward, reverse) {
                                return;
                            }
                        }
                    }
                }
            }
            Family::C => {
                let reverse = pair(EDGE_WEIGHTS["c_include"]).1;
                let mut stems: FxHashMap<(&str, String), Vec<&str>> = FxHashMap::default();
                for file in &files {
                    let (dir, stem, _) = split(file);
                    stems.entry((dir, stem)).or_default().push(file);
                }
                for group in stems.values() {
                    let ext = |f: &&str| split(f).2;
                    for h in group
                        .iter()
                        .filter(|f| HEADER_EXTENSIONS.contains(&ext(f).as_str()))
                    {
                        for i in group
                            .iter()
                            .filter(|f| IMPL_EXTENSIONS.contains(&ext(f).as_str()))
                        {
                            if !link(edges, h, i, C_FAMILY_SEMANTIC.base_weight, reverse) {
                                return;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl EdgeBuilder for CellEdgeBuilder {
    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        let mut edges = EdgeDict::default();
        let Some(root) = repo_root else {
            return edges;
        };
        let mut by_rel: FxHashMap<String, Vec<&Fragment>> = FxHashMap::default();
        let scripts = self.scripts(root, fragments);
        for f in fragments {
            let rel = rel_of(root, f.path());
            if concerns(self.family, &rel) || scripts.contains(&rel) {
                by_rel.entry(rel).or_default().push(f);
            }
        }
        if by_rel.is_empty() {
            return edges;
        }
        let Some(head) = self.open(root) else {
            return edges;
        };
        let reps = base::file_representatives(by_rel.values().flatten().copied());
        let representative: FxHashMap<&str, &FragmentId> = by_rel
            .iter()
            .filter_map(|(rel, frags)| Some((rel.as_str(), reps.get(frags.first()?.path())?)))
            .collect();
        let cfg: &CellEdgeWeights = &CELL_EDGES;
        let defs = Defs::of(&head, cfg);
        // Python: an import whose names the file never uses is the old
        // unconfirmed import; every exact use is its own edge.
        let used: FxHashSet<(&str, &str)> = if self.family == Family::Python {
            head.bindings()
                .iter()
                .filter(|b| !matches!(b.relation, Relation::Imports | Relation::Reexports))
                .flat_map(|b| {
                    landings(b, &defs, cfg)
                        .into_iter()
                        .filter(|l| l.exact)
                        .map(move |l| (b.file.as_str(), l.file))
                })
                .collect()
        } else {
            FxHashSet::default()
        };
        let mut reported = 0u64;
        for (at, b) in head.bindings().iter().enumerate() {
            if !crate::resource::poll_emissions(at, 4096, edges.len() as u64, &mut reported) {
                break;
            }
            let Some(src) = by_rel
                .get(b.file.as_str())
                .and_then(|frags| owner(frags, b.site.lines.0))
            else {
                continue;
            };
            for l in landings(b, &defs, cfg) {
                let dst = match l.line {
                    Some(line) => by_rel
                        .get(l.file)
                        .and_then(|frags| owner(frags, line))
                        .map(|f| &f.id),
                    None => representative.get(l.file).copied(),
                };
                let Some(dst) = dst else {
                    continue;
                };
                let (mut forward, reverse) =
                    weight(self.family, b.file.as_str(), b.relation, l.line.is_none());
                if self.family == Family::Python {
                    let imports = matches!(b.relation, Relation::Imports | Relation::Reexports);
                    if imports && !used.contains(&(b.file.as_str(), l.file)) {
                        forward *= PYTHON_SEMANTIC.import_unconfirmed_penalty;
                    } else if !imports && l.exact {
                        forward *= PYTHON_SEMANTIC.import_confirmed_boost;
                    }
                }
                add_edge(&mut edges, &src.id, dst, forward * l.factor, reverse);
            }
        }
        self.units(&representative, &head, &mut edges);
        edges
    }

    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        _file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let Some(root) = repo_root else {
            return Vec::new();
        };
        let changed: FxHashSet<String> = changed
            .iter()
            .map(|p| rel_of(root, &p.to_string_lossy()))
            .filter(|rel| concerns(self.family, rel))
            .collect();
        if changed.is_empty() {
            return Vec::new();
        }
        let Some(head) = self.open(root) else {
            return Vec::new();
        };
        let cfg: &CellEdgeWeights = &CELL_EDGES;
        let defs = Defs::of(&head, cfg);
        let mut near: FxHashMap<&str, FxHashSet<&str>> = FxHashMap::default();
        for b in head.bindings() {
            for l in landings(b, &defs, cfg) {
                near.entry(b.file.as_str()).or_default().insert(l.file);
                near.entry(l.file).or_default().insert(b.file.as_str());
            }
        }
        let mut seen: HashSet<&str> = changed.iter().map(String::as_str).collect();
        let mut frontier: Vec<&str> = seen.iter().copied().collect();
        for _ in 0..SEMANTIC_DISCOVERY.max_depth {
            let mut next = Vec::new();
            for file in &frontier {
                for other in near.get(file).into_iter().flatten() {
                    if seen.insert(other) {
                        next.push(*other);
                    }
                }
            }
            frontier = next;
        }
        let mut found: Vec<PathBuf> = candidates
            .iter()
            .filter(|c| {
                let rel = rel_of(root, &c.to_string_lossy());
                !changed.contains(&rel) && seen.contains(rel.as_str())
            })
            .cloned()
            .collect();
        found.sort();
        found
    }

    /// The Dockerfile and configuration cells are the config category's, as the
    /// builders they replace were; the rest are semantic.
    fn category_label(&self) -> Option<&str> {
        matches!(self.family, Family::Config | Family::Dockerfile).then_some("config")
    }

    fn knife_name(&self) -> String {
        format!("cells-{}", slug(self.family))
    }
}

/// The definitions of a family's head snapshot as the bindings land on them:
/// by key, and (only when unresolved names draw edges) by name.
struct Defs<'a> {
    by_key: FxHashMap<&'a DefKey, (&'a str, u32)>,
    by_name: FxHashMap<&'a str, Vec<(&'a str, u32)>>,
}

impl<'a> Defs<'a> {
    fn of(head: &'a HeadFamily, cfg: &CellEdgeWeights) -> Self {
        let mut by_key = FxHashMap::default();
        let mut by_name: FxHashMap<&str, Vec<(&str, u32)>> = FxHashMap::default();
        for d in head.definitions() {
            let at = (d.file.as_str(), d.name_span.lines.0);
            by_key.insert(&d.key, at);
            if cfg.unresolved {
                by_name
                    .entry(d.key.local.name.as_str())
                    .or_default()
                    .push(at);
            }
        }
        Self { by_key, by_name }
    }
}

/// Where a binding lands: the file, the line of the definition it names (none for
/// a whole file), the factor its standing carries, and whether the landing is
/// exact (the binding was resolved).
struct Landing<'a> {
    file: &'a str,
    line: Option<u32>,
    factor: f64,
    exact: bool,
}

/// The landings of a binding outside its own file, as `CELL_EDGES` counts them:
/// a resolved binding or a module import lands once, at `resolved`; with
/// `candidates` a candidate lands on each of its targets, at `candidate_factor`
/// shared between them; with `unresolved` a binding of an unbound name lands on
/// each definition of that name in the family, at `unresolved_factor` shared
/// between them. A binding the switches leave out, or one that lands too widely,
/// has none.
fn landings<'a>(b: &'a Binding, defs: &Defs<'a>, cfg: &CellEdgeWeights) -> Vec<Landing<'a>> {
    let own = b.file.as_str();
    let shared = |found: Vec<(&'a str, u32)>, share: f64, cap: usize| -> Vec<Landing<'a>> {
        let found: Vec<_> = found.into_iter().filter(|(file, _)| *file != own).collect();
        if found.is_empty() || found.len() > cap {
            return Vec::new();
        }
        let factor = share / found.len() as f64;
        found
            .into_iter()
            .map(|(file, line)| Landing {
                file,
                line: Some(line),
                factor,
                exact: false,
            })
            .collect()
    };
    match &b.resolution {
        Resolution::Resolved { target, .. } => defs
            .by_key
            .get(target)
            .filter(|(file, _)| *file != own)
            .map(|&(file, line)| Landing {
                file,
                line: Some(line),
                factor: cfg.resolved,
                exact: true,
            })
            .into_iter()
            .collect(),
        Resolution::Module { file, .. } if file.as_str() != own => vec![Landing {
            file: file.as_str(),
            line: None,
            factor: cfg.resolved,
            exact: true,
        }],
        Resolution::Candidate { targets, .. } if cfg.candidates => shared(
            targets
                .iter()
                .filter_map(|t| defs.by_key.get(t).copied())
                .collect(),
            cfg.candidate_factor,
            cfg.max_candidates,
        ),
        Resolution::Unresolved { reason }
            if cfg.unresolved
                && reason.is_unknown()
                && *reason != UnresolvedReason::MissingModule
                && !matches!(b.relation, Relation::Imports | Relation::Reexports) =>
        {
            shared(
                defs.by_name
                    .get(b.name.as_str())
                    .cloned()
                    .unwrap_or_default(),
                cfg.unresolved_factor,
                cfg.max_unresolved_targets,
            )
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::edge_weights::CELL_EDGE_GROUPS;

    #[test]
    fn the_groups_are_the_tables_groups() {
        let groups: Vec<&str> = GROUPS.iter().map(|(group, _, _)| *group).collect();
        assert_eq!(groups, CELL_EDGE_GROUPS);
    }

    #[test]
    fn every_old_builder_a_group_replaces_exists_and_every_family_has_a_group() {
        let old: Vec<String> = super::super::semantic::text_builders()
            .iter()
            .chain(super::super::config_edges::text_builders().iter())
            .map(|b| b.knife_name())
            .collect();
        for (group, _, names) in GROUPS.iter() {
            for name in *names {
                assert!(
                    old.iter().any(|o| o == name),
                    "{group} replaces {name}, which no text builder is named"
                );
            }
        }
        for family in Family::ALL {
            assert!(
                GROUPS
                    .iter()
                    .any(|(_, families, _)| families.contains(&family)),
                "{} is in no group",
                family.name()
            );
        }
    }
}
