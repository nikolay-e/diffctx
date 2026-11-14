//! Rust: Cargo workspaces and path dependencies, the crates a package
//! builds, and each crate's `mod` tree with the visibility it declares.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    Draft, Ecosystem, Files, Result, RootOrigin, SourceSet, dir_of, expand_members, join, name_of,
    toml_at, toml_strings,
};

const MAX_MODULE_DEPTH: usize = 64;
const MAX_MODULES_PER_CRATE: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CrateKind {
    Lib,
    Bin,
    Test,
    Bench,
    Example,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Visibility {
    Private,
    Pub,
    Crate,
    Super,
    Restricted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustModule {
    /// `a::b` from the crate root; empty for the root itself.
    pub path: String,
    pub file: String,
    pub visibility: Visibility,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RustCrate {
    /// The name `use` spells: the package name with `-` as `_`.
    pub name: String,
    pub kind: CrateKind,
    pub root_file: String,
    pub modules: Vec<RustModule>,
    /// Dependencies the package renames: the name `use` spells and the name of
    /// the crate it stands for.
    pub renames: Vec<(String, String)>,
}

struct Manifest<'a> {
    path: &'a str,
    root: &'a str,
    table: toml::Table,
}

/// A `[workspace.dependencies]` entry: the directory it names.
struct Inherited {
    dir: Option<String>,
    package: Option<String>,
}

pub(super) fn discover(files: &Files, draft: &mut Draft) -> Result<()> {
    let mut manifests = Vec::new();
    for path in files.named("Cargo.toml") {
        let root = dir_of(path);
        let Some(text) = files.read(path)? else {
            continue;
        };
        match toml::from_str::<toml::Table>(&text) {
            Ok(table) => manifests.push(Manifest { path, root, table }),
            Err(error) => draft.unknown(
                root,
                Ecosystem::Cargo,
                path,
                format!("not valid TOML: {error}"),
            ),
        }
    }
    let mut inherited: BTreeMap<&str, BTreeMap<String, Inherited>> = BTreeMap::new();
    for manifest in &manifests {
        let Some(table) =
            toml_at(&manifest.table, &["workspace", "dependencies"]).and_then(|v| v.as_table())
        else {
            continue;
        };
        let entries = table
            .iter()
            .map(|(name, spec)| {
                let dir = spec
                    .get("path")
                    .and_then(|p| p.as_str())
                    .and_then(|p| join(manifest.root, p));
                let package = package_of(spec);
                (name.clone(), Inherited { dir, package })
            })
            .collect();
        inherited.insert(manifest.root, entries);
    }
    let workspace_roots: BTreeSet<&str> = inherited.keys().copied().collect();
    for manifest in &manifests {
        read_manifest(files, manifest, &inherited, &workspace_roots, draft)?;
    }
    Ok(())
}

/// The workspace root that owns `root`: the nearest manifest above it with a
/// `[workspace.dependencies]` table.
fn workspace_of<'a>(workspaces: &BTreeSet<&'a str>, root: &str) -> Option<&'a str> {
    let mut at = root;
    loop {
        if let Some(found) = workspaces.get(at) {
            return Some(*found);
        }
        if at.is_empty() {
            return None;
        }
        at = dir_of(at);
    }
}

fn package_of(spec: &toml::Value) -> Option<String> {
    spec.get("package")
        .and_then(|p| p.as_str())
        .map(str::to_string)
}

fn dependency_tables_of<'a>(table: &'a toml::Table, out: &mut Vec<&'a toml::Table>) {
    for key in ["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(dependencies) = table.get(key).and_then(|v| v.as_table()) {
            out.push(dependencies);
        }
    }
}

