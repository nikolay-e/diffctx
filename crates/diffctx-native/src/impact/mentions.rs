//! Path mentions: text that names a repository path, and the kind of place it
//! sits in.
//!
//! A mention is classified by where it is written, not by what it names:
//!
//! - `code`: a string or an identifier of a source file;
//! - `config`: a value of a structured config (YAML, JSON, TOML, INI, `.env`);
//! - `command`: a line that starts a program: a shell script, a `Dockerfile`, a
//!   `Makefile`, a CI step's `run:`, a `subprocess` call;
//! - `prose`: a document, a patch, a comment, a sentence in a string;
//! - `data`: rows of a table or a log, and files no tool reads by extension;
//! - `ignore_list`: an ignore or attribute file, which selects paths and reads
//!   none of them.
//!
//! The first three read the path (`Class::reads`): the change breaks them. The
//! others only talk about it. The family that reads a file settles a pointer it
//! holds from the file's own directory (`Cell::pointers_to`), and its answer
//! overrules the text's: a line the family resolved elsewhere names nothing here.

use std::path::Path;

use rustc_hash::{FxHashMap, FxHashSet};
use schemars::JsonSchema;
use serde::Serialize;

use super::{
    Cells, MAX_NAMED_BY, MAX_NAMED_BY_PER_TARGET, MAX_STALE_PER_TARGET, MAX_STALE_REFERENCES,
    PROJECT_MANIFESTS, PathReference, ScoredState, StaleReference, extension, is_code_file,
    is_document, read_or_note, rel,
};

/// The kind of place a path is mentioned in.
#[derive(Serialize, JsonSchema, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    /// A string or an identifier of a source file.
    Code,
    /// A value of a structured config.
    Config,
    /// A line that starts a program.
    Command,
    /// A document, a patch, a comment, a sentence.
    Prose,
    /// Rows of data, a log, a file no tool reads.
    Data,
    /// An ignore or attribute file: it selects paths and reads none.
    IgnoreList,
}

impl Class {
    /// Whether a change to the named file breaks the mention.
    pub(super) const fn reads(self) -> bool {
        matches!(self, Self::Code | Self::Config | Self::Command)
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Code => "code",
            Self::Config => "config",
            Self::Command => "command",
            Self::Prose => "prose",
            Self::Data => "data",
            Self::IgnoreList => "ignore_list",
        }
    }
}

/// The place `text` (a line of `path`, or none: the place alone) sits in. `token`
/// is the mention itself, which tells a sentence about a path from a value that
/// is one.
pub(super) fn class_of(
    root_dir: &Path,
    source: &crate::source::Source,
    path: &str,
    text: &str,
    token: Option<&str>,
) -> Class {
    if selects_paths(path) {
        Class::IgnoreList
    } else if is_data(root_dir, source, path) {
        Class::Data
    } else if is_document(path)
        || matches!(extension(path).as_str(), "diff" | "patch")
        || commented(text)
        || prints_a_name(text)
        || (!is_code_file(path) && token.is_some_and(|t| is_prose_about(text, t)))
    {
        Class::Prose
    } else if on_a_command_line(path, text) {
        Class::Command
    } else if is_code_file(path) {
        Class::Code
    } else {
        Class::Config
    }
}

/// A line that prints words (`echo lib.sh is mentioned`) names the path to the
/// reader of the output, not to the shell; a redirect or a pipe makes it a use.
fn prints_a_name(text: &str) -> bool {
    let line = text.trim_start();
    (line.starts_with("echo ") || line.starts_with("printf ")) && !line.contains(['>', '|'])
}

/// Words a line that starts a program holds: a line that spells a path without
/// one of them only talks about it.
const RUNNERS: &[&str] = &[
    "run",
    "bash",
    "sh",
    "exec",
    "execa",
    "spawn",
    "subprocess",
    "popen",
    "system",
    "check_output",
    "check_call",
    "call",
    "node",
    "python",
    "python3",
    "ruby",
    "perl",
    "source",
    "invoke",
    "which",
    "execsync",
    "execfilesync",
    "spawnsync",
];

