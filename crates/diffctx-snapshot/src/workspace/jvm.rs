//! Kotlin and the JVM: Gradle settings and build scripts, Maven poms, the
//! source sets under `src/`. The scripts are read as text and never run, so
//! a dependency a script computes is not seen; a script that reaches for
//! another build (`includeBuild`) leaves its projects' dependencies unknown.

use std::collections::{BTreeMap, BTreeSet};

use super::{Draft, Ecosystem, Files, Result, RootOrigin, SourceSet, dir_of, has_extension, join};

const LANGUAGE_DIRS: [&str; 4] = ["kotlin", "java", "scala", "groovy"];

pub(super) fn discover(files: &Files, draft: &mut Draft) -> Result<()> {
    let mut settings: BTreeMap<String, Settings> = BTreeMap::new();
    for name in ["settings.gradle", "settings.gradle.kts"] {
        for path in files.named(name) {
            let root = dir_of(path);
            if let Some(text) = files.read(path)? {
                settings.insert(root.to_string(), parse_settings(root, &text));
            }
        }
    }
    for (root, build) in &settings {
        draft.project(root, Ecosystem::Gradle);
        for (_, dir) in &build.modules {
            draft.project(dir, Ecosystem::Gradle);
        }
        let project = draft.project(root, Ecosystem::Gradle);
        for (_, dir) in &build.modules {
            project.link_member(dir.clone());
        }
        for dir in &build.included_builds {
            project.link_dir(dir.clone());
        }
    }
    for (root, build) in &settings {
        if !build.included_builds.is_empty() {
            let note =
                format!("{root}/settings: includeBuild substitutes dependencies by coordinate");
            for dir in std::iter::once(root).chain(build.modules.iter().map(|(_, d)| d)) {
                draft.unknown(dir, Ecosystem::Gradle, &note, note.clone());
            }
        }
    }

    for name in ["build.gradle", "build.gradle.kts"] {
        for path in files.named(name) {
            let root = dir_of(path);
            let Some(text) = files.read(path)? else {
                continue;
            };
            draft.project(root, Ecosystem::Gradle);
            let build = nearest_settings(&settings, root);
            let mut dirs = Vec::new();
            let mut unresolved = false;
            for reference in project_references(&text) {
                match build.and_then(|b| b.resolve(&reference)) {
                    Some(dir) => dirs.push(dir),
                    None => unresolved = true,
                }
            }
            if unresolved {
                draft.unknown(
                    root,
                    Ecosystem::Gradle,
                    path,
                    "a project dependency names a module no settings file includes".to_string(),
                );
            }
            let project = draft.project(root, Ecosystem::Gradle);
            for dir in dirs {
                project.link_dir(dir);
            }
        }
    }

    for path in files.named("pom.xml") {
        let root = dir_of(path);
        let Some(text) = files.read(path)? else {
            continue;
        };
        let (artifact, modules, dependencies) = read_pom(&text);
        let project = draft.project(root, Ecosystem::Maven);
        if let Some(artifact) = artifact {
            project.name(Ecosystem::Maven, artifact);
        }
        for module in modules {
            let dir = if has_extension(&module, "xml") {
                dir_of(&module).to_string()
            } else {
                module
            };
            if let Some(dir) = join(root, &dir) {
                project.link_member(dir);
            }
        }
        for dependency in dependencies {
            project.link_name(Ecosystem::Maven, dependency);
        }
    }

    source_sets(files, draft);
    Ok(())
}

