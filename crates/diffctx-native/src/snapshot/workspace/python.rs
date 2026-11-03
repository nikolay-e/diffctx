//! Python: `pyproject.toml` (PEP 621, uv, Poetry, setuptools, hatch), the
//! `src` layout, literal `sys.path` edits, and what an `__init__.py` binds.

use super::{
    Draft, Ecosystem, Files, Late, Reexport, Result, RootOrigin, dir_of, expand_members,
    has_extension, join, name_of, toml_at, toml_strings,
};

/// A source file larger than this is not scanned for `sys.path` edits.
const MAX_SCANNED_FILE: usize = 256 * 1024;
const MAX_SCANNED_TOTAL: usize = 64 * 1024 * 1024;

pub(super) fn discover(files: &Files, draft: &mut Draft) -> Result<()> {
    for manifest in files.named("pyproject.toml") {
        let root = dir_of(manifest);
        let Some(text) = files.read(manifest)? else {
            continue;
        };
        draft.project(root, Ecosystem::Python);
        match toml::from_str::<toml::Table>(&text) {
            Ok(table) => read_pyproject(files, root, &table, draft),
            Err(error) => draft.unknown(
                root,
                Ecosystem::Python,
                manifest,
                format!("not valid TOML: {error}"),
            ),
        }
    }
    scan_sources(files, draft)
}

/// PEP 503 name: case-folded, runs of `-_.` one `-`.
fn normalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut separator = false;
    for c in name.chars() {
        if matches!(c, '-' | '_' | '.') {
            separator = true;
        } else {
            if separator && !out.is_empty() {
                out.push('-');
            }
            separator = false;
            out.push(c.to_ascii_lowercase());
        }
    }
    out
}

fn requirement_name(requirement: &str) -> Option<String> {
    let name: String = requirement
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .collect();
    (!name.is_empty()).then(|| normalize(&name))
}

/// `name @ file:../b` points at a directory.
fn direct_reference(requirement: &str) -> Option<&str> {
    let (_, target) = requirement.split_once(" @ ")?;
    let target = target.split(';').next()?.trim();
    let target = target.strip_prefix("file://").unwrap_or(target);
    let target = target.strip_prefix("file:").unwrap_or(target);
    (target.starts_with("./") || target.starts_with("../")).then_some(target)
}

fn read_pyproject(files: &Files, root: &str, table: &toml::Table, draft: &mut Draft) {
    let mut requirements: Vec<String> = Vec::new();
    requirements.extend(toml_strings(toml_at(table, &["project", "dependencies"])));
    if let Some(optional) =
        toml_at(table, &["project", "optional-dependencies"]).and_then(|v| v.as_table())
    {
        for group in optional.values() {
            requirements.extend(toml_strings(Some(group)));
        }
    }
    if let Some(groups) = table.get("dependency-groups").and_then(|v| v.as_table()) {
        for group in groups.values() {
            requirements.extend(toml_strings(Some(group)));
        }
    }
    requirements.extend(toml_strings(toml_at(
        table,
        &["tool", "uv", "dev-dependencies"],
    )));
    requirements.extend(toml_strings(toml_at(table, &["build-system", "requires"])));

    let mut poetry: Vec<(String, Option<String>)> = Vec::new();
    let mut poetry_tables: Vec<&toml::Table> = Vec::new();
    if let Some(t) = toml_at(table, &["tool", "poetry", "dependencies"]).and_then(|v| v.as_table())
    {
        poetry_tables.push(t);
    }
    if let Some(groups) = toml_at(table, &["tool", "poetry", "group"]).and_then(|v| v.as_table()) {
        for group in groups.values() {
            if let Some(t) = group.get("dependencies").and_then(|v| v.as_table()) {
                poetry_tables.push(t);
            }
        }
    }
    for dependencies in poetry_tables {
        for (name, spec) in dependencies {
            let path = spec
                .as_table()
                .and_then(|t| t.get("path"))
                .and_then(|p| p.as_str())
                .map(str::to_string);
            poetry.push((normalize(name), path));
        }
    }

    let sources = toml_at(table, &["tool", "uv", "sources"]).and_then(|v| v.as_table());
    let members = toml_strings(toml_at(table, &["tool", "uv", "workspace", "members"]));
    let excluded = toml_strings(toml_at(table, &["tool", "uv", "workspace", "exclude"]));

    let mut roots: Vec<String> = Vec::new();
    roots.extend(
        toml_strings(toml_at(
            table,
            &["tool", "pytest", "ini_options", "pythonpath"],
        ))
        .iter()
        .filter_map(|p| join(root, p)),
    );
    for key in [
        &["tool", "setuptools", "package-dir"][..],
        &["tool", "setuptools", "packages", "find", "where"][..],
    ] {
        match toml_at(table, key) {
            Some(toml::Value::Table(dirs)) => roots.extend(
                dirs.values()
                    .filter_map(|v| v.as_str())
                    .filter_map(|p| join(root, p)),
            ),
            other => roots.extend(toml_strings(other).iter().filter_map(|p| join(root, p))),
        }
    }
    if let Some(toml::Value::Array(packages)) = toml_at(table, &["tool", "poetry", "packages"]) {
        for package in packages {
            if let Some(from) = package.get("from").and_then(|v| v.as_str()) {
                roots.extend(join(root, from));
            }
        }
    }
    for package in toml_strings(toml_at(
        table,
        &["tool", "hatch", "build", "targets", "wheel", "packages"],
    )) {
        if package.contains('/') {
            roots.extend(join(root, dir_of(&package)));
        }
    }
    roots.extend(
        toml_strings(toml_at(table, &["tool", "maturin", "python-source"]))
            .iter()
            .filter_map(|p| join(root, p)),
    );

    let name = toml_at(table, &["project", "name"])
        .or_else(|| toml_at(table, &["tool", "poetry", "name"]))
        .and_then(|v| v.as_str())
        .map(normalize);

    let src = join(root, "src");
    let layout = src.filter(|src| {
        !files.has(&format!("{src}/__init__.py"))
            && files.under(src).any(|p| has_extension(p, "py"))
    });

    let member_dirs: Vec<String> = expand_members(files, root, &members)
        .into_iter()
        .filter(|dir| {
            !excluded
                .iter()
                .any(|e| join(root, e).as_deref() == Some(dir))
        })
        .filter(|dir| files.has(&format!("{dir}/pyproject.toml")))
        .collect();

    let project = draft.project(root, Ecosystem::Python);
    project.root(root.to_string(), RootOrigin::Manifest);
    if let Some(layout) = layout {
        project.root(layout, RootOrigin::Layout);
    }
    for dir in roots {
        project.root(dir, RootOrigin::Configured);
    }
    if let Some(name) = name {
        project.name(Ecosystem::Python, name);
    }
    for requirement in &requirements {
        if let Some(name) = requirement_name(requirement) {
            project.link_name(Ecosystem::Python, name);
        }
        if let Some(dir) = direct_reference(requirement).and_then(|t| join(root, t)) {
            project.link_dir(dir);
        }
    }
    for (name, path) in poetry {
        project.link_name(Ecosystem::Python, name);
        if let Some(dir) = path.and_then(|p| join(root, &p)) {
            project.link_dir(dir);
        }
    }
    if let Some(sources) = sources {
        for (name, source) in sources {
            if let Some(dir) = source
                .get("path")
                .and_then(|p| p.as_str())
                .and_then(|p| join(root, p))
            {
                project.link_dir(dir);
            }
            if source.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
                project.link_name(Ecosystem::Python, normalize(name));
            }
        }
    }
    for member in member_dirs {
        project.link_member(member);
    }
}