/// The extensions of files a command line starts by their path: a piece that
/// ends in one is a path, not a word of the line (`gen.sh` holds no `sh`).
const SCRIPT_EXTENSIONS: &[&str] = &[
    "sh", "bash", "zsh", "py", "js", "mjs", "cjs", "rb", "pl", "ts",
];

pub(super) fn starts_a_program(text: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-')))
        .map(str::to_lowercase)
        .any(|piece| {
            if RUNNERS.contains(&piece.as_str()) {
                return true;
            }
            let path_like = piece.contains('/')
                || piece
                    .rsplit_once('.')
                    .is_some_and(|(_, ext)| SCRIPT_EXTENSIONS.contains(&ext));
            !path_like && piece.split('.').any(|word| RUNNERS.contains(&word))
        })
}

/// The keys of a config whose value is a command line.
const COMMAND_KEYS: &[&str] = &[
    "run:",
    "script:",
    "command:",
    "commands:",
    "entrypoint:",
    "cmd:",
    "exec:",
];

fn on_a_command_line(path: &str, text: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let shell_like = matches!(
        extension(path).as_str(),
        "sh" | "bash" | "zsh" | "ksh" | "mk"
    ) || name.starts_with("dockerfile")
        || name.starts_with("containerfile")
        || matches!(name.as_str(), "makefile" | "gnumakefile" | "justfile");
    let line = text.trim_start().trim_start_matches("- ");
    shell_like
        || COMMAND_KEYS.iter().any(|key| line.starts_with(key))
        || (is_code_file(path) && starts_a_program(text))
}

/// How a line can name a path: the strongest first, so a line naming two
/// targets is attributed to the one it names most exactly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Naming {
    FullPath,
    RelativePath,
    Module,
    UniqueBasename,
}

fn normalize_rel(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    parts.join("/")
}

fn without_extension(path: &str) -> &str {
    match path.rfind('.') {
        Some(dot) if !path[dot..].contains('/') && dot > path.rfind('/').map_or(0, |s| s + 1) => {
            &path[..dot]
        }
        _ => path,
    }
}

/// How `token`, found in `referrer`, names `target` — or `None` when it is a
/// different name that merely shares a basename or a word (#369, #383).
pub(super) fn naming(
    token: &str,
    referrer: &str,
    target: &str,
    unique_basename: bool,
    root: &str,
) -> Option<Naming> {
    let trimmed = token.trim_start_matches("./");
    // A file at the root is spelled by its bare name, which names any file
    // of that name: `svc/tests` reading `"AGENTS.md"` reads `svc/AGENTS.md`
    // (#644), and `"/../AGENTS.md"` is a probe string, not the root's.
    // Behind a prefix it is the root's only from the repository's own path
    // or one mount point or variable: `$HOME/other/README.md` and
    // `/home/x/elsewhere/README.md` are other trees' files (#673).
    let anchored = || {
        token
            .strip_suffix(target)
            .and_then(|p| p.strip_suffix('/'))
            .is_some_and(|prefix| {
                target.contains('/')
                    || prefix == root
                    || (!prefix.trim_start_matches('/').contains('/')
                        && !matches!(prefix, "~" | "$HOME" | "${HOME}"))
            })
    };
    if (target.contains('/') || unique_basename) && (trimmed == target || anchored()) {
        return Some(Naming::FullPath);
    }
    let dir = referrer.rsplit_once('/').map_or("", |(d, _)| d);
    if token.contains('/') {
        let joined = normalize_rel(&format!("{dir}/{token}"));
        if joined == target || joined == without_extension(target) {
            return Some(Naming::RelativePath);
        }
    }
    // A Python relative import: `.probes` is the sibling module, each
    // further dot one package up.
    let dots = token.chars().take_while(|c| *c == '.').count();
    if dots > 0 && !token.contains('/') {
        let rest = &token[dots..];
        let mut base = dir.to_string();
        for _ in 1..dots {
            base = base
                .rsplit_once('/')
                .map_or(String::new(), |(d, _)| d.to_string());
        }
        let module = normalize_rel(&format!("{base}/{}", rest.replace('.', "/")));
        let target_module = without_extension(target);
        if !rest.is_empty()
            && (module == target_module || format!("{module}/__init__") == target_module)
        {
            return Some(Naming::RelativePath);
        }
    }
    // `pkg.probes` / `pkg/probes`: a module path of at least two segments
    // that the target's path ends with.
    let module = without_extension(target);
    let segments: Vec<&str> = module.split('/').collect();
    for take in 2..=segments.len() {
        let tail = &segments[segments.len() - take..];
        let dotted = tail.join(".");
        let slashed = tail.join("/");
        if token == dotted || token.starts_with(&format!("{dotted}.")) || trimmed == slashed {
            return Some(Naming::Module);
        }
    }
    let basename = target.rsplit('/').next().unwrap_or(target);
    (unique_basename && basename.contains('.') && trimmed == basename)
        .then_some(Naming::UniqueBasename)
}

