//! What a repository says about its own shape.
//!
//! Its projects (the directories that hold a manifest), the roots their
//! imports resolve against, and the dependencies the manifests declare
//! between them. Everything is read from
//! manifests through a `ManifestSource`; a configuration file is parsed, never
//! run. A manifest that cannot be read as written leaves its project's
//! dependencies *unknown*, and an unknown dependency set allows every link, so
//! a gap in this model never turns into a missed caller.
//!
//! The rule it serves: a name nothing resolved never crosses a project root,
//! and a link between two projects exists only through a declared dependency.
//! `may_reference` is that predicate.

mod go;
mod jvm;
mod node;
mod python;
mod rust;
mod source;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::snapshot::loss::Loss;
use crate::snapshot::types::RepoPath;

pub use rust::{CrateKind, RustCrate, RustModule, Visibility};
pub use source::ManifestSource;

pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

/// A read of the snapshot failed. An absent manifest is `Ok(None)`, never this.
#[derive(Debug)]
pub struct Error {
    pub path: String,
    pub cause: BoxError,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "listing the snapshot: {}", self.cause)
        } else {
            write!(f, "reading {}: {}", self.path, self.cause)
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectId(u32);

impl ProjectId {
    fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ecosystem {
    Python,
    Node,
    Cargo,
    Gradle,
    Maven,
    Go,
    /// A manifest of a language this model does not read (`composer.json`,
    /// `Gemfile`, `mix.exs`, `deno.json`, `setup.py`): the directory is a
    /// project root, its dependencies are unknown.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootOrigin {
    /// The project directory itself.
    Manifest,
    /// Where the layout puts code: `src/`, `src/main/kotlin`, a crate's `src`.
    Layout,
    /// A directory the manifest names: `pythonpath`, `baseUrl`, a `paths`
    /// target, `package-dir`.
    Configured,
    /// A literal `sys.path.insert(0, "...")`.
    SysPath,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRoot {
    pub dir: String,
    pub origin: RootOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSet {
    pub name: String,
    pub dir: String,
    pub test: bool,
}

/// A `tsconfig` `paths` entry, its targets resolved against `baseUrl`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alias {
    pub pattern: String,
    pub targets: Vec<String>,
}

/// A `package.json` entry point: `exports`, `main`, `module`, `types`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Export {
    pub subpath: String,
    pub target: String,
}

/// A name an `__init__.py` binds from another module of its package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reexport {
    /// The directory of the `__init__.py`.
    pub package: String,
    /// The name as the package exports it (`*` for a star import).
    pub name: String,
    /// The module as written: `.pricing`, `shop.pricing`.
    pub module: String,
}

/// A package a file belongs to, for the languages whose packages the
/// directory states.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    pub project: ProjectId,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub id: ProjectId,
    /// Repo-relative directory; empty for the repository root.
    pub root: String,
    pub ecosystems: BTreeSet<Ecosystem>,
    /// Names the project is known by to its dependants, with the ecosystem
    /// that spells them (Python names are normalised).
    pub names: Vec<(Ecosystem, String)>,
    pub import_roots: Vec<ImportRoot>,
    pub source_sets: Vec<SourceSet>,
    pub aliases: Vec<Alias>,
    pub exports: Vec<Export>,
    pub reexports: Vec<Reexport>,
    /// Directories holding an `__init__.py`.
    pub python_packages: BTreeSet<String>,
    pub crates: Vec<RustCrate>,
    /// The Go module path, which prefixes the import path of every package.
    pub go_module: Option<String>,
    /// Dependencies the manifests declare directly, workspace members
    /// included.
    pub declared: BTreeSet<ProjectId>,
    /// The workspace members among them.
    pub members: BTreeSet<ProjectId>,
    /// False when a manifest of the project could not be read as written.
    pub dependencies_known: bool,
}

#[derive(Default)]
pub struct WorkspaceModel {
    projects: Vec<Project>,
    by_root: BTreeMap<String, ProjectId>,
    /// Everything reachable through declared dependencies, per project.
    reach: Vec<BTreeSet<ProjectId>>,
    /// Whether the project's reach passes through a project whose
    /// dependencies are unknown.
    open: Vec<bool>,
    losses: Vec<Loss>,
}