/// The JVM projects' `src/<set>` directories and the language directories
/// inside them, which the packages hang from.
fn source_sets(files: &Files, draft: &mut Draft) {
    let jvm: Vec<String> = draft
        .projects
        .iter()
        .filter(|(_, p)| {
            p.ecosystems.contains(&Ecosystem::Gradle) || p.ecosystems.contains(&Ecosystem::Maven)
        })
        .map(|(root, _)| root.clone())
        .collect();
    for root in jvm {
        let Some(src) = join(&root, "src") else {
            continue;
        };
        let sets: Vec<String> = files
            .dirs()
            .filter(|d| dir_of(d) == src && d.len() > src.len())
            .map(str::to_string)
            .collect();
        let mut roots = Vec::new();
        let mut found = Vec::new();
        for dir in sets {
            let name = dir[src.len() + 1..].to_string();
            for language in LANGUAGE_DIRS {
                let code = format!("{dir}/{language}");
                if files.is_dir(&code) {
                    roots.push(code);
                }
            }
            found.push(SourceSet {
                test: name.to_lowercase().contains("test"),
                name,
                dir,
            });
        }
        let project = draft.project(&root, Ecosystem::Gradle);
        project.root(root.clone(), RootOrigin::Manifest);
        for dir in roots {
            project.root(dir, RootOrigin::Layout);
        }
        project.source_sets.extend(found);
    }
}

struct Settings {
    root: String,
    /// `(gradle path without the leading colon, directory)`.
    modules: Vec<(String, String)>,
    included_builds: Vec<String>,
}

impl Settings {
    /// The directory of a project reference: a Gradle path (`:core:api`) or
    /// a typesafe accessor (`projects.coreApi`).
    fn resolve(&self, reference: &Reference) -> Option<String> {
        match reference {
            Reference::Path(path) => {
                let path = path.trim_start_matches(':');
                if path.is_empty() {
                    return Some(self.root.clone());
                }
                self.modules
                    .iter()
                    .find(|(p, _)| p == path)
                    .map(|(_, dir)| dir.clone())
                    .or_else(|| join(&self.root, &path.replace(':', "/")))
            }
            Reference::Accessor(segments) => {
                let flat = |s: &str| {
                    s.chars()
                        .filter(|c| c.is_alphanumeric())
                        .collect::<String>()
                        .to_lowercase()
                };
                let wanted: Vec<String> = segments.iter().map(|s| flat(s)).collect();
                self.modules
                    .iter()
                    .find(|(p, _)| {
                        let have: Vec<String> = p.split(':').map(flat).collect();
                        have == wanted
                    })
                    .map(|(_, dir)| dir.clone())
            }
        }
    }
}

fn nearest_settings<'a>(
    settings: &'a BTreeMap<String, Settings>,
    root: &str,
) -> Option<&'a Settings> {
    let mut at = root;
    loop {
        if let Some(found) = settings.get(at) {
            return Some(found);
        }
        if at.is_empty() {
            return None;
        }
        at = dir_of(at);
    }
}

fn quoted(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices();
    while let Some((start, quote)) = chars.next() {
        if quote != '"' && quote != '\'' {
            continue;
        }
        let body = &text[start + 1..];
        match body.find(quote) {
            Some(end) => {
                out.push(body[..end].to_string());
                for _ in 0..body[..=end].chars().count() {
                    chars.next();
                }
            }
            None => break,
        }
    }
    out
}

fn statement(lines: &[&str], from: usize) -> String {
    let mut text = lines[from].to_string();
    let mut at = from;
    let open = |t: &str| t.matches('(').count() > t.matches(')').count();
    while at + 1 < lines.len() && (open(&text) || text.trim_end().ends_with(',')) {
        at += 1;
        text.push(' ');
        text.push_str(lines[at].split("//").next().unwrap_or("").trim());
    }
    text
}

