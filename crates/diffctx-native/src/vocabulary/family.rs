//! The languages one resolver reads, and the resolver behind each: the layer the
//! impact answer and the ranking graph's cells share.

#![deny(clippy::disallowed_methods)]

use std::sync::Arc;

use anyhow::Result;

use crate::facts::{DefKey, FactsCache, Language};
use crate::resolve::{
    Binding, CResolver, ConfigResolver, Definition, DockerfileResolver, GoResolver, JavaResolver,
    KotlinResolver, PhpResolver, PythonResolver, RustResolver, ShellResolver, SqlResolver,
    SwiftResolver, TerraformResolver, TypeScriptResolver,
};
use crate::snapshot::{
    self, Admission, AdmissionPolicy, AnalysisContext, Loss, RangeSpec, RepoPath, RunBudget, Side,
    SideTag, WorkspaceModel,
};

/// The languages one resolver reads: a cell answers for one family.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Family {
    Python,
    TypeScript,
    Rust,
    Kotlin,
    Java,
    Go,
    Php,
    C,
    /// YAML files read as configuration objects: what an object's fields name,
    /// by the table of the facts layer.
    Config,
    /// Terraform files: a module is a directory, and a name is a traversal's
    /// root and its attributes.
    Terraform,
    /// A Dockerfile: its stages, the variables and places its instructions name.
    Dockerfile,
    /// Shell scripts: functions and variables, the files a script sources.
    Shell,
    /// Swift: the Kotlin engine in its Swift dialect, a module being a target.
    Swift,
    /// SQL files: the routines and views they define, and the calls and reads of them.
    Sql,
}

impl Family {
    pub(crate) const ALL: [Self; 14] = [
        Self::Python,
        Self::TypeScript,
        Self::Rust,
        Self::Kotlin,
        Self::Java,
        Self::Go,
        Self::Php,
        Self::C,
        Self::Config,
        Self::Terraform,
        Self::Dockerfile,
        Self::Shell,
        Self::Swift,
        Self::Sql,
    ];

    pub(crate) fn has(self, language: Language) -> bool {
        match self {
            Self::Python => language == Language::Python,
            Self::TypeScript => matches!(
                language,
                Language::JavaScript | Language::TypeScript | Language::Html | Language::Astro
            ),
            Self::Rust => language == Language::Rust,
            Self::Kotlin => language == Language::Kotlin,
            Self::Java => language == Language::Java,
            Self::Go => language == Language::Go,
            Self::Php => language == Language::Php,
            Self::C => matches!(language, Language::C | Language::Cpp),
            Self::Config => language.is_config_family(),
            Self::Terraform => language == Language::Terraform,
            Self::Dockerfile => language == Language::Dockerfile,
            Self::Shell => matches!(language, Language::Shell | Language::ShellUnread),
            Self::Swift => language == Language::Swift,
            Self::Sql => language == Language::Sql,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Python => "Python",
            Self::TypeScript => "TypeScript/JavaScript",
            Self::Rust => "Rust",
            Self::Kotlin => "Kotlin",
            Self::Java => "Java",
            Self::Go => "Go",
            Self::Php => "PHP",
            Self::C => "C",
            Self::Config => "configuration",
            Self::Terraform => "Terraform",
            Self::Dockerfile => "Dockerfile",
            Self::Shell => "shell",
            Self::Swift => "Swift",
            Self::Sql => "SQL",
        }
    }

    /// Whether a file of this extension is read by the family's resolver.
    pub(crate) fn has_path(self, path: &str) -> bool {
        // A YAML file is the configuration cell's only where it holds objects
        // (`Cell::owns` reads that off the resolver): the rest of the YAML of a
        // repository (workflows, compose files, values) is read by other paths.
        self != Self::Config && self.by_extension(path)
    }

    fn by_extension(self, path: &str) -> bool {
        RepoPath::new(path)
            .is_ok_and(|p| Language::from_path(&p).is_some_and(|language| self.has(language)))
    }