pub(crate) struct Pending {
    pub(crate) ecosystems: BTreeSet<Ecosystem>,
    pub(crate) names: Vec<(Ecosystem, String)>,
    pub(crate) roots: Vec<ImportRoot>,
    pub(crate) source_sets: Vec<SourceSet>,
    pub(crate) aliases: Vec<Alias>,
    pub(crate) exports: Vec<Export>,
    pub(crate) crates: Vec<RustCrate>,
    pub(crate) go_module: Option<String>,
    pub(crate) links: Vec<Link>,
    pub(crate) unknown: bool,
}

impl Pending {
    fn new() -> Self {
        Self {
            ecosystems: BTreeSet::new(),
            names: Vec::new(),
            roots: Vec::new(),
            source_sets: Vec::new(),
            aliases: Vec::new(),
            exports: Vec::new(),
            crates: Vec::new(),
            go_module: None,
            links: Vec::new(),
            unknown: false,
        }
    }

    pub(crate) fn root(&mut self, dir: String, origin: RootOrigin) {
        if !self.roots.iter().any(|r| r.dir == dir) {
            self.roots.push(ImportRoot { dir, origin });
        }
    }

    pub(crate) fn name(&mut self, ecosystem: Ecosystem, name: String) {
        if !self
            .names
            .iter()
            .any(|(e, n)| *e == ecosystem && *n == name)
        {
            self.names.push((ecosystem, name));
        }
    }

    pub(crate) fn link_dir(&mut self, dir: String) {
        self.links.push(Link::Dir(dir));
    }

    pub(crate) fn link_name(&mut self, ecosystem: Ecosystem, name: String) {
        self.links.push(Link::Name(ecosystem, name));
    }

    /// A project the workspace this one roots lists as a member.
    pub(crate) fn link_member(&mut self, dir: String) {
        self.links.push(Link::Member(dir));
    }
}

/// A dependency as a manifest states it, before every project is known.
pub(crate) enum Link {
    /// A directory: the project that holds it.
    Dir(String),
    /// A package name in an ecosystem: the project that declares it.
    Name(Ecosystem, String),
    /// A member of the workspace rooted here. The root sees its members; a
    /// dependant of the root does not thereby see them.
    Member(String),
}

/// What can only be attached to a project once every manifest is read.
pub(crate) enum Late {
    SysPath {
        file: String,
        literal: String,
    },
    Reexport {
        file: String,
        reexport: Reexport,
    },
    PythonPackage {
        dir: String,
    },
    TsConfig {
        dir: String,
        aliases: Vec<Alias>,
        roots: Vec<String>,
        references: Vec<String>,
    },
}

#[derive(Default)]
pub(crate) struct Draft {
    projects: BTreeMap<String, Pending>,
    losses: Vec<Loss>,
    late: Vec<Late>,
    /// Directories whose projects all see one another (`go.work`).
    mutual: Vec<Vec<String>>,
}

impl Draft {
    pub(crate) fn project(&mut self, root: &str, ecosystem: Ecosystem) -> &mut Pending {
        let pending = self
            .projects
            .entry(root.to_string())
            .or_insert_with(Pending::new);
        pending.ecosystems.insert(ecosystem);
        pending
    }

    pub(crate) fn unknown(&mut self, root: &str, ecosystem: Ecosystem, path: &str, why: String) {
        self.project(root, ecosystem).unknown = true;
        self.lose(path, why);
    }

    /// A manifest, or a file a manifest's reading needed, that was not read
    /// as written: a named unknown.
    pub(crate) fn lose(&mut self, path: &str, reason: String) {
        // The path comes from the snapshot's listing; one that is no path
        // is kept for disclosure, as the listing keeps it.
        let path = RepoPath::new(path).unwrap_or_else(|_| RepoPath::unchecked(path.to_string()));
        self.losses.push(Loss::ManifestUnread { path, reason });
    }

    pub(crate) fn late(&mut self, late: Late) {
        self.late.push(late);
    }

    pub(crate) fn mutual(&mut self, dirs: Vec<String>) {
        self.mutual.push(dirs);
    }
}

