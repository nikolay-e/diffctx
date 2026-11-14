//! Which project and module a path belongs to, from the head's workspace model.
//! One workspace answers for both sides of a range, so a manifest added in the
//! range does not turn every key into a new one.

use crate::facts::ids::{ModulePath, ProjectId};
use crate::facts::language::Language;
use crate::snapshot::WorkspaceModel;

pub(super) fn locate(
    workspace: &WorkspaceModel,
    language: Language,
    path: &str,
) -> (ProjectId, ModulePath) {
    let project = workspace.project_of(path);
    let root = project.map_or("", |id| workspace.project(id).root.as_str());
    let module = match language {
        Language::Python => python_module(workspace, project, root, path),
        Language::Rust => workspace
            .rust_module(path)
            .map(|(krate, module)| {
                if module.path.is_empty() {
                    krate.name.clone()
                } else {
                    format!("{}::{}", krate.name, module.path)
                }
            })
            .unwrap_or_else(|| stem(inside(root, path)).to_string()),
        // A module is the SwiftPM target a file sits in; a file outside the
        // `Sources/<target>/` and `Tests/<target>/` layout is in the one unnamed
        // module.
        Language::Swift => swift_target(path).map_or_else(String::new, str::to_string),
        Language::Kotlin | Language::Java | Language::Go => workspace
            .package_of(path)
            .map(|package| package.name)
            .unwrap_or_else(|| stem(inside(root, path)).to_string()),
        Language::JavaScript
        | Language::TypeScript
        | Language::Php
        | Language::Config
        | Language::HelmTemplate
        | Language::HelmValues
        | Language::Shell
        | Language::ShellUnread
        | Language::Html
        | Language::Astro => stem(inside(root, path)).to_string(),
        // A header and its source share a stem and both define the name.
        Language::C | Language::Cpp => inside(root, path).to_string(),
        // A module is a directory, whatever manifest or lockfile the project has.
        Language::Terraform => path
            .rsplit_once('/')
            .map_or(String::new(), |(directory, _)| directory.to_string()),
        // `Dockerfile.dev` and `Dockerfile.prod` share a stem.
        // A routine redefined by a later migration is a definition of each file.
        Language::Dockerfile | Language::Sql => inside(root, path).to_string(),
    };
    (ProjectId::new(root), ModulePath::new(module))
}

/// `pkg/sub/mod.py` -> `pkg.sub.mod` under the longest import root holding it;
/// `pkg/__init__.py` is `pkg`.
fn python_module(
    workspace: &WorkspaceModel,
    project: Option<crate::snapshot::workspace::ProjectId>,
    root: &str,
    path: &str,
) -> String {
    let base = project
        .into_iter()
        .flat_map(|id| workspace.import_roots(id))
        .map(|import_root| import_root.dir.as_str())
        .filter(|dir| dir.is_empty() || path.starts_with(&format!("{dir}/")))
        .max_by_key(|dir| dir.len())
        .unwrap_or(root);
    let dotted = stem(inside(base, path)).replace('/', ".");
    match dotted.strip_suffix(".__init__") {
        Some(package) => package.to_string(),
        None if dotted == "__init__" => String::new(),
        None => dotted,
    }
}

/// The directory under `Sources/` or `Tests/` that holds the file.
fn swift_target(path: &str) -> Option<&str> {
    let parts: Vec<&str> = path.split('/').collect();
    parts
        .windows(3)
        .find(|w| w[0] == "Sources" || w[0] == "Tests")
        .map(|w| w[1])
}

fn inside<'p>(dir: &str, path: &'p str) -> &'p str {
    if dir.is_empty() {
        path
    } else {
        path.strip_prefix(dir)
            .map_or(path, |rest| rest.trim_start_matches('/'))
    }
}

/// The path without its file extension (a dot inside the file name only).
fn stem(path: &str) -> &str {
    let name_start = path.rfind('/').map_or(0, |at| at + 1);
    match path[name_start..].rfind('.') {
        Some(dot) if dot > 0 => &path[..name_start + dot],
        _ => path,
    }
}