    /// The files that can name this family's definitions: the languages that
    /// call into each other share a group (Kotlin and Java, C and C++, the web's
    /// scripts and the markup that embeds them), any other family stands alone.
    pub const fn group(self) -> Group {
        match self {
            Self::Kotlin | Self::Java => Group::Jvm,
            Self::C => Group::Native,
            Self::TypeScript => Group::Web,
            Self::Python
            | Self::Rust
            | Self::Go
            | Self::Php
            | Self::Config
            | Self::Terraform
            | Self::Dockerfile
            | Self::Shell
            | Self::Swift
            | Self::Sql => Group::Family(self),
        }
    }
}

/// The files whose uses can name a definition: its interaction group. Membership
/// is by extension, so the languages no cell reads (Scala, Groovy, Objective-C,
/// Vue, Svelte, HTML, Astro, Swift, SQL, ...) are in their group and are
/// accounted for like any file of it, never left out of scope.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Group {
    Family(Family),
    /// Java, Kotlin, Scala and Groovy: they call each other.
    Jvm,
    /// C, C++ and Objective-C: they bridge and link.
    Native,
    /// The scripts of the web and the markup and components that embed them.
    Web,
    /// A language no family reads, alone in its group (its name as the language
    /// table spells it).
    Language(&'static str),
}

impl Group {
    /// The group a file belongs to by its extension; none for a file of no known
    /// language.
    pub fn of_path(path: &str) -> Option<Self> {
        if let Ok(repo) = RepoPath::new(path) {
            if let Some(language) = Language::from_path(&repo) {
                return Family::ALL
                    .into_iter()
                    .find(|family| family.has(language))
                    .map(Family::group);
            }
        }
        if path.to_ascii_lowercase().ends_with(".astro") {
            return Some(Self::Web);
        }
        crate::languages::get_language_for_file(path).map(|language| match language {
            "java" | "kotlin" | "scala" | "groovy" => Self::Jvm,
            "c" | "cpp" | "objectivec" => Self::Native,
            "typescript" | "tsx" | "javascript" | "jsx" | "html" | "vue" | "svelte" => Self::Web,
            other => Self::Language(other),
        })
    }

    pub fn has_path(self, path: &str) -> bool {
        Self::of_path(path) == Some(self)
    }
}

/// The resolver of a family, behind the one set of questions a cell asks.
pub(crate) enum Resolver {
    Python(PythonResolver),
    TypeScript(TypeScriptResolver),
    Rust(RustResolver),
    Kotlin(KotlinResolver),
    Java(JavaResolver),
    Go(GoResolver),
    Php(PhpResolver),
    C(CResolver),
    Config(ConfigResolver),
    Terraform(TerraformResolver),
    Dockerfile(DockerfileResolver),
    Shell(ShellResolver),
    Sql(SqlResolver),
    Swift(SwiftResolver),
}

impl Resolver {
    pub(crate) fn open(
        family: Family,
        ctx: &AnalysisContext,
        side: SideTag,
        cache: &FactsCache,
    ) -> Result<Self> {
        let _open = crate::profile::detail(
            match side {
                SideTag::Head => "resolver.open.head",
                SideTag::Base => "resolver.open.base",
            },
            family.name(),
        );
        Ok(match family {
            Family::Python => Self::Python(PythonResolver::open(ctx, side, cache)?),
            Family::TypeScript => Self::TypeScript(TypeScriptResolver::open(ctx, side, cache)?),
            Family::Rust => Self::Rust(RustResolver::open(ctx, side, cache)?),
            Family::Kotlin => Self::Kotlin(KotlinResolver::open(ctx, side, cache)?),
            Family::Java => Self::Java(JavaResolver::open(ctx, side, cache)?),
            Family::Go => Self::Go(GoResolver::open(ctx, side, cache)?),
            Family::Php => Self::Php(PhpResolver::open(ctx, side, cache)?),
            Family::C => Self::C(CResolver::open(ctx, side, cache)?),
            Family::Config => Self::Config(ConfigResolver::open(ctx, side, cache)?),
            Family::Terraform => Self::Terraform(TerraformResolver::open(ctx, side, cache)?),
            Family::Dockerfile => Self::Dockerfile(DockerfileResolver::open(ctx, side, cache)?),
            Family::Shell => Self::Shell(ShellResolver::open(ctx, side, cache)?),
            Family::Swift => Self::Swift(SwiftResolver::open(ctx, side, cache)?),
            Family::Sql => Self::Sql(SqlResolver::open(ctx, side, cache)?),
        })
    }