/// The snapshot's paths and a way to read them, indexed for the questions the
/// readers ask.
pub(crate) struct Files<'a> {
    source: &'a dyn ManifestSource,
    paths: BTreeSet<String>,
    dirs: BTreeSet<String>,
}

impl<'a> Files<'a> {
    fn load(source: &'a dyn ManifestSource) -> Result<Self> {
        let listed = source.paths().map_err(|cause| Error {
            path: String::new(),
            cause,
        })?;
        let mut dirs = BTreeSet::new();
        for path in &listed {
            let mut dir = dir_of(path);
            while !dir.is_empty() && dirs.insert(dir.to_string()) {
                dir = dir_of(dir);
            }
        }
        Ok(Self {
            source,
            paths: listed.into_iter().collect(),
            dirs,
        })
    }

    pub(crate) fn has(&self, path: &str) -> bool {
        self.paths.contains(path)
    }

    pub(crate) fn is_dir(&self, dir: &str) -> bool {
        dir.is_empty() || self.dirs.contains(dir)
    }

    pub(crate) fn dirs(&self) -> impl Iterator<Item = &str> + '_ {
        self.dirs.iter().map(String::as_str)
    }

    /// Every path whose file name is `name`, outside installed dependencies.
    pub(crate) fn named<'s>(&'s self, name: &'s str) -> impl Iterator<Item = &'s str> + 's {
        self.paths
            .iter()
            .map(String::as_str)
            .filter(move |p| name_of(p) == name && !vendored(p))
    }

    pub(crate) fn all(&self) -> impl Iterator<Item = &str> + '_ {
        self.paths
            .iter()
            .map(String::as_str)
            .filter(|p| !vendored(p))
    }

    pub(crate) fn under(&self, dir: &str) -> impl Iterator<Item = &str> + '_ {
        let prefix = if dir.is_empty() {
            String::new()
        } else {
            format!("{dir}/")
        };
        self.paths
            .range(prefix.clone()..)
            .take_while(move |p| p.starts_with(&prefix))
            .map(String::as_str)
    }

    pub(crate) fn read(&self, path: &str) -> Result<Option<String>> {
        match self.source.read(path) {
            Ok(Some(bytes)) => Ok(Some(
                String::from_utf8_lossy(&bytes)
                    .trim_start_matches('\u{feff}')
                    .to_string(),
            )),
            Ok(None) => Ok(None),
            Err(cause) => Err(Error {
                path: path.to_string(),
                cause,
            }),
        }
    }
}

fn vendored(path: &str) -> bool {
    path.split('/')
        .any(|part| matches!(part, "node_modules" | ".git"))
}

/// Whether the file name ends in `.extension`, in any letter case.
pub(crate) fn has_extension(path: &str, extension: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

pub(crate) fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

pub(crate) fn name_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `rel` against `dir`, normalised; `None` when it leaves the repository.
pub(crate) fn join(dir: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            other => parts.push(other),
        }
    }
    Some(parts.join("/"))
}

fn segment_matches(pattern: &str, segment: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match p.split_first() {
            None => s.is_empty(),
            Some(('*', rest)) => (0..=s.len()).any(|skip| go(rest, &s[skip..])),
            Some(('?', rest)) => !s.is_empty() && go(rest, &s[1..]),
            Some((c, rest)) => s.first() == Some(c) && go(rest, &s[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = segment.chars().collect();
    go(&p, &s)
}

fn path_matches(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| path_matches(rest, &path[skip..])),
        Some((head, rest)) => path
            .split_first()
            .is_some_and(|(seg, tail)| segment_matches(head, seg) && path_matches(rest, tail)),
    }
}

/// The directories under `base` that the workspace globs name, minus the
/// negated ones (`!packages/legacy`).
pub(crate) fn expand_members(files: &Files, base: &str, globs: &[String]) -> Vec<String> {
    let compile = |glob: &str| -> Option<String> {
        join(
            base,
            glob.trim().trim_start_matches("./").trim_end_matches('/'),
        )
    };
    let (excluded, included): (Vec<&String>, Vec<&String>) =
        globs.iter().partition(|g| g.trim_start().starts_with('!'));
    let included: Vec<String> = included.into_iter().filter_map(|g| compile(g)).collect();
    let excluded: Vec<String> = excluded
        .into_iter()
        .filter_map(|g| compile(g.trim_start().trim_start_matches('!')))
        .collect();
    let matches = |patterns: &[String], dir: &str| {
        let segments: Vec<&str> = dir.split('/').collect();
        patterns.iter().any(|p| {
            let pattern: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
            path_matches(&pattern, &segments)
        })
    };
    files
        .dirs()
        .filter(|dir| matches(&included, dir) && !matches(&excluded, dir))
        .map(str::to_string)
        .collect()
}

pub(crate) fn toml_at<'a>(table: &'a toml::Table, path: &[&str]) -> Option<&'a toml::Value> {
    let (first, rest) = path.split_first()?;
    let mut value = table.get(*first)?;
    for key in rest {
        value = value.as_table()?.get(*key)?;
    }
    Some(value)
}