/// What still names a path the change deleted or renamed, in the tree the
/// change leaves behind: the range's head, or the working tree.
pub(super) fn stale_references(
    state: &ScoredState,
    diff_range: Option<&str>,
) -> (Vec<StaleReference>, usize) {
    let mut targets: Vec<(String, String)> = state
        .deleted_files
        .iter()
        .map(|d| (d.clone(), "deleted".to_string()))
        .collect();
    targets.extend(
        state
            .renamed_files
            .iter()
            .map(|(old, new)| (old.clone(), format!("renamed to {new}"))),
    );
    if targets.is_empty() {
        return (Vec::new(), 0);
    }
    let root = state.root_dir.to_string_lossy().replace('\\', "/");
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let surviving: FxHashSet<String> =
        read_or_note(state, state.run.source().list_paths(&state.root_dir))
            .into_iter()
            .filter_map(|p| p.rsplit('/').next().map(str::to_string))
            .collect();
    let mut needles: Vec<String> = Vec::new();
    for (target, _) in &targets {
        needles.push(target.clone());
        if let Some(name) = Path::new(target).file_name().and_then(|n| n.to_str()) {
            needles.push(name.to_string());
        }
        // A module is named without its extension (`from pkg.probes import`).
        if is_code_file(target) {
            needles.extend(
                Path::new(target)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(str::to_string),
            );
        }
    }
    needles.sort();
    needles.dedup();
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    state.run.note_withheld(grepped.withheld);
    let lines = grepped.rows;
    let source = state.run.source();
    let renamed_to: FxHashSet<&str> = state
        .renamed_files
        .iter()
        .map(|(_, new)| new.as_str())
        .collect();
    let token_re = once_cell::sync::Lazy::force(&PATH_TOKEN);
    let mut found = Vec::new();
    let mut per_target: FxHashMap<&str, usize> = FxHashMap::default();
    let mut omitted = 0;
    // A line this change wrote about the deletion is its author's record of
    // it — a changelog entry, a note, a comment — not a reference it missed;
    // a code line it wrote still is one (#456).
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    for (path, line, text) in state.run.bounded(&lines) {
        if renamed_to.contains(path.as_str()) {
            continue;
        }
        if written.contains(&(path.as_str(), text.trim()))
            && (!is_code_file(path) || commented(text))
        {
            continue;
        }
        let surviving = &surviving;
        let targets = &targets;
        let root = root.as_str();
        let best = token_re
            .find_iter(text)
            .flat_map(|m| {
                targets
                    .iter()
                    .filter(|(t, _)| t != path)
                    .filter_map(move |(t, kind)| {
                        let basename = t.rsplit('/').next().unwrap_or(t);
                        // A path that ends a sentence carries its full stop.
                        let token = m.as_str().trim_end_matches('.');
                        naming(token, path, t, !surviving.contains(basename), root)
                            .map(|n| (n, std::cmp::Reverse(t.len()), t, kind))
                    })
            })
            .min();
        let Some((_, _, target, kind)) = best else {
            continue;
        };
        let count = per_target.entry(target.as_str()).or_default();
        if *count >= MAX_STALE_PER_TARGET || found.len() >= MAX_STALE_REFERENCES {
            omitted += 1;
            continue;
        }
        *count += 1;
        found.push(StaleReference {
            path: path.clone(),
            line: *line,
            target: target.clone(),
            kind: kind.clone(),
            class: class_of(&state.root_dir, &source, path, text, None),
        });
    }
    (found, omitted)
}