    pub(crate) fn bindings(&self) -> &[Binding] {
        match self {
            Self::Python(r) => r.bindings(),
            Self::TypeScript(r) => r.bindings(),
            Self::Rust(r) => r.bindings(),
            Self::Kotlin(r) => r.bindings(),
            Self::Java(r) => r.bindings(),
            Self::Go(r) => r.bindings(),
            Self::Php(r) => r.bindings(),
            Self::C(r) => r.bindings(),
            Self::Config(r) => r.bindings(),
            Self::Terraform(r) => r.bindings(),
            Self::Dockerfile(r) => r.bindings(),
            Self::Shell(r) => r.bindings(),
            Self::Sql(r) => r.bindings(),
            Self::Swift(r) => r.bindings(),
        }
    }

    /// The modules a name names, for the families that import modules by name.
    pub(crate) fn modules_named(&self, dotted: &str) -> Vec<RepoPath> {
        match self {
            Self::Python(r) => r.modules_named(dotted),
            _ => Vec::new(),
        }
    }

    pub(crate) fn definitions(&self) -> &[Definition] {
        match self {
            Self::Python(r) => r.definitions(),
            Self::TypeScript(r) => r.definitions(),
            Self::Rust(r) => r.definitions(),
            Self::Kotlin(r) => r.definitions(),
            Self::Java(r) => r.definitions(),
            Self::Go(r) => r.definitions(),
            Self::Php(r) => r.definitions(),
            Self::C(r) => r.definitions(),
            Self::Config(r) => r.definitions(),
            Self::Terraform(r) => r.definitions(),
            Self::Dockerfile(r) => r.definitions(),
            Self::Shell(r) => r.definitions(),
            Self::Sql(r) => r.definitions(),
            Self::Swift(r) => r.definitions(),
        }
    }

    pub(crate) fn references_to(&self, key: &DefKey) -> Vec<&Binding> {
        match self {
            Self::Python(r) => r.references_to(key),
            Self::TypeScript(r) => r.references_to(key),
            Self::Rust(r) => r.references_to(key),
            Self::Kotlin(r) => r.references_to(key),
            Self::Java(r) => r.references_to(key),
            Self::Go(r) => r.references_to(key),
            Self::Php(r) => r.references_to(key),
            Self::C(r) => r.references_to(key),
            Self::Config(r) => r.references_to(key),
            Self::Terraform(r) => r.references_to(key),
            Self::Dockerfile(r) => r.references_to(key),
            Self::Shell(r) => r.references_to(key),
            Self::Sql(r) => r.references_to(key),
            Self::Swift(r) => r.references_to(key),
        }
    }

    /// Whether a removal in `file` is a change of what the project exports. Rust
    /// exports only through modules declared `pub` from a library root, Go through
    /// packages other than `main` and those under `internal/`; a file
    /// outside every crate (no manifest names it) and one the head no longer
    /// holds are taken to have exported.
    pub(crate) fn exports(&self, file: &str, workspace: &WorkspaceModel) -> bool {
        match self {
            Self::Rust(r) => {
                r.is_public_module(file)
                    || workspace.rust_module(file).is_none()
                    || !r.definitions().iter().any(|d| d.file.as_str() == file)
            }
            Self::Go(r) => {
                r.is_public_package(file)
                    || !r.definitions().iter().any(|d| d.file.as_str() == file)
            }
            _ => true,
        }
    }

    /// The files the resolver read.
    pub(crate) fn read_paths(&self) -> &[RepoPath] {
        match self {
            Self::Python(r) => r.read_paths(),
            Self::TypeScript(r) => r.read_paths(),
            Self::Rust(r) => r.read_paths(),
            Self::Kotlin(r) => r.read_paths(),
            Self::Java(r) => r.read_paths(),
            Self::Go(r) => r.read_paths(),
            Self::Php(r) => r.read_paths(),
            Self::C(r) => r.read_paths(),
            Self::Config(r) => r.read_paths(),
            Self::Terraform(r) => r.read_paths(),
            Self::Dockerfile(r) => r.read_paths(),
            Self::Shell(r) => r.read_paths(),
            Self::Sql(r) => r.read_paths(),
            Self::Swift(r) => r.read_paths(),
        }
    }