fn read_manifest(
    files: &Files,
    manifest: &Manifest,
    inherited: &BTreeMap<&str, BTreeMap<String, Inherited>>,
    workspaces: &BTreeSet<&str>,
    draft: &mut Draft,
) -> Result<()> {
    let (root, table) = (manifest.root, &manifest.table);
    let package = table.get("package").and_then(|p| p.as_table());
    let package_name = package
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_string);

    let members = toml_strings(toml_at(table, &["workspace", "members"]));
    let excluded = toml_strings(toml_at(table, &["workspace", "exclude"]));
    let mut globs = members;
    globs.extend(excluded.iter().map(|e| format!("!{e}")));
    let member_dirs: Vec<String> = expand_members(files, root, &globs)
        .into_iter()
        .filter(|dir| files.has(&format!("{dir}/Cargo.toml")))
        .collect();

    let mut dependency_tables: Vec<&toml::Table> = Vec::new();
    dependency_tables_of(table, &mut dependency_tables);
    if let Some(targets) = table.get("target").and_then(|v| v.as_table()) {
        for target in targets.values().filter_map(|v| v.as_table()) {
            dependency_tables_of(target, &mut dependency_tables);
        }
    }
    let patched = ["patch", "replace"]
        .iter()
        .any(|key| table.get(*key).is_some());

    let owner = workspace_of(workspaces, root).and_then(|w| inherited.get(w));

    let mut links: Vec<String> = Vec::new();
    let mut renames: Vec<(String, String)> = Vec::new();
    for dependencies in dependency_tables {
        for (name, spec) in dependencies {
            let package = package_of(spec).or_else(|| {
                let inherited = spec.get("workspace").and_then(|w| w.as_bool()) == Some(true);
                inherited
                    .then(|| {
                        owner
                            .and_then(|o| o.get(name))
                            .and_then(|e| e.package.clone())
                    })
                    .flatten()
            });
            if let Some(package) = package.filter(|p| p != name) {
                renames.push((name.replace('-', "_"), package.replace('-', "_")));
            }
            let own_path = spec
                .get("path")
                .and_then(|p| p.as_str())
                .and_then(|p| join(root, p));
            if let Some(dir) = own_path {
                links.push(dir);
                continue;
            }
            if spec.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                if let Some(entry) = owner.and_then(|o| o.get(name)) {
                    links.extend(entry.dir.clone());
                }
            }
        }
    }

    let mut crates = crates_of(files, manifest, package_name.as_deref())?;
    for krate in &mut crates {
        krate.renames.clone_from(&renames);
    }
    let sets = ["src", "tests", "benches", "examples"]
        .iter()
        .filter_map(|name| {
            let dir = join(root, name)?;
            files.is_dir(&dir).then(|| SourceSet {
                name: (*name).to_string(),
                dir,
                test: *name != "src" && *name != "examples",
            })
        });
    let source_sets: Vec<SourceSet> = sets.collect();
    let src_root = join(root, "src").filter(|d| files.is_dir(d));

    if patched {
        draft.unknown(
            root,
            Ecosystem::Cargo,
            manifest.path,
            "[patch] or [replace] redirects dependencies this model does not follow".to_string(),
        );
    }
    let project = draft.project(root, Ecosystem::Cargo);
    project.root(root.to_string(), RootOrigin::Manifest);
    if let Some(src) = src_root {
        project.root(src, RootOrigin::Layout);
    }
    if let Some(name) = package_name {
        project.name(Ecosystem::Cargo, name);
    }
    for dir in links {
        project.link_dir(dir);
    }
    for dir in member_dirs {
        project.link_member(dir);
    }
    project.source_sets.extend(source_sets);
    project.crates.extend(crates);
    Ok(())
}