fn parse_settings(root: &str, text: &str) -> Settings {
    let lines: Vec<&str> = text.lines().collect();
    let mut paths: Vec<String> = Vec::new();
    let mut overrides: BTreeMap<String, String> = BTreeMap::new();
    let mut included_builds = Vec::new();
    for (index, raw) in lines.iter().enumerate() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.starts_with("includeBuild") {
            included_builds.extend(
                quoted(&statement(&lines, index))
                    .into_iter()
                    .filter_map(|d| join(root, &d)),
            );
        } else if line.starts_with("include")
            && !line["include".len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            for module in quoted(&statement(&lines, index)) {
                let module = module.trim_start_matches(':').to_string();
                let mut prefix = String::new();
                for segment in module.split(':').filter(|s| !s.is_empty()) {
                    if !prefix.is_empty() {
                        prefix.push(':');
                    }
                    prefix.push_str(segment);
                    paths.push(prefix.clone());
                }
            }
        } else if line.contains("projectDir") && line.contains("project(") {
            let strings = quoted(line);
            if let (Some(first), Some(last)) = (strings.first(), strings.last()) {
                if strings.len() >= 2 {
                    overrides.insert(first.trim_start_matches(':').to_string(), last.clone());
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    let modules = paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .filter_map(|path| {
            let relative = overrides
                .get(&path)
                .cloned()
                .unwrap_or_else(|| path.replace(':', "/"));
            join(root, &relative).map(|dir| (path, dir))
        })
        .collect();
    Settings {
        root: root.to_string(),
        modules,
        included_builds,
    }
}

enum Reference {
    Path(String),
    Accessor(Vec<String>),
}

/// The projects a build script depends on: `project(":core")`,
/// `project(path = ":core")`, `projects.core.api`.
fn project_references(text: &str) -> Vec<Reference> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find("project(") {
        let start = at + found + "project(".len();
        let end = text[start..].find(')').map_or(text.len(), |e| start + e);
        if let Some(path) = quoted(&text[start..end])
            .into_iter()
            .find(|s| s.starts_with(':'))
        {
            out.push(Reference::Path(path));
        }
        at = end;
    }
    let mut at = 0;
    while let Some(found) = text[at..].find("projects.") {
        let before_ok = text[..at + found]
            .chars()
            .next_back()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'));
        let start = at + found + "projects.".len();
        let tail = &text[start..];
        let length = tail
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
            .unwrap_or(tail.len());
        if before_ok && length > 0 {
            let segments: Vec<String> = tail[..length]
                .split('.')
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if !segments.is_empty() {
                out.push(Reference::Accessor(segments));
            }
        }
        at = start + length.max(1);
    }
    out
}

fn blocks<'a>(text: &'a str, tag: &str) -> Vec<&'a str> {
    let (open, close) = (format!("<{tag}"), format!("</{tag}>"));
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(found) = text[at..].find(&open) {
        let name_end = at + found + open.len();
        if !text[name_end..].starts_with(['>', ' ', '/', '\n', '\t', '\r']) {
            at = name_end;
            continue;
        }
        let Some(gt) = text[name_end..].find('>') else {
            break;
        };
        let body = name_end + gt + 1;
        if text[name_end..name_end + gt].ends_with('/') {
            at = body;
            continue;
        }
        let Some(end) = text[body..].find(&close) else {
            break;
        };
        out.push(&text[body..body + end]);
        at = body + end + close.len();
    }
    out
}

fn without(text: &str, tag: &str) -> String {
    let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(&open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(&close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn tag_text<'a>(text: &'a str, tag: &str) -> Option<&'a str> {
    blocks(text, tag).into_iter().next().map(str::trim)
}

/// `(artifactId, module directories, artifactIds depended on)` of a pom.
fn read_pom(text: &str) -> (Option<String>, Vec<String>, Vec<String>) {
    let mut own = text.to_string();
    for tag in [
        "parent",
        "dependencies",
        "dependencyManagement",
        "build",
        "profiles",
        "modules",
        "properties",
        "reporting",
    ] {
        own = without(&own, tag);
    }
    let artifact = tag_text(&own, "artifactId").map(str::to_string);
    let modules = blocks(text, "modules")
        .into_iter()
        .flat_map(|m| blocks(m, "module"))
        .map(|m| m.trim().to_string())
        .collect();
    let mut declared = text.to_string();
    for tag in ["dependencyManagement", "build", "profiles"] {
        declared = without(&declared, tag);
    }
    let dependencies = blocks(&declared, "dependencies")
        .into_iter()
        .flat_map(|d| blocks(d, "dependency"))
        .filter_map(|d| tag_text(&without(d, "exclusions"), "artifactId").map(str::to_string))
        .collect();
    (artifact, modules, dependencies)
}
