//! Where an import lands: the import roots a file's project sees, the
//! directories the file itself puts on `sys.path`, and relative imports.

use super::index::{Index, Spec, dir_of, join};
use crate::snapshot::WorkspaceModel;

#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) enum ModuleRes {
    File(usize),
    /// A directory of the repository without an `__init__.py`: a namespace
    /// package, whose members are its modules. Carries the dotted name.
    Package(String),
    /// No file of the repository: the standard library, an installed package.
    External,
    /// A relative import that lands on no file.
    Missing,
    /// A file of a project the importer has no declared dependency on.
    Outside,
    /// Found only under a project the importer names as a requirement without
    /// saying where it comes from: the name may be that project or an index's.
    Ambiguous(usize),
}

impl Index {
    /// The directories `sys.path` mentions of the file name, when they hold
    /// Python files. A `__file__`-relative mention is walked from the file; a
    /// plain one is tried from the file's directory and from the repository root.
    fn mention_dirs(&self, at: usize) -> Vec<String> {
        let file = &self.files[at];
        let path = file.facts.path.as_str();
        let mut found = Vec::new();
        for text in &file.mentions {
            let (bases, rest): (Vec<Vec<String>>, &str) =
                if let Some(rest) = text.strip_prefix("__file__") {
                    (vec![path.split('/').map(str::to_string).collect()], rest)
                } else if text.starts_with('/') {
                    continue;
                } else {
                    let here: Vec<String> = file
                        .dir
                        .split('/')
                        .filter(|part| !part.is_empty())
                        .map(str::to_string)
                        .collect();
                    (vec![here, Vec::new()], text.as_str())
                };
            for mut parts in bases {
                let mut walked = true;
                for step in rest.split('/').filter(|part| !part.is_empty()) {
                    match step {
                        "." => {}
                        ".." => walked &= parts.pop().is_some(),
                        other => parts.push(other.to_string()),
                    }
                }
                let dir = parts.join("/");
                if walked && self.dirs.contains(&dir) && !found.contains(&dir) {
                    found.push(dir);
                }
            }
        }
        found
    }

    /// The directories `conftest.py` files of the file's directory and the ones
    /// above it put on `sys.path`: pytest runs every conftest above a test.
    fn conftest_dirs(&self, at: usize) -> Vec<String> {
        let mut found = Vec::new();
        let mut dir = self.files[at].dir.clone();
        loop {
            if let Some(conftest) = self.by_file.get(&join(&dir, "conftest.py")) {
                if *conftest != at {
                    found.extend(self.mention_dirs(*conftest));
                }
            }
            if dir.is_empty() {
                break;
            }
            dir = dir_of(&dir).to_string();
        }
        found
    }

    /// The parent of each `tests/` or `test/` directory around the file, and its
    /// `src/` layout: a test suite is run from the directory above it.
    fn suite_parents(&self, at: usize) -> Vec<String> {
        let mut found = Vec::new();
        let mut dir = self.files[at].dir.as_str();
        while !dir.is_empty() {
            let name = dir.rsplit('/').next().unwrap_or(dir);
            if matches!(name, "tests" | "test") {
                let parent = dir_of(dir);
                found.push(parent.to_string());
                // The `src` layout beside the suite: `src/` holds the packages.
                let layout = join(parent, "src");
                if self.dirs.contains(&layout) && !self.has_init(&layout) {
                    found.push(layout);
                }
            }
            dir = dir_of(dir);
        }
        found
    }

    fn project_roots(&self, workspace: &WorkspaceModel, at: usize) -> Vec<String> {
        match self.files[at].project {
            Some(project) => workspace
                .visible_import_roots(project)
                .into_iter()
                .map(|root| root.dir.clone())
                .collect(),
            // Under no manifest the repository is the project, and its `src`
            // layout is an import root as it is under a manifest.
            None => {
                let layout = "src".to_string();
                let mut roots = vec![String::new()];
                if self.dirs.contains(&layout) && !self.has_init(&layout) {
                    roots.push(layout);
                }
                roots
            }
        }
    }

    /// The directories an absolute import of the file is searched under, in the
    /// order Python searches them: what the file puts on `sys.path`, what a
    /// module it imports first puts there (a test rig), what the `conftest.py`
    /// files above it put there, the directory a script runs from and the one
    /// above a test suite, then the project's import roots.
    pub(super) fn roots(&self, workspace: &WorkspaceModel, at: usize) -> Vec<String> {
        if let Some(known) = self.roots_cache.borrow().get(&at) {
            return known.clone();
        }
        let file = &self.files[at];
        let mut script = file.dir.clone();
        while !script.is_empty() && self.has_init(&script) {
            script = dir_of(&script).to_string();
        }
        let own = self.mention_dirs(at);
        let own_len = own.len();
        let mut plain: Vec<String> = own;
        plain.extend(self.conftest_dirs(at));
        plain.push(script);
        plain.extend(self.suite_parents(at));
        plain.extend(self.project_roots(workspace, at));
        // A module the file imports puts its own directories on `sys.path`
        // before the file's next import: found with the roots known so far.
        let mut rigs: Vec<String> = Vec::new();
        for rec in &file.imports {
            let (relative, module) = match &rec.spec {
                Spec::Module { relative, dotted } => (*relative, dotted.as_str()),
                Spec::Member {
                    relative, module, ..
                }
                | Spec::Star { relative, module } => (*relative, module.as_str()),
            };
            if relative > 0 || module.is_empty() {
                continue;
            }
            let rest = module.replace('.', "/");
            let found = plain.iter().find_map(|root| self.find_module(root, &rest));
            if let Some(rig) = found.filter(|rig| *rig != at) {
                rigs.extend(self.mention_dirs(rig));
            }
        }
        let mut roots: Vec<String> = Vec::new();
        for dir in plain[..own_len]
            .iter()
            .chain(&rigs)
            .chain(&plain[own_len..])
        {
            if !roots.contains(dir) {
                roots.push(dir.clone());
            }
        }
        self.roots_cache.borrow_mut().insert(at, roots.clone());
        roots
    }

    pub(super) fn resolve_module(
        &self,
        workspace: &WorkspaceModel,
        at: usize,
        relative: u32,
        dotted: &str,
    ) -> ModuleRes {
        let file = &self.files[at];
        let user = file.facts.path.as_str();
        let rest = dotted.replace('.', "/");
        let reaches =
            |found: usize| workspace.may_reference(user, self.files[found].facts.path.as_str());
        if relative > 0 {
            let mut base = file.dir.clone();
            for _ in 1..relative {
                base = dir_of(&base).to_string();
            }
            return match self.find_module(&base, &rest) {
                Some(found) if reaches(found) => ModuleRes::File(found),
                Some(_) => ModuleRes::Outside,
                None => ModuleRes::Missing,
            };
        }
        let mut outside = false;
        let roots = self.roots(workspace, at);
        for root in &roots {
            match self.find_module(root, &rest) {
                Some(found) if reaches(found) => return ModuleRes::File(found),
                Some(_) => outside = true,
                None => {}
            }
        }
        if outside {
            return ModuleRes::Outside;
        }
        if let Some(project) = file.project {
            let weak = workspace
                .weak_import_roots(project)
                .into_iter()
                .find_map(|root| self.find_module(&root.dir, &rest));
            if let Some(found) = weak {
                return ModuleRes::Ambiguous(found);
            }
        }
        if roots
            .iter()
            .any(|root| self.dirs.contains(&join(root, &rest)))
        {
            return ModuleRes::Package(dotted.to_string());
        }
        ModuleRes::External
    }
}
