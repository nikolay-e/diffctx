//! Whether a fragment names a changed definition: local bindings, receivers, module aliases and the
//! imports that reach a module or a package.

use std::path::Path;

use rustc_hash::FxHashMap;

use crate::decide::cells::Cells;

use crate::run_provenance::state::ScoredState;

use diffctx_base::types::{Fragment, FragmentKind};

use diffctx_facts::parsers::code_text;

use diffctx_facts::parsers::embedded::names_word;

use diffctx_occurrence::tokens::Names;

use super::changes::{rel, without_decorators};
use super::dispatch::enclosing_class;

/// Whether `code` binds `name` as a local — `let name`, `let mut name` — so
/// that its calls of `name` reach the local, not a definition elsewhere.
pub(super) fn binds_locally(code: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].trim_end();
        let head = before.strip_suffix("mut").map_or(before, str::trim_end);
        let Some(rest) = head.strip_suffix("let") else {
            return false;
        };
        !rest.chars().next_back().is_some_and(word)
            && !code[at + name.len()..].chars().next().is_some_and(word)
    })
}

/// The line of `f` that first spells `name` as a whole word.
pub(super) fn first_naming_line(f: &Fragment, name: &str) -> Option<u32> {
    f.content
        .lines()
        .position(|l| names_word(l, name))
        .map(|i| f.start_line() + i as u32)
}

/// Whether `line` holds a call of `name`: the whole word, then `(`.
pub(super) fn calls_word(line: &str, name: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    line.match_indices(name).any(|(at, _)| {
        !line[..at].chars().next_back().is_some_and(word)
            && line[at + name.len()..].starts_with('(')
    })
}

pub(super) fn is_code_file(path: &str) -> bool {
    let ext = diffctx_base::languages::file_ext(Path::new(path));
    diffctx_config::config::extensions::CODE_EXTENSIONS.contains(ext.as_str())
}

/// Who may stand before a `.` in front of the name and still mean it.
pub(super) enum Receiver<'a> {
    /// A method: any object can carry it.
    Any,
    /// A module-level definition: only its own module, by stem or by an
    /// alias the caller's file binds to it. `re.search` is not a call to a
    /// changed `download.search` (#345).
    Module {
        stem: &'a str,
        aliases: &'a [String],
    },
}

fn receiver_before(code: &str, at: usize) -> Option<&str> {
    let head = code[..at].strip_suffix('.')?;
    let head = head.strip_suffix('?').unwrap_or(head);
    let start = head
        .rfind(|c: char| !(c.is_alphanumeric() || c == '_'))
        .map_or(0, |i| i + 1);
    Some(&head[start..])
}

/// The caller must name the symbol itself, in code: the language builders
/// link a call by name to every definition of that name in the repository
/// (case-folded), so an edge alone says "same name somewhere", a comment
/// mention says nothing, and `Cli` in a doc line is not `Cli::parse()`.
pub(super) fn mentions(caller: &Fragment, symbol: Option<&str>, receiver: &Receiver) -> bool {
    let Some(sym) = symbol else { return true };
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    code_text(caller.path(), &caller.content)
        .lines()
        .any(|code| {
            let bytes = code.as_bytes();
            code.match_indices(sym).any(|(at, _)| {
                let before = at.checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(at + sym.len()).copied();
                if before.is_some_and(is_word) || after.is_some_and(is_word) {
                    return false;
                }
                match (receiver, receiver_before(&code, at)) {
                    (Receiver::Module { stem, aliases }, Some(r)) => {
                        r.eq_ignore_ascii_case(stem) || aliases.iter().any(|a| a == r)
                    }
                    _ => true,
                }
            })
        })
}

