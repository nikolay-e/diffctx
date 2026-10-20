//! Python imports: `import a.b as c`, `from .m import x`, and how a
//! module path lands on a file of the snapshot.

use once_cell::sync::Lazy;
use regex::Regex;

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::FxHashMap;

use super::{FileBindings, Resolution, Resolver, Target, narrow_to_root, normalize, parent_dir};
use crate::facts::python::Step;

/// What a Python environment puts on `sys.path` besides the code itself,
/// read once per directory.
#[derive(Default)]
pub(super) struct PythonEnv {
    pytest: RefCell<FxHashMap<String, Option<Rc<[String]>>>>,
    uv: RefCell<FxHashMap<String, Option<Rc<UvSources>>>>,
    projects: std::cell::OnceCell<FxHashMap<String, String>>,
}

struct UvSources {
    roots: Vec<String>,
    /// A workspace root's sources apply to every member.
    workspace: bool,
}

/// A distribution name as PEP 503 compares it.
fn project_name(name: &str) -> String {
    let mut out = String::new();
    for part in name.split(['-', '_', '.']).filter(|p| !p.is_empty()) {
        if !out.is_empty() {
            out.push('-');
        }
        out.push_str(&part.to_ascii_lowercase());
    }
    out
}

/// `key` of `[section]` in an ini file, continuation lines joined: `None`
/// when the section is absent, empty when the key is.
fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let (mut inside, mut found, mut continues) = (false, false, false);
    let mut value = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            inside = t.trim_matches(['[', ']']).trim() == section;
            found |= inside;
            continues = false;
        } else if !inside || t.is_empty() || t.starts_with(['#', ';']) {
            continues = continues && t.is_empty();
        } else if continues && line.starts_with([' ', '\t']) {
            value.push(' ');
            value.push_str(t);
        } else {
            continues = false;
            if let Some((k, v)) = t.split_once(['=', ':']) {
                if k.trim() == key {
                    value = v.trim().to_string();
                    continues = true;
                }
            }
        }
    }
    found.then_some(value)
}

static PY_IMPORT: Lazy<Regex> = Lazy::new(|| Regex::new(r"^import\s+(.+)$").unwrap());
static PY_FROM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^from\s+([.\w]+)\s+import\s+(.+)$").unwrap());

/// Import statements as single logical lines: parenthesised and
/// backslash-continued imports joined, comments dropped.
fn paren_balance(line: &str) -> isize {
    line.chars()
        .map(|c| match c {
            '(' => 1,
            ')' => -1,
            _ => 0,
        })
        .sum()
}

fn python_statements(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pending: Option<String> = None;
    let mut depth = 0isize;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(acc) = pending.as_mut() {
            acc.push(' ');
            acc.push_str(line.trim_end_matches('\\'));
            depth += paren_balance(line);
            if depth <= 0 && !line.ends_with('\\') {
                out.push(pending.take().unwrap_or_default());
            }
            continue;
        }
        if !(line.starts_with("import ") || line.starts_with("from ")) {
            continue;
        }
        depth = paren_balance(line);
        if depth > 0 || line.ends_with('\\') {
            pending = Some(line.trim_end_matches('\\').to_string());
        } else {
            out.push(line.to_string());
        }
    }
    out.extend(pending);
    out
}

pub fn python_bindings(text: &str) -> FileBindings {
    let mut b = FileBindings::default();
    for stmt in python_statements(text) {
        let stmt = stmt.replace(['(', ')'], " ");
        if let Some(c) = PY_FROM.captures(stmt.trim()) {
            for item in c[2].split(',') {
                bind_from_item(&mut b, &c[1], item);
            }
        } else if let Some(c) = PY_IMPORT.captures(stmt.trim()) {
            for item in c[1].split(',') {
                bind_import_item(&mut b, item);
            }
        }
    }
    b
}

/// `x`, `x as y` or `*` of `from module import …`.
fn bind_from_item(b: &mut FileBindings, module: &str, item: &str) {
    let words: Vec<&str> = item.split_whitespace().collect();
    let (name, local) = match words.as_slice() {
        ["*"] => {
            b.star_from.push(module.to_string());
            return;
        }
        [name] => (*name, *name),
        [name, "as", local] => (*name, *local),
        _ => return,
    };
    b.names.insert(
        local.to_string(),
        Target::Member {
            module: module.to_string(),
            name: name.to_string(),
        },
    );
}