/// An `import`, `from … import` or `export … from` line.
pub(super) fn is_import_line(code: &str) -> bool {
    let head = code.trim_start();
    head.starts_with("import ")
        || head.starts_with("from ")
        || (head.starts_with("export ") && head.contains(" from "))
}

/// Whether the resolver binds a file's imports: a Python or a JavaScript,
/// TypeScript or HTML file.
pub(super) fn reads_imports(path: &str) -> bool {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "py" | "pyi"
                    | "js"
                    | "jsx"
                    | "ts"
                    | "tsx"
                    | "mjs"
                    | "cjs"
                    | "mts"
                    | "cts"
                    | "html"
                    | "htm"
            )
        })
}

/// `--flag path` in a shell line and `*p` in C are code, not comments.
pub(super) fn commented(text: &str) -> bool {
    let t = text.trim_start();
    ["#", "//", "/*", "* "].iter().any(|m| t.starts_with(m)) || t == "*"
}

/// An ignore or attribute file lists paths to include or exclude them; it
/// reads none of their content (#682).
pub(super) fn selects_paths(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    (name.starts_with('.') && name.ends_with("ignore"))
        || name == ".gitattributes"
        || path.ends_with(".diffctx/ignore")
}

/// pip reads `requirements.txt`, `requirements-dev.txt`, `dev-requirements.txt`,
/// `requirements/base.txt` and the `constraints` kin by their names.
fn is_pip_manifest(path: &str) -> bool {
    let lower = path.to_lowercase();
    ["requirements", "constraints"]
        .iter()
        .any(|word| lower.contains(word))
}

/// Rows of data, logs and notes: an extension that says nothing reads them.
/// A `.txt` is one unless a tool reads it by its name: the language table
/// knows `CMakeLists.txt`, and pip's manifests are named by `is_pip_manifest`.
pub(super) fn has_data_extension(path: &str) -> bool {
    match extension(path).as_str() {
        "csv" | "tsv" | "log" | "jsonl" | "ndjson" => true,
        "txt" => {
            let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
            !crate::languages::FILENAME_TO_LANGUAGE.contains_key(name.as_str())
                && !is_pip_manifest(path)
        }
        _ => false,
    }
}

/// Rows of data and transcripts mention paths; nothing runs them (#673).
/// An extensionless file is data unless its name or shebang says what runs
/// it — as a referrer; a changed one is a config or a manifest with readers
/// until proven otherwise (`Procfile`, `CODEOWNERS`) and goes by extension
/// alone (#123).
pub(super) fn is_data(root_dir: &Path, source: &crate::source::Source, path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    if !name.contains('.') {
        return crate::languages::get_language_for_file(path).is_none()
            && !source
                .read_to_string(&root_dir.join(path))
                .is_some_and(|t| t.starts_with("#!"));
    }
    has_data_extension(path)
}

/// A structured config: its keys and values are read by a program.
pub(super) fn is_config_file(path: &str) -> bool {
    matches!(
        extension(path).as_str(),
        "yaml" | "yml" | "json" | "toml" | "ini" | "env" | "properties" | "conf" | "cfg"
    )
}

/// Code, a config, or a file with no extension (a Dockerfile, a Makefile, a
/// script): where a path is spelled to be used. Anything else that survives
/// the filters (a page, a template) only links to it.
fn reads_paths(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    is_code_file(path) || is_config_file(path) || !name.contains('.')
}