/// Names the caller's file binds the module to: `import a.stem as R`,
/// `from a import stem as R`, `import * as R from "./stem"`.
pub(super) fn module_aliases(file_fragments: &[&Fragment], stem: &str) -> Vec<String> {
    let mut aliases = Vec::new();
    for f in file_fragments {
        for line in f.content.lines() {
            let words: Vec<&str> = line
                .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '*'))
                .filter(|w| !w.is_empty())
                .collect();
            if !words.iter().any(|w| w.eq_ignore_ascii_case(stem)) {
                continue;
            }
            for pair in words.windows(2) {
                if pair[0] == "as" && pair[1] != "*" {
                    aliases.push(pair[1].to_string());
                }
            }
        }
    }
    aliases.sort();
    aliases.dedup();
    aliases
}

/// A method lives inside a type, or names its receiver in the declaration
/// (Kotlin `fun String.pad()`); anything else is reached through its module.
/// `file`: the fragments of the core's own file, in fragment order.
pub(crate) fn is_member(core: &Fragment, file: &[&Fragment]) -> bool {
    if enclosing_class(core, file).is_some() {
        return true;
    }
    let head = without_decorators(&core.content).trim_start();
    let receiver = head
        .strip_prefix("fun ")
        .is_some_and(|rest| rest.split('(').next().is_some_and(|n| n.contains('.')));
    receiver
        || file.iter().any(|f| {
            f.path() == core.path()
                && f.id != core.id
                && (f.kind.is_container() || f.kind == FragmentKind::Impl)
                && f.start_line() <= core.start_line()
                && f.end_line() >= core.end_line()
        })
}

/// Languages where a directory is a namespace: a sibling file reaches the
/// changed symbol with no import to name it by. Rust, Python and JavaScript
/// siblings still name the module (`use`, `import`), so for them it must
/// appear among the caller file's identifiers.
const PACKAGE_SCOPED_EXTENSIONS: &[&str] = &[
    "java", "kt", "kts", "scala", "cs", "php", "c", "cc", "cpp", "cxx", "h", "hpp", "m", "mm",
    "swift", "dart",
];

/// What impact knows of each file in view: its fragments, for the import lines,
/// and, through the run's token index, what the whole file spells (an import sits
/// outside the calling fragment).
pub(super) struct Files<'a> {
    pub(super) fragments: FxHashMap<&'a str, Vec<&'a Fragment>>,
    pub(super) cells: &'a Cells,
    pub(super) state: &'a ScoredState,
}

impl Files<'_> {
    /// Whether the file spells one of `words` anywhere. A file the index did not
    /// read, or one of no group, is not known to spell none: it counts as
    /// spelling them.
    fn spells(&self, path: &str, words: &[&str]) -> bool {
        let shown = rel(self.state, path);
        let Some(group) = diffctx_resolve::vocabulary::family::Group::of_name(&shown) else {
            return true;
        };
        let Some(tokens) = self.cells.group_tokens(self.state, group) else {
            return true;
        };
        tokens
            .spells(&shown, &Names::new(words.iter().copied()))
            .unwrap_or(true)
    }
}