    pub(crate) fn losses(&self) -> &[Loss] {
        match self {
            Self::Python(r) => r.losses(),
            Self::TypeScript(r) => r.losses(),
            Self::Rust(r) => r.losses(),
            Self::Kotlin(r) => r.losses(),
            Self::Java(r) => r.losses(),
            Self::Go(r) => r.losses(),
            Self::Php(r) => r.losses(),
            Self::C(r) => r.losses(),
            Self::Config(r) => r.losses(),
            Self::Terraform(r) => r.losses(),
            Self::Dockerfile(r) => r.losses(),
            Self::Shell(r) => r.losses(),
            Self::Sql(r) => r.losses(),
            Self::Swift(r) => r.losses(),
        }
    }

    /// The module calls that name a source outside the repository.
    pub(crate) fn external_sources(&self) -> usize {
        match self {
            Self::Terraform(r) => r.external_sources(),
            _ => 0,
        }
    }
}

/// `context` for a reader that holds no `ScoredState`: the root and the run's
/// I/O limits.
pub(crate) fn context_in(
    root: &std::path::Path,
    limits: (
        Option<std::time::Instant>,
        Arc<std::sync::atomic::AtomicBool>,
    ),
    range: Option<&str>,
    symbol: bool,
) -> Result<AnalysisContext> {
    let repo = snapshot::open(root)?;
    let spec = if symbol {
        RangeSpec::At(Side::WorkingTree)
    } else {
        match snapshot::resolve(&repo, range.map_or("HEAD", str::trim))? {
            RangeSpec::Staged { base } => RangeSpec::Review {
                base,
                head: snapshot::index_tree(&repo)?,
            },
            other => other,
        }
    };
    let (deadline, cancel) = limits;
    let budget = RunBudget {
        deadline,
        cancel: Some(cancel),
        read_fault: crate::resource::test_read_fault(),
    };
    // The cap the engine reads a changed file under: a file past it is listed,
    // not read, here as in the fragments.
    let admission = Admission::new(AdmissionPolicy {
        size_cap: crate::config::limits::LIMITS.max_changed_file_size as u64,
        ..AdmissionPolicy::default()
    });
    Ok(AnalysisContext::with_admission(
        repo,
        spec,
        budget,
        Arc::new(admission),
    )?)
}

/// What the ranking graph reads of one family's resolved head snapshot: the
/// bindings and the definitions, as plain data. The resolver itself stays with
/// the cell (the TypeScript one is not `Send`, so a run cannot hold one for
/// every thread that builds edges).
pub(crate) struct HeadFamily {
    bindings: Vec<Binding>,
    definitions: Vec<Definition>,
}

impl HeadFamily {
    pub(crate) fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub(crate) fn definitions(&self) -> &[Definition] {
        &self.definitions
    }
}

/// `family` resolved over the head snapshot of the run at `root`: the range the
/// run analysed, `HEAD` and the working tree for a run with none. Opened once
/// per run and family however many edge builders and discovery ask; the run's
/// I/O deadline and cancellation are the current run's. A family that cannot be
/// read is an `Err`, never kept: the caller notes it and draws no edge.
pub(crate) fn open_head(
    root: &std::path::Path,
    range: Option<&str>,
    family: Family,
) -> Result<Arc<HeadFamily>> {
    let key = std::path::PathBuf::from(format!("{}@{}", root.display(), range.unwrap_or("")));
    crate::resource::memoized(family.name(), &key, || -> Result<Arc<HeadFamily>> {
        let limits = crate::resource::current_io_limits()
            .unwrap_or_else(|| (None, Arc::new(std::sync::atomic::AtomicBool::new(false))));
        let ctx = context_in(root, limits, range, false)?;
        let cache = FactsCache::user();
        let resolver = Resolver::open(family, &ctx, SideTag::Head, &cache)?;
        Ok(Arc::new(HeadFamily {
            bindings: resolver.bindings().to_vec(),
            definitions: resolver.definitions().to_vec(),
        }))
    })
}