/// Words that join a sentence and never a command or a value.
const FUNCTION_WORDS: &[&str] = &[
    "the", "an", "is", "are", "was", "were", "be", "in", "on", "of", "to", "for", "with", "and",
    "or", "see", "this", "that", "it", "as", "at", "by", "from", "not", "you", "we", "if", "then",
    "but", "also", "please",
];

/// A sentence that spells a path talks about the file; it does not read it:
/// `"setup steps are in README.md"` in a JSON dump or a `description:` value
/// (#673). A command line or a bare value has too few plain words around the
/// path, and one with shell syntax is never prose.
fn is_prose_about(text: &str, token: &str) -> bool {
    let rest = text.replacen(token, " ", 1);
    if rest.contains([';', '|', '&', '$', '<', '>', '`']) || rest.contains(" -") {
        return false;
    }
    let plain: Vec<String> = rest
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|w| w.len() >= 2 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .collect();
    plain.len() >= 3 && plain.iter().any(|w| FUNCTION_WORDS.contains(&w.as_str()))
}

/// The paths a file says it was generated from in its first lines
/// (`# GENERATED from src/dash.jsonnet - do not edit`), spelled from the
/// repository root and from the file's own directory.
fn generated_from(source: &crate::source::Source, root_dir: &Path, path: &str) -> Vec<String> {
    static MARKER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"(?i)\bgenerated\s+from\s+([A-Za-z0-9_./@+-]+)").expect("marker regex")
    });
    let Some(text) = source.read_to_string(&root_dir.join(path)) else {
        return Vec::new();
    };
    let head: String = text.lines().take(5).collect::<Vec<_>>().join("\n");
    let Some(from) = MARKER.captures(&head).and_then(|c| c.get(1)) else {
        return Vec::new();
    };
    let from = from.as_str().trim_end_matches('.');
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    vec![normalize_rel(from), normalize_rel(&format!("{dir}/{from}"))]
}