/// The crates a package builds: the library, the binaries, and the
/// integration tests, benchmarks and examples cargo finds by convention or
/// the manifest names.
fn crates_of(files: &Files, manifest: &Manifest, package: Option<&str>) -> Result<Vec<RustCrate>> {
    let Some(package) = package else {
        return Ok(Vec::new());
    };
    let (root, table) = (manifest.root, &manifest.table);
    let ident = |name: &str| name.replace('-', "_");
    let mut roots: Vec<(String, CrateKind, String)> = Vec::new();

    let lib = table.get("lib").and_then(|v| v.as_table());
    let lib_path = lib
        .and_then(|l| l.get("path"))
        .and_then(|p| p.as_str())
        .and_then(|p| join(root, p))
        .or_else(|| join(root, "src/lib.rs"));
    let lib_name = lib
        .and_then(|l| l.get("name"))
        .and_then(|n| n.as_str())
        .map_or_else(|| ident(package), ident);
    if let Some(path) = lib_path.filter(|p| files.has(p)) {
        roots.push((lib_name, CrateKind::Lib, path));
    }

    let mut conventional = |dir: &str, kind: CrateKind, main: Option<&str>| {
        let Some(base) = join(root, dir) else { return };
        if let Some(main) = main.and_then(|m| join(root, m)).filter(|m| files.has(m)) {
            roots.push((ident(package), kind, main));
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for path in files.under(&base) {
            let rest = &path[base.len() + 1..];
            let (name, file) = match rest.split_once('/') {
                None => match rest.strip_suffix(".rs") {
                    Some(stem) => (stem, path),
                    None => continue,
                },
                Some((sub, "main.rs")) => (sub, path),
                _ => continue,
            };
            if seen.insert(name.to_string()) {
                roots.push((ident(name), kind, file.to_string()));
            }
        }
    };
    conventional("src/bin", CrateKind::Bin, Some("src/main.rs"));
    conventional("tests", CrateKind::Test, None);
    conventional("benches", CrateKind::Bench, None);
    conventional("examples", CrateKind::Example, None);

    for (key, kind) in [
        ("bin", CrateKind::Bin),
        ("test", CrateKind::Test),
        ("bench", CrateKind::Bench),
        ("example", CrateKind::Example),
    ] {
        let Some(entries) = table.get(key).and_then(|v| v.as_array()) else {
            continue;
        };
        for entry in entries {
            let name = entry.get("name").and_then(|n| n.as_str());
            let path = entry
                .get("path")
                .and_then(|p| p.as_str())
                .and_then(|p| join(root, p));
            if let (Some(name), Some(path)) = (name, path) {
                if files.has(&path) && !roots.iter().any(|(_, _, p)| *p == path) {
                    roots.push((ident(name), kind, path));
                }
            }
        }
    }

    let mut out = Vec::new();
    for (name, kind, root_file) in roots {
        let modules = module_tree(files, &root_file)?;
        out.push(RustCrate {
            name,
            kind,
            root_file,
            modules,
            renames: Vec::new(),
        });
    }
    Ok(out)
}

fn module_tree(files: &Files, root_file: &str) -> Result<Vec<RustModule>> {
    let mut modules = vec![RustModule {
        path: String::new(),
        file: root_file.to_string(),
        visibility: Visibility::Pub,
    }];
    let mut seen: BTreeSet<String> = BTreeSet::from([root_file.to_string()]);
    let mut stack: Vec<(String, String, bool)> = vec![(root_file.to_string(), String::new(), true)];
    while let Some((file, module, owns_dir)) = stack.pop() {
        if module.split("::").count() > MAX_MODULE_DEPTH || modules.len() >= MAX_MODULES_PER_CRATE {
            continue;
        }
        let Some(text) = files.read(&file)? else {
            continue;
        };
        let children_dir = if owns_dir {
            dir_of(&file).to_string()
        } else {
            let stem = name_of(&file).trim_end_matches(".rs");
            match join(dir_of(&file), stem) {
                Some(dir) => dir,
                None => continue,
            }
        };
        for (name, visibility, path_attribute) in declarations(&text) {
            let candidates: Vec<String> = match path_attribute {
                Some(attribute) => join(dir_of(&file), &attribute).into_iter().collect(),
                None => [
                    join(&children_dir, &format!("{name}.rs")),
                    join(&children_dir, &format!("{name}/mod.rs")),
                ]
                .into_iter()
                .flatten()
                .collect(),
            };
            let Some(child) = candidates.into_iter().find(|c| files.has(c)) else {
                continue;
            };
            if !seen.insert(child.clone()) {
                continue;
            }
            let path = if module.is_empty() {
                name.clone()
            } else {
                format!("{module}::{name}")
            };
            let owns = name_of(&child) == "mod.rs";
            modules.push(RustModule {
                path: path.clone(),
                file: child.clone(),
                visibility,
            });
            stack.push((child, path, owns));
        }
    }
    Ok(modules)
}

/// `(name, visibility, #[path])` of every `mod name;` declared outside
/// comments, one per line.
fn declarations(text: &str) -> Vec<(String, Visibility, Option<String>)> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut path_attribute: Option<String> = None;
    for line in text.lines() {
        let mut code = line;
        if in_block {
            match code.find("*/") {
                Some(end) => {
                    code = &code[end + 2..];
                    in_block = false;
                }
                None => continue,
            }
        }
        let code = code.split("//").next().unwrap_or("");
        if let Some(start) = code.find("/*") {
            if !code[start..].contains("*/") {
                in_block = true;
            }
        }
        let trimmed = code.trim();
        if let Some(attribute) = trimmed
            .strip_prefix("#[path")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
        {
            let literal = attribute.trim().trim_end_matches(']').trim();
            path_attribute = literal
                .strip_prefix('"')
                .and_then(|l| l.strip_suffix('"'))
                .map(str::to_string);
            continue;
        }
        if let Some((visibility, name)) = parse_declaration(trimmed) {
            out.push((name.to_string(), visibility, path_attribute.take()));
        } else if !trimmed.starts_with("#[") && !trimmed.is_empty() {
            path_attribute = None;
        }
    }
    out
}

fn parse_declaration(line: &str) -> Option<(Visibility, &str)> {
    let mut rest = line;
    while rest.starts_with("#[") {
        rest = rest[rest.find(']')? + 1..].trim_start();
    }
    let (visibility, rest) = match rest.strip_prefix("pub") {
        Some(after) if after.starts_with('(') => {
            let close = after.find(')')?;
            let inner = after[1..close].trim();
            let visibility = match inner {
                "crate" => Visibility::Crate,
                "super" => Visibility::Super,
                "self" => Visibility::Private,
                _ => Visibility::Restricted,
            };
            (visibility, after[close + 1..].trim_start())
        }
        Some(after) if after.starts_with(char::is_whitespace) => {
            (Visibility::Pub, after.trim_start())
        }
        _ => (Visibility::Private, rest),
    };
    let rest = rest.strip_prefix("mod")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let name = &rest[..end];
    (!name.is_empty() && rest[end..].trim_start().starts_with(';')).then_some((visibility, name))
}