/// `a.b.c` (binds `a`) or `a.b as x` (binds `x` to `a.b`) of `import …`.
fn bind_import_item(b: &mut FileBindings, item: &str) {
    let words: Vec<&str> = item.split_whitespace().collect();
    let (local, module) = match words.as_slice() {
        [dotted] => {
            let head = dotted.split('.').next().unwrap_or(dotted);
            (head, head)
        }
        [dotted, "as", local] => (*local, *dotted),
        _ => return,
    };
    b.names
        .insert(local.to_string(), Target::Module(module.to_string()));
}

impl Resolver<'_> {
    /// Whether `callee` (`make`, `helpers.make`) names a function of the
    /// repository: what it returns the syntax does not say, so a receiver
    /// built by it is unknown, never another class (#477).
    pub(super) fn names_python_function(&self, importer: &str, callee: &str) -> bool {
        if super::lang_of(importer) != Some(super::Lang::Python) {
            return false;
        }
        let (head, function) = match callee.rsplit_once('.') {
            Some((module, f)) => (module, f),
            None => (callee, callee),
        };
        let first = head.split('.').next().unwrap_or(head);
        let Some(target) = self.bindings(importer).names.get(first).cloned() else {
            return false;
        };
        let rest = &head[first.len()..];
        let spec = match (&target, callee.contains('.')) {
            (Target::Member { module, .. }, false) => module.clone(),
            (Target::Member { module, name }, true) => {
                let sep = if module.ends_with('.') { "" } else { "." };
                format!("{module}{sep}{name}{rest}")
            }
            (Target::Module(m), true) => format!("{m}{rest}"),
            (Target::Module(_), false) => return false,
        };
        let name = match &target {
            Target::Member { name, .. } if !callee.contains('.') => name.as_str(),
            _ => function,
        };
        let Resolution::File(file) = self.resolve(importer, &spec) else {
            return false;
        };
        self.text(&file)
            .and_then(|t| crate::facts::python::facts(&t))
            .is_some_and(|f| {
                f.declarations.iter().any(|d| {
                    d.name == name
                        && d.kind == crate::facts::DeclKind::Function
                        && d.class.is_none()
                        && d.scope.is_none()
                })
            })
    }

    /// Whether `path`, a Python file, declares a class named `name` itself.
    pub(super) fn declares_python_class(&self, path: &str, name: &str) -> bool {
        super::lang_of(path) == Some(super::Lang::Python)
            && self
                .text(path)
                .and_then(|t| crate::facts::python::facts(&t))
                .is_some_and(|f| {
                    f.declarations.iter().any(|d| {
                        d.name == name
                            && d.kind == crate::facts::DeclKind::Class
                            && d.scope.is_none()
                    })
                })
    }

    fn python_module_files(&self, rel: &str) -> [String; 2] {
        [format!("{rel}.py"), format!("{rel}/__init__.py")]
    }

    /// What `file` itself puts on `sys.path`, repo-relative.
    fn inserted(&self, file: &str) -> Vec<String> {
        let Some(facts) = self
            .text(file)
            .and_then(|t| crate::facts::python::facts(&t))
        else {
            return Vec::new();
        };
        facts
            .sys_path
            .iter()
            .flatten()
            .filter_map(|steps| {
                let mut parts: Vec<&str> = file.split('/').collect();
                for step in steps {
                    match step {
                        Step::Up => {
                            parts.pop()?;
                        }
                        Step::Down(d) => parts.push(d),
                    }
                }
                Some(parts.join("/"))
            })
            .collect()
    }

    /// The directories `importer` and the repository modules it imports (a
    /// test rig) put on `sys.path`, repo-relative (#525), then what the
    /// environment running it adds: the packages its uv project takes from a
    /// path or the workspace (#557) and, for a test, pytest's `pythonpath`
    /// and what each `conftest.py` above it inserts (#553, #573).
    fn sys_path_roots(&self, importer: &str) -> Rc<[String]> {
        if let Some(known) = self.python_roots.borrow().get(importer) {
            return known.clone();
        }
        let mut roots = self.inserted(importer);
        let bindings = self.bindings(importer);
        let specs = bindings
            .names
            .values()
            .map(|t| match t {
                Target::Module(m) | Target::Member { module: m, .. } => m,
            })
            .chain(&bindings.star_from);
        for spec in specs {
            if let Resolution::File(f) = self.resolve_python_plain(importer, spec) {
                roots.extend(self.inserted(&f));
            }
        }
        let test = crate::testfiles::is_test_path(Path::new(importer));
        let (mut pytest, mut project) = (test, true);
        let mut dir = Some(parent_dir(importer));
        while let Some(d) = dir {
            if pytest {
                let conftest = normalize(&Path::new(d).join("conftest.py"));
                if conftest != importer && self.files.contains(&conftest) {
                    roots.extend(self.inserted(&conftest));
                }
                if let Some(paths) = self.pytest_pythonpath(d) {
                    roots.extend(paths.iter().cloned());
                    pytest = false;
                }
            }
            // The nearest project's sources, and a workspace root's, which
            // uv applies to every member.
            if let Some(uv) = self.uv_sources(d) {
                if project || uv.workspace {
                    roots.extend(uv.roots.iter().cloned());
                }
                project = false;
            }
            dir = (!d.is_empty()).then(|| parent_dir(d));
        }
        roots.dedup();
        let roots: Rc<[String]> = roots.into();
        self.python_roots
            .borrow_mut()
            .insert(importer.to_string(), roots.clone());
        roots
    }

    fn manifest(&self, dir: &str, name: &str) -> Option<Rc<str>> {
        let path = normalize(&Path::new(dir).join(name));
        self.files
            .contains(&path)
            .then(|| self.text(&path))
            .flatten()
    }

    /// The `pythonpath` of the pytest configuration in `dir`, repo-relative,
    /// when `dir` holds one: `pytest.ini`, a `pyproject.toml` with a
    /// `[tool.pytest…]` table, `tox.ini` with `[pytest]`, `setup.cfg` with
    /// `[tool:pytest]`, in the order pytest looks for its rootdir.
    fn pytest_pythonpath(&self, dir: &str) -> Option<Rc<[String]>> {
        if let Some(known) = self.python_env.pytest.borrow().get(dir) {
            return known.clone();
        }
        let ini = self
            .manifest(dir, "pytest.ini")
            .or_else(|| self.manifest(dir, ".pytest.ini"))
            .map(|t| ini_value(&t, "pytest", "pythonpath").unwrap_or_default());
        let toml = || {
            let table = self
                .manifest(dir, "pyproject.toml")?
                .parse::<toml::Table>()
                .ok()?;
            let pytest = table.get("tool")?.get("pytest")?;
            let value = pytest
                .get("ini_options")
                .and_then(|o| o.get("pythonpath"))
                .or_else(|| pytest.get("pythonpath"));
            Some(match value {
                Some(toml::Value::Array(a)) => a
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
                Some(toml::Value::String(s)) => s.clone(),
                _ => String::new(),
            })
        };
        let read = ini
            .or_else(toml)
            .or_else(|| ini_value(&self.manifest(dir, "tox.ini")?, "pytest", "pythonpath"))
            .or_else(|| {
                ini_value(
                    &self.manifest(dir, "setup.cfg")?,
                    "tool:pytest",
                    "pythonpath",
                )
            })
            .map(|value| {
                value
                    .split_whitespace()
                    .map(|entry| normalize(&Path::new(dir).join(entry)))
                    .collect()
            });
        self.python_env
            .pytest
            .borrow_mut()
            .insert(dir.to_string(), read.clone());
        read
    }

    /// The import roots of the packages the uv project in `dir` takes from a
    /// path or from its workspace, a `src/` layout's `src` (#557).
    fn uv_sources(&self, dir: &str) -> Option<Rc<UvSources>> {
        if let Some(known) = self.python_env.uv.borrow().get(dir) {
            return known.clone();
        }
        let read = self.manifest(dir, "pyproject.toml").map(|text| {
            let table = text.parse::<toml::Table>().unwrap_or_default();
            let uv = table.get("tool").and_then(|t| t.get("uv"));
            let sources = uv
                .and_then(|u| u.get("sources"))
                .and_then(toml::Value::as_table);
            let mut roots = Vec::new();
            for (name, spec) in sources.into_iter().flatten() {
                let specs = match spec {
                    toml::Value::Array(a) => a.iter().collect(),
                    one => vec![one],
                };
                for spec in specs {
                    let package = match spec.get("path").and_then(toml::Value::as_str) {
                        Some(path) => Some(normalize(&Path::new(dir).join(path))),
                        None if spec.get("workspace").and_then(toml::Value::as_bool)
                            == Some(true) =>
                        {
                            self.python_projects().get(&project_name(name)).cloned()
                        }
                        None => None,
                    };
                    let Some(package) = package else { continue };
                    let src = normalize(&Path::new(&package).join("src"));
                    let prefix = format!("{src}/");
                    roots.push(if self.files.iter().any(|f| f.starts_with(&prefix)) {
                        src
                    } else {
                        package
                    });
                }
            }
            Rc::new(UvSources {
                roots,
                workspace: uv.and_then(|u| u.get("workspace")).is_some(),
            })
        });
        self.python_env
            .uv
            .borrow_mut()
            .insert(dir.to_string(), read.clone());
        read
    }

    /// Every Python project of the repository, by its normalised name.
    fn python_projects(&self) -> &FxHashMap<String, String> {
        self.python_env.projects.get_or_init(|| {
            self.files
                .iter()
                .filter(|f| f.rsplit('/').next() == Some("pyproject.toml"))
                .filter_map(|f| {
                    let table = self.text(f)?.parse::<toml::Table>().ok()?;
                    let name = table.get("project")?.get("name")?.as_str()?;
                    Some((project_name(name), parent_dir(f).to_string()))
                })
                .collect()
        })
    }

    /// A directory on `sys.path` is searched before the rest: a test that
    /// puts `transcribe/` there imports `transcribe/jobs.py` as `jobs`.
    pub(super) fn resolve_python(&self, importer: &str, spec: &str) -> Resolution {
        if !spec.starts_with('.') {
            let rest = spec.replace('.', "/");
            for root in self.sys_path_roots(importer).iter() {
                let base = if root.is_empty() {
                    rest.clone()
                } else {
                    format!("{root}/{rest}")
                };
                if let Some(f) = self
                    .python_module_files(&base)
                    .into_iter()
                    .find(|f| self.files.contains(f))
                {
                    return Resolution::File(f);
                }
            }
        }
        self.resolve_python_plain(importer, spec)
    }

    fn resolve_python_plain(&self, importer: &str, spec: &str) -> Resolution {
        let dots = spec.chars().take_while(|c| *c == '.').count();
        let rest = spec[dots..].replace('.', "/");
        if dots > 0 {
            return self.resolve_python_relative(importer, dots, &rest);
        }
        let [module_file, package_file] = self.python_module_files(&rest);
        let matches: Vec<&String> = self
            .files
            .iter()
            .filter(|f| {
                [&module_file, &package_file].into_iter().any(|suffix| {
                    f.as_str() == suffix.as_str() || f.ends_with(&format!("/{suffix}"))
                })
            })
            .collect();
        match matches.as_slice() {
            [] => Resolution::External,
            [one] => Resolution::File((*one).clone()),
            _ => narrow_to_root(importer, &matches, &[&module_file, &package_file]),
        }
    }

    fn resolve_python_relative(&self, importer: &str, dots: usize, rest: &str) -> Resolution {
        let mut dir = parent_dir(importer).to_string();
        for _ in 1..dots {
            dir = parent_dir(&dir).to_string();
        }
        let base = match (dir.is_empty(), rest.is_empty()) {
            (_, true) => dir,
            (true, false) => rest.to_string(),
            (false, false) => format!("{dir}/{rest}"),
        };
        let found: Vec<String> = if base.is_empty() {
            vec!["__init__.py".to_string()]
        } else {
            self.python_module_files(&base).into()
        };
        found
            .into_iter()
            .find(|f| self.files.contains(f))
            .map_or(Resolution::Unsupported, Resolution::File)
    }
}
