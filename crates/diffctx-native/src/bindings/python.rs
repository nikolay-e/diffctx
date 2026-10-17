//! Python imports: `import a.b as c`, `from .m import x`, and how a
//! module path lands on a file of the snapshot.

use once_cell::sync::Lazy;
use regex::Regex;

use std::rc::Rc;

use super::{FileBindings, Resolution, Resolver, Target, narrow_to_root, parent_dir};
use crate::facts::python::Step;

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
                        && d.kind == crate::facts::python::DeclKind::Function
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
                            && d.kind == crate::facts::python::DeclKind::Class
                            && d.scope.is_none()
                    })
                })
    }

    fn python_module_files(&self, rel: &str) -> [String; 2] {
        [format!("{rel}.py"), format!("{rel}/__init__.py")]
    }

    /// The directories `importer` and the repository modules it imports (a
    /// test rig) put on `sys.path`, repo-relative (#525).
    fn sys_path_roots(&self, importer: &str) -> Rc<[String]> {
        if let Some(known) = self.python_roots.borrow().get(importer) {
            return known.clone();
        }
        let added = |file: &str| -> Vec<String> {
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
        };
        let mut roots = added(importer);
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
                roots.extend(added(&f));
            }
        }
        roots.dedup();
        let roots: Rc<[String]> = roots.into();
        self.python_roots
            .borrow_mut()
            .insert(importer.to_string(), roots.clone());
        roots
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