/// `__init__.py` files (packages and what they re-export) and literal
/// `sys.path` edits, read from every Python file once.
fn scan_sources(files: &Files, draft: &mut Draft) -> Result<()> {
    let mut budget = MAX_SCANNED_TOTAL;
    let mut first_skipped: Option<&str> = None;
    let sources: Vec<&str> = files.all().filter(|p| has_extension(p, "py")).collect();
    for path in sources {
        let is_init = name_of(path) == "__init__.py";
        if is_init {
            draft.late(Late::PythonPackage {
                dir: dir_of(path).to_string(),
            });
        }
        if budget == 0 {
            first_skipped.get_or_insert(path);
            continue;
        }
        let Some(text) = files.read(path)? else {
            continue;
        };
        if text.len() > MAX_SCANNED_FILE {
            continue;
        }
        budget = budget.saturating_sub(text.len());
        if text.contains("sys.path") {
            for literal in sys_path_literals(&text) {
                draft.late(Late::SysPath {
                    file: path.to_string(),
                    literal,
                });
            }
        }
        if is_init {
            for (name, module) in reexports(&text) {
                draft.late(Late::Reexport {
                    file: path.to_string(),
                    reexport: Reexport {
                        package: dir_of(path).to_string(),
                        name,
                        module,
                    },
                });
            }
        }
    }
    if let Some(path) = first_skipped {
        draft.lose(
            path,
            "the Python source scan stopped at its byte budget before this file".to_string(),
        );
    }
    Ok(())
}

/// The string literals of `sys.path.insert(<n>, "<literal>")` and
/// `sys.path.append("<literal>")`; an argument that is anything else is not
/// a literal and is not read.
fn sys_path_literals(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let code = line.split('#').next().unwrap_or("");
        let argument = if let Some((_, rest)) = code.split_once("sys.path.insert(") {
            rest.split_once(',').map(|(_, a)| a)
        } else if let Some((_, rest)) = code.split_once("sys.path.append(") {
            Some(rest)
        } else {
            None
        };
        let Some(argument) = argument else { continue };
        let argument = argument.trim();
        let Some(quote) = argument.chars().next().filter(|c| matches!(c, '"' | '\'')) else {
            continue;
        };
        let body = &argument[1..];
        let Some(end) = body.find(quote) else {
            continue;
        };
        if body[end + 1..].trim_start().starts_with(')') && end > 0 {
            found.push(body[..end].to_string());
        }
    }
    found
}

/// `(exported name, module as written)` of every top-level
/// `from <module> import a, b as c` of an `__init__.py`, parenthesised lists
/// over several lines included.
fn reexports(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some(rest) = line.strip_prefix("from ") else {
            continue;
        };
        let Some((module, names)) = rest.split_once(" import ") else {
            continue;
        };
        let module = module.trim();
        if module == "__future__" {
            continue;
        }
        let mut names = names.split('#').next().unwrap_or("").trim().to_string();
        if names.starts_with('(') {
            while !names.contains(')') {
                let Some(next) = lines.next() else { break };
                names.push(' ');
                names.push_str(next.split('#').next().unwrap_or("").trim());
            }
        }
        let names = names.trim_matches(|c| c == '(' || c == ')');
        for item in names.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let exported = item.rsplit(" as ").next().unwrap_or(item).trim();
            out.push((exported.to_string(), module.to_string()));
        }
    }
    out
}