pub(crate) fn toml_strings(value: Option<&toml::Value>) -> Vec<String> {
    match value {
        Some(toml::Value::String(one)) => vec![one.clone()],
        Some(toml::Value::Array(many)) => many
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

impl WorkspaceModel {
    /// Reads every manifest of the snapshot `source` lists.
    pub fn read(source: &dyn ManifestSource) -> Result<Self> {
        let files = Files::load(source)?;
        let mut draft = Draft::default();
        python::discover(&files, &mut draft)?;
        node::discover(&files, &mut draft)?;
        rust::discover(&files, &mut draft)?;
        jvm::discover(&files, &mut draft)?;
        go::discover(&files, &mut draft)?;
        for name in [
            "composer.json",
            "Gemfile",
            "mix.exs",
            "deno.json",
            "setup.py",
        ] {
            for manifest in files.named(name) {
                let ecosystem = if name == "setup.py" {
                    Ecosystem::Python
                } else {
                    Ecosystem::Other
                };
                draft.unknown(
                    dir_of(manifest),
                    ecosystem,
                    manifest,
                    format!("{name} states dependencies this model does not read"),
                );
            }
        }
        let symlinks = source.symlinks().map_err(|cause| Error {
            path: String::new(),
            cause,
        })?;
        Ok(draft.finish(&files, &symlinks))
    }

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn project(&self, id: ProjectId) -> &Project {
        &self.projects[id.index()]
    }

    /// Manifests the model could not read as written.
    pub fn losses(&self) -> &[Loss] {
        &self.losses
    }

    /// The project whose manifest directory is nearest above `path`; `None`
    /// for a path under no manifest, which belongs to every project.
    pub fn project_of(&self, path: &str) -> Option<ProjectId> {
        let mut dir = dir_of(path);
        loop {
            if let Some(id) = self.by_root.get(dir) {
                return Some(*id);
            }
            if dir.is_empty() {
                return None;
            }
            dir = dir_of(dir);
        }
    }

    pub fn import_roots(&self, project: ProjectId) -> &[ImportRoot] {
        &self.project(project).import_roots
    }

    /// The roots code in `project` resolves imports against: its own and
    /// those of every project it depends on.
    pub fn visible_import_roots(&self, project: ProjectId) -> Vec<&ImportRoot> {
        let mut roots: Vec<&ImportRoot> = self.import_roots(project).iter().collect();
        for dependency in &self.reach[project.index()] {
            roots.extend(self.import_roots(*dependency));
        }
        roots
    }

    /// Whether code in `user` can name code in `used` through a declared
    /// dependency, direct or through others. True when either side's
    /// dependencies are unknown.
    pub fn depends(&self, user: ProjectId, used: ProjectId) -> bool {
        user != used && (self.open[user.index()] || self.reach[user.index()].contains(&used))
    }

    /// Whether a name used in `user_path` can refer to a definition in
    /// `used_path`: one project, a declared dependency, or a path under no
    /// manifest. The one place that decides it.
    pub fn may_reference(&self, user_path: &str, used_path: &str) -> bool {
        match (self.project_of(user_path), self.project_of(used_path)) {
            (Some(user), Some(used)) => user == used || self.depends(user, used),
            _ => true,
        }
    }

    /// The package `path` belongs to where the layout states it: a Python
    /// directory with an `__init__.py`, a JVM or Go directory.
    pub fn package_of(&self, path: &str) -> Option<Package> {
        let project = self.project_of(path)?;
        let dir = dir_of(path);
        let extension = path.rsplit_once('.').map_or("", |(_, e)| e);
        let name = match extension {
            "py" => self
                .project(project)
                .python_packages
                .contains(dir)
                .then(|| dir.to_string())?,
            "go" => dir.to_string(),
            "kt" | "kts" | "java" | "scala" | "groovy" => {
                let root = self
                    .import_roots(project)
                    .iter()
                    .filter(|r| {
                        r.origin == RootOrigin::Layout
                            && (dir == r.dir || dir.starts_with(&format!("{}/", r.dir)))
                    })
                    .max_by_key(|r| r.dir.len())?;
                dir.strip_prefix(root.dir.as_str())
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .replace('/', ".")
            }
            _ => return None,
        };
        Some(Package { project, name })
    }

    /// Whether two files are in one package, which sees its own names
    /// without a file naming the other's module.
    pub fn same_package(&self, a: &str, b: &str) -> bool {
        match (self.package_of(a), self.package_of(b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// Where the Rust module `path` sits in its crate, when a crate of the
    /// workspace declares it.
    pub fn rust_module(&self, path: &str) -> Option<(&RustCrate, &RustModule)> {
        let project = self.project(self.project_of(path)?);
        project.crates.iter().find_map(|krate| {
            krate
                .modules
                .iter()
                .find(|m| m.file == path)
                .map(|m| (krate, m))
        })
    }
}

fn owner(roots: &BTreeSet<String>, dir: &str) -> Option<String> {
    let mut at = dir;
    loop {
        if roots.contains(at) {
            return Some(at.to_string());
        }
        if at.is_empty() {
            return None;
        }
        at = dir_of(at);
    }
}

impl Draft {
    #[allow(
        clippy::disallowed_methods,
        reason = "a key absent from these maps is a project with no entries, not a failed read"
    )]
    fn finish(mut self, files: &Files, symlinks: &[(String, String)]) -> WorkspaceModel {
        let roots: BTreeSet<String> = self.projects.keys().cloned().collect();
        let mut packages: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut reexports: BTreeMap<String, Vec<Reexport>> = BTreeMap::new();
        for late in std::mem::take(&mut self.late) {
            match late {
                Late::SysPath { file, literal } => {
                    let Some(home) = owner(&roots, dir_of(&file)) else {
                        continue;
                    };
                    let candidates = [join(dir_of(&file), &literal), join(&home, &literal)];
                    let Some(dir) = candidates
                        .into_iter()
                        .flatten()
                        .find(|d| !d.is_empty() && files.is_dir(d))
                    else {
                        continue;
                    };
                    let other = owner(&roots, &dir);
                    if let Some(project) = self.projects.get_mut(&home) {
                        project.root(dir.clone(), RootOrigin::SysPath);
                        if other.as_deref() != Some(home.as_str()) {
                            project.link_dir(dir);
                        }
                    }
                }
                Late::Reexport { file, reexport } => {
                    if let Some(home) = owner(&roots, dir_of(&file)) {
                        reexports.entry(home).or_default().push(reexport);
                    }
                }
                Late::PythonPackage { dir } => {
                    if let Some(home) = owner(&roots, &dir) {
                        packages.entry(home).or_default().insert(dir);
                    }
                }
                Late::TsConfig {
                    dir,
                    aliases,
                    roots: configured,
                    references,
                } => {
                    let Some(home) = owner(&roots, &dir) else {
                        continue;
                    };
                    // A target that holds the project (`baseUrl: ".."`) is the
                    // project's surroundings, not another project's code.
                    let holds_home =
                        |d: &str| d.is_empty() || home == d || home.starts_with(&format!("{d}/"));
                    let crossing: Vec<String> = configured
                        .iter()
                        .chain(&references)
                        .filter(|d| !holds_home(d) && owner(&roots, d).is_some_and(|o| o != home))
                        .cloned()
                        .collect();
                    if let Some(project) = self.projects.get_mut(&home) {
                        for root in configured {
                            project.root(root, RootOrigin::Configured);
                        }
                        for alias in aliases {
                            if !project.aliases.contains(&alias) {
                                project.aliases.push(alias);
                            }
                        }
                        for target in crossing {
                            project.link_dir(target);
                        }
                    }
                }
            }
        }
        for group in std::mem::take(&mut self.mutual) {
            let homes: BTreeSet<String> = group.iter().filter_map(|d| owner(&roots, d)).collect();
            for home in &homes {
                if let Some(project) = self.projects.get_mut(home) {
                    for other in homes.iter().filter(|o| *o != home) {
                        project.link_dir(other.clone());
                    }
                }
            }
        }
        for (path, target) in symlinks {
            let (Some(resolved), Some(home)) =
                (join(dir_of(path), target), owner(&roots, dir_of(path)))
            else {
                continue;
            };
            if files.is_dir(&resolved) && owner(&roots, &resolved).is_some_and(|o| o != home) {
                if let Some(project) = self.projects.get_mut(&home) {
                    project.link_dir(resolved);
                }
            }
        }

        let order: Vec<String> = self.projects.keys().cloned().collect();
        let by_root: BTreeMap<String, ProjectId> = order
            .iter()
            .enumerate()
            .map(|(i, root)| {
                (
                    root.clone(),
                    ProjectId(u32::try_from(i).unwrap_or(u32::MAX)),
                )
            })
            .collect();
        let mut by_name: BTreeMap<(Ecosystem, String), Vec<ProjectId>> = BTreeMap::new();
        for (root, pending) in &self.projects {
            for (ecosystem, name) in &pending.names {
                by_name
                    .entry((*ecosystem, name.clone()))
                    .or_default()
                    .push(by_root[root]);
            }
        }

        let mut projects = Vec::with_capacity(order.len());
        for root in &order {
            let pending = self.projects.remove(root).unwrap_or_else(Pending::new);
            let id = by_root[root];
            let mut declared = BTreeSet::new();
            let mut members = BTreeSet::new();
            for link in &pending.links {
                match link {
                    Link::Member(dir) => {
                        if let Some(target) = owner(&roots, dir).and_then(|o| by_root.get(&o)) {
                            declared.insert(*target);
                            members.insert(*target);
                        }
                    }
                    Link::Dir(dir) => {
                        if let Some(target) = owner(&roots, dir).and_then(|o| by_root.get(&o)) {
                            declared.insert(*target);
                        }
                    }
                    Link::Name(ecosystem, name) => {
                        if let Some(targets) = by_name.get(&(*ecosystem, name.clone())) {
                            declared.extend(targets);
                        }
                    }
                }
            }
            declared.remove(&id);
            members.remove(&id);
            projects.push(Project {
                id,
                root: root.clone(),
                ecosystems: pending.ecosystems,
                names: pending.names,
                import_roots: pending.roots,
                source_sets: pending.source_sets,
                aliases: pending.aliases,
                exports: pending.exports,
                reexports: reexports.remove(root).unwrap_or_default(),
                python_packages: packages.remove(root).unwrap_or_default(),
                crates: pending.crates,
                go_module: pending.go_module,
                declared,
                members,
                dependencies_known: !pending.unknown,
            });
        }

        let mut reach = Vec::with_capacity(projects.len());
        let mut open = Vec::with_capacity(projects.len());
        for project in &projects {
            let mut seen: BTreeSet<ProjectId> = BTreeSet::new();
            let mut stack: Vec<ProjectId> = project.declared.iter().copied().collect();
            let mut unknown = !project.dependencies_known;
            while let Some(next) = stack.pop() {
                if next == project.id || !seen.insert(next) {
                    continue;
                }
                let dependency = &projects[next.index()];
                unknown |= !dependency.dependencies_known;
                stack.extend(dependency.declared.difference(&dependency.members).copied());
            }
            reach.push(seen);
            open.push(unknown);
        }

        WorkspaceModel {
            projects,
            by_root,
            reach,
            open,
            losses: self.losses,
        }
    }
}