/// Lines outside the change that name a changed file by its path, from a
/// file of another runtime or a config: the command line that runs it, the
/// Dockerfile that copies it, the settings entry that runs a script (#393,
/// #523). An import is a caller, which the binding model answers; a
/// document or a comment only mentions the file.
pub(super) fn named_by_path(
    state: &ScoredState,
    cells: &Cells,
    diff_range: Option<&str>,
) -> (Vec<PathReference>, usize) {
    // Every build step names a project manifest; that it changed is said
    // where dependencies are.
    let targets: Vec<String> = state
        .changed_files
        .iter()
        .map(|p| rel(state, &p.to_string_lossy()))
        .filter(|t| !PROJECT_MANIFESTS.contains(&t.rsplit('/').next().unwrap_or(t)))
        .collect();
    if targets.is_empty() {
        return (Vec::new(), 0);
    }
    let root = state.root_dir.to_string_lossy().replace('\\', "/");
    let head = diff_range
        .map(crate::snapshot::split_diff_range)
        .and_then(|(_, head)| head);
    let mut basenames: FxHashMap<String, usize> = FxHashMap::default();
    for p in read_or_note(state, state.run.source().list_paths(&state.root_dir)) {
        if let Some(b) = p.rsplit('/').next() {
            *basenames.entry(b.to_string()).or_default() += 1;
        }
    }
    let basename_of = |t: &str| t.rsplit('/').next().unwrap_or(t).to_string();
    let mut needles: Vec<String> = targets
        .iter()
        .flat_map(|t| [t.clone(), basename_of(t)])
        .collect();
    needles.sort();
    needles.dedup();
    // A file the ignore rules withhold is not searched here; the header's
    // withheld count stays the one the answer's own reads give.
    let grepped = read_or_note(
        state,
        crate::grep::grep_lines(
            &state.root_dir,
            &state.run.source(),
            &needles,
            head.as_deref(),
        ),
    );
    // What this change wrote is the diff itself.
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    let token_re = once_cell::sync::Lazy::force(&PATH_TOKEN);
    let source = state.run.source();
    let mut candidates: Vec<PathReference> = Vec::new();
    for (path, line, text) in state.run.bounded(&grepped.rows) {
        // A document or a patch mentions paths without consuming them. A
        // document importing another (`@AGENTS.md`) does load it, but says
        // so on every edit of it, a constant line that teaches the reader to
        // skip the block (#563, #564).
        if !class_of(&state.root_dir, &source, path, text, None).reads()
            || is_import_line(text)
            || written.contains(&(path.as_str(), text.trim()))
        {
            continue;
        }
        let hit = token_re.find_iter(text).find_map(|m| {
            let token = m.as_str().trim_end_matches('.');
            targets
                .iter()
                .find(|t| {
                    let basename = basename_of(t);
                    *t != path
                        && token.ends_with(basename.as_str())
                        && matches!(
                            naming(token, path, t, basenames.get(&basename) == Some(&1), &root),
                            Some(Naming::FullPath | Naming::RelativePath | Naming::UniqueBasename)
                        )
                })
                .map(|t| (t, token))
        });
        let Some((target, token)) = hit else {
            continue;
        };
        // A sentence in a config's string (a dump, a description) is not a
        // reader; code is judged by its comments above.
        let class = class_of(&state.root_dir, &source, path, text, Some(token));
        if !class.reads() {
            continue;
        }
        // The family that reads the file settled this very line on another file:
        // `COPY other/run.sh` is no mention of `scripts/run.sh` whatever the
        // basenames say.
        if cells
            .of(path)
            .is_some_and(|cell| cell.points_elsewhere(path, *line, target))
        {
            continue;
        }
        candidates.push(PathReference {
            path: path.clone(),
            line: *line,
            target: target.clone(),
            key: None,
            class,
        });
    }
    // What a family's resolver settled on the target by its path, from the
    // directory the pointer is written in, which the text search spells wrong
    // (`../scripts/run.sh`, a Kustomize `resources` entry, a Terraform `source`).
    for target in &targets {
        for (path, line) in cells.iter().flat_map(|cell| cell.pointers_to(target)) {
            let written_here = state.hunk_texts.iter().any(|h| {
                let (first, last) = h.new_range();
                h.path == path && first <= line && line <= last && !h.added.is_empty()
            });
            if path == *target
                || written_here
                || candidates
                    .iter()
                    .any(|r| r.path == path && r.line == line && r.target == *target)
            {
                continue;
            }
            let class = class_of(&state.root_dir, &source, &path, "", None);
            if class.reads() {
                candidates.push(PathReference {
                    path,
                    line,
                    target: target.clone(),
                    key: None,
                    class,
                });
            }
        }
    }
    // A generated file is its source's output: when the source names the
    // file too, the source is the consumer to open (#110).
    let spelled: FxHashSet<String> = candidates.iter().map(|r| r.path.clone()).collect();
    let source = state.run.source();
    let mut derived: FxHashMap<String, bool> = FxHashMap::default();
    candidates.retain(|r| {
        let folded = *derived.entry(r.path.clone()).or_insert_with(|| {
            generated_from(&source, &state.root_dir, &r.path)
                .iter()
                .any(|from| *from != r.path && spelled.contains(from))
        });
        !folded
    });
    // Code and config consumers come before pages and templates, so the cap
    // drops the latter first (#673).
    candidates.sort_by_key(|r| !reads_paths(&r.path));
    let mut found: Vec<PathReference> = Vec::new();
    let mut per_target: FxHashMap<String, usize> = FxHashMap::default();
    let mut omitted = 0;
    for r in candidates {
        let count = per_target.entry(r.target.clone()).or_default();
        if *count >= MAX_NAMED_BY_PER_TARGET || found.len() >= MAX_NAMED_BY {
            omitted += 1;
            continue;
        }
        *count += 1;
        found.push(r);
    }
    (found, omitted)
}

/// A run of characters a path or a module name is spelled with.
pub(super) static PATH_TOKEN: once_cell::sync::Lazy<regex::Regex> =
    once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"[A-Za-z0-9_./@~$+-]*[A-Za-z0-9_$][A-Za-z0-9_./@~$+-]*")
            .expect("path token regex")
    });