/// A caller that never names the changed file's module and lives in another
/// directory is a name match, not a call: `Command` in a binary crate is not
/// `std::process::Command`, however many files construct one. Same-directory
/// callers in a package-scoped language (Java, C#, Swift) pass, because a
/// package needs no import to reach its neighbours.
pub(super) fn references_module(
    caller: &Fragment,
    files: &Files,
    symbol: Option<&str>,
    changed_path: &str,
) -> bool {
    let changed = Path::new(changed_path);
    let caller_path = Path::new(caller.path());
    if caller_path == changed {
        return true;
    }
    // The import sits at the top of the file, outside the calling fragment.
    let ext = caller_path
        .extension()
        .map_or_else(String::new, |e| e.to_string_lossy().to_lowercase());
    if caller_path.parent() == changed.parent() && PACKAGE_SCOPED_EXTENSIONS.contains(&ext.as_str())
    {
        return true;
    }
    let Some(stem) = changed.file_stem().and_then(|s| s.to_str()) else {
        return true;
    };
    let stem = stem.to_lowercase();
    // A shell `lib.sh` is sourced by its own name, not its directory (#497).
    let sourced_by_name = changed.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_lowercase().as_str(),
            "sh" | "bash" | "zsh" | "ksh"
        )
    });
    if !sourced_by_name
        && matches!(
            stem.as_str(),
            "mod" | "lib" | "main" | "index" | "init" | "__init__"
        )
    {
        // A module named for its role (`mod.rs`, `index.ts`, `__init__.py`)
        // is imported by its directory, and a binary root is not imported.
        let dir = changed
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|d| d.to_str())
            .map(str::to_lowercase)
            .filter(|d| {
                !matches!(
                    d.as_str(),
                    "src" | "lib" | "app" | "bin" | "tests" | "test" | "pkg" | "internal"
                )
            });
        return dir.is_some_and(|d| files.spells(caller.path(), &[d.as_str()]));
    }
    files.spells(caller.path(), &[stem.as_str()])
        || symbol.is_some_and(|s| imported_from_package(changed, files, caller.path(), s))
}

/// `from models import Steps` reaches `models/wearables.py::Steps` through
/// the package that re-exports it: the caller names the package, never the
/// module (#351). Only an import line naming both the package and the symbol
/// counts — `models` alone is a common word. The resolver of #329 replaces
/// this.
fn imported_from_package(changed: &Path, files: &Files, caller_path: &str, symbol: &str) -> bool {
    let Some(dir) = changed
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|d| d.to_str())
    else {
        return false;
    };
    let has_word = |line: &str, word: &str| {
        line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| w == word)
    };
    files.fragments.get(caller_path).is_some_and(|frags| {
        frags.iter().flat_map(|f| f.content.lines()).any(|line| {
            let head = line.trim_start();
            ["from ", "import ", "use ", "export "]
                .iter()
                .any(|k| head.starts_with(k))
                && has_word(head, dir)
                && has_word(head, symbol)
        })
    })
}

/// A fragment that only imports is where a name comes in, not where it is
/// called: the module docstring and the import block of a file link to every
/// changed symbol the file uses and would be listed as callers with nothing
/// to check.
pub(super) fn is_import_block(content: &str) -> bool {
    let mut saw_code = false;
    let mut in_docstring = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if in_docstring {
            if line.contains("\"\"\"") || line.contains("'''") {
                in_docstring = false;
            }
            continue;
        }
        if line.starts_with("\"\"\"") || line.starts_with("'''") {
            let rest = &line[3..];
            if !rest.contains("\"\"\"") && !rest.contains("'''") {
                in_docstring = true;
            }
            continue;
        }
        if line.starts_with("//")
            || line.starts_with('#')
            || line.starts_with("/*")
            || line.starts_with('*')
        {
            continue;
        }
        let is_import = line.starts_with("import ")
            || line.starts_with("from ")
            || line.starts_with("use ")
            || line.starts_with("pub use ")
            || line.starts_with("require ")
            || line.starts_with("require(")
            || line.starts_with("const ") && line.contains("require(")
            || line.starts_with("export ") && line.contains(" from ")
            || line.starts_with("package ")
            || line.starts_with("using ")
            || line.starts_with("extern crate ")
            || line.starts_with("mod ")
            || line == ")"
            || line == "};"
            || line == "}"
            || line.ends_with(",") && !line.contains('(');
        if !is_import {
            saw_code = true;
            break;
        }
    }
    !saw_code
}

pub(super) fn extension(path: &str) -> String {
    path.rsplit_once('.')
        .map_or("", |(_, e)| e)
        .to_ascii_lowercase()
}

/// A file read for its prose, whose consumers find it by its path. Not
/// `.txt`: `requirements.txt` and `CMakeLists.txt` are read by tools.
pub(super) fn is_document(path: &str) -> bool {
    matches!(extension(path).as_str(), "md" | "markdown" | "rst" | "adoc")
}
