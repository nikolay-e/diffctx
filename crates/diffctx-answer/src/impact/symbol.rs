//! The `--symbol` answers: a symbol query parsed, a named symbol, a module or a local found.

use std::ops::Range;
use std::path::Path;

use std::time::Instant;

use crate::decide::{self};

use diffctx_facts::facts::{FileFacts, Local, Scope, Span};

use diffctx_occurrence::tokens::occurrence::{Sealed, account_local};

use diffctx_resolve::vocabulary;

use super::mentions::{is_config_file, is_data, selects_paths};

use super::IMPACT_TOKEN_CAP;
use super::callers::{is_code_file, is_document};
use super::output::{
    Caller, ChangedSymbol, ImpactOutput, NamedFile, SymbolNamed, UnanalysedFile, Unknown,
};

/// Names shown on the folded line before the rest is counted.
pub(super) const TOO_COMMON_NAMED: usize = 8;

/// A caller no static test link reaches although one could have: the one
/// predicate behind the header count, the folded count and the line (#386).
pub(crate) fn without_test_link(c: &Caller) -> bool {
    c.evidence == "resolved" && c.tests.is_empty() && c.registered_by.is_none() && !c.tests_open
}

/// A row that rests on a word match alone: listed as text, never counted or
/// rendered as a caller.
pub fn is_textual(c: &Caller) -> bool {
    c.evidence == "name"
}

/// What every row built from a word match is labelled with.
pub(crate) const TEXTUAL_MATCH: &str = "a textual match, not a resolved binding";

/// `NAME`, `PATH:NAME`, or a qualified name: `Class.method`, `Class::method`,
/// `module.func`, a Helm `chart.helper`. A Kotlin name may hold spaces and
/// punctuation between backticks (`` `N escapes MemberNames` ``); there `.` and
/// `:` belong to the name, and the segments come back without the backticks,
/// as the facts layer names the definition.
pub fn parse_symbol_query(query: &str) -> Option<(Option<&str>, Vec<&str>)> {
    // The path ends at the last lone `:` outside backticks; a `::` belongs to
    // the name (`shop.py:Ledger::total`).
    let bytes = query.as_bytes();
    let mut quoted = false;
    let mut lone = None;
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'`' => quoted = !quoted,
            b':' if !quoted
                && bytes.get(i + 1) != Some(&b':')
                && (i == 0 || bytes[i - 1] != b':') =>
            {
                lone = Some(i);
            }
            _ => {}
        }
    }
    if quoted {
        return None;
    }
    let (path, name) = match lone {
        Some(i) if i > 0 => (Some(&query[..i]), &query[i + 1..]),
        _ => (None, query),
    };
    let name_bytes = name.as_bytes();
    let mut segments = Vec::new();
    let (mut start, mut at) = (0, 0);
    quoted = false;
    while at < name_bytes.len() {
        let separator = match name_bytes[at] {
            b'`' => {
                quoted = !quoted;
                0
            }
            b'.' if !quoted => 1,
            b':' if !quoted && name_bytes.get(at + 1) == Some(&b':') => 2,
            _ => 0,
        };
        if separator > 0 {
            segments.push(&name[start..at]);
            at += separator;
            start = at;
        } else {
            at += 1;
        }
    }
    segments.push(&name[start..]);
    segments
        .into_iter()
        .map(segment_name)
        .collect::<Option<Vec<_>>>()
        .map(|segments| (path, segments))
}

/// What a segment names: the text between its backticks, or the segment itself
/// when it has none of them, no `:`, no control character and no space at either
/// end. An empty segment names nothing.
fn segment_name(segment: &str) -> Option<&str> {
    if let Some(inner) = segment
        .strip_prefix('`')
        .and_then(|rest| rest.strip_suffix('`'))
    {
        return (!inner.trim().is_empty()
            && !inner.contains('`')
            && !inner.chars().any(char::is_control))
        .then_some(inner);
    }
    (!segment.is_empty()
        && segment == segment.trim()
        && !segment.contains(['`', ':'])
        && !segment.chars().any(char::is_control))
    .then_some(segment)
}

/// Textual rows shown before they are counted.
pub(crate) const MAX_TEXTUAL_SHOWN: usize = 12;

/// The files that spell a name nothing defines, from one line each: not
/// documents, data, ignore files or lockfiles, which mention without
/// reading. Code and config come first (Forgejo #114).
pub fn symbol_named(
    root_dir: &Path,
    source: &diffctx_snapshot::source::Source,
    lines: Vec<(String, u32, String)>,
    unread: Vec<UnanalysedFile>,
    limits: Vec<diffctx_snapshot::resource::LimitReason>,
) -> Option<SymbolNamed> {
    let mut rows: Vec<NamedFile> = lines
        .into_iter()
        .filter(|(path, _, _)| {
            !is_document(path) && !selects_paths(path) && !is_data(root_dir, source, path)
        })
        .map(|(path, line, _)| NamedFile { path, line })
        .collect();
    if rows.is_empty() {
        return None;
    }
    rows.sort_by_key(|r| {
        (
            !(is_code_file(&r.path) || is_config_file(&r.path)),
            r.path.clone(),
        )
    });
    let files = rows.len();
    rows.truncate(MAX_TEXTUAL_SHOWN);
    Some(SymbolNamed {
        files,
        rows,
        unread,
        limits,
    })
}

/// A `--symbol` query that only text answers: the output carries the names
/// and nothing else.
pub fn symbol_named_output(root_dir: &Path, query: &str, named: SymbolNamed) -> ImpactOutput {
    // The files read as text are the run's files not analysed: the header says
    // which, and why.
    ImpactOutput {
        limits: named.limits.clone(),
        unanalysed_files: named.unread.clone(),
        symbol_named: Some(named),
        ..symbol_blocks_output(root_dir, query)
    }
}

pub(super) fn directory_name(root_dir: &Path) -> String {
    root_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| root_dir.to_string_lossy().to_string())
}

/// One use of a local, inside the definition it is a local of.
#[derive(Debug)]
pub struct LocalUse {
    /// The inner local or definition the use sits in.
    pub inside: Option<String>,
    pub line: u32,
    pub call: bool,
}

/// A `--symbol` name that is a local of a definition, and its uses there.
#[derive(Debug)]
pub struct LocalOf {
    pub path: String,
    pub name: String,
    /// `path::Definition`, or the file for module-level code.
    pub enclosing: String,
    pub line: u32,
    pub uses: Vec<LocalUse>,
    /// What the occurrence accounting of its body made of each relation of use.
    pub account: [Sealed; 6],
    /// The file looks a name up in its namespace (`locals()[key]`, `eval`, `exec`),
    /// which may be the local's.
    pub computed: bool,
}

/// The answer of a `--symbol` query that has blocks of its own to fill in, and no
/// search of named files.
fn symbol_blocks_output(root_dir: &Path, query: &str) -> ImpactOutput {
    ImpactOutput {
        name: directory_name(root_dir),
        symbol: Some(query.to_string()),
        cap: IMPACT_TOKEN_CAP,
        ..ImpactOutput::default()
    }
}

/// A `--symbol` query that names a module: the module is the definition and the
/// imports the resolver landed on it are its rows (gh#710). `None` when the name
/// is no module of the tree.
pub fn symbol_module_output(
    root_dir: &Path,
    query: &str,
    deadline: Instant,
) -> anyhow::Result<Option<ImpactOutput>> {
    let limits = (
        Some(deadline),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    );
    let modules = decide::cell::modules_named(root_dir, limits, query)?;
    if modules.is_empty() {
        return Ok(None);
    }
    let mut output = symbol_blocks_output(root_dir, query);
    output.changed = modules
        .into_iter()
        .map(|module| ChangedSymbol {
            callers: module
                .importers
                .into_iter()
                .map(|importer| Caller {
                    kind: vocabulary::stated_kind_of(
                        &importer.file,
                        vocabulary::RelationKind::Imports,
                    ),
                    ..Caller::at_line(
                        importer.file,
                        importer.inside,
                        format!("{0}-{0}", importer.line),
                        "resolved",
                        "reference",
                        importer.line,
                    )
                })
                .collect(),
            ..ChangedSymbol::bare(
                module.file,
                None,
                format!("1-{}", module.lines),
                "module",
                decide::coverage::Coverages::unowned(Vec::new()),
            )
        })
        .collect();
    Ok(Some(output))
}

/// A `--symbol` query that names only locals: one block each, never "no
/// definition found".
pub fn symbol_local_output(root_dir: &Path, query: &str, locals: Vec<LocalOf>) -> ImpactOutput {
    let mut output = symbol_blocks_output(root_dir, query);
    output.changed = local_blocks(locals);
    output
}

/// The sealed account of `local` over the body it lives in: the definition that encloses it, or the
/// whole file at module level. `calls`, `hidden` and `beyond` are what the scope reading found.
pub fn account_of_local(
    file: &FileFacts,
    text: &str,
    local: &Local,
    calls: &[Range<u32>],
    hidden: &[(Range<u32>, Span)],
    beyond: &[Range<u32>],
) -> [Sealed; 6] {
    let mut home = &local.enclosing;
    while let Scope::Callback { parent, .. } = home {
        home = parent;
    }
    let body = match home {
        Scope::Def(key) => file
            .blob
            .defs()
            .find(|d| d.local == *key)
            .map(|d| d.span.bytes.clone()),
        _ => None,
    }
    .unwrap_or(0..text.len() as u32);
    account_local(file, text.as_bytes(), local, &body, calls, hidden, beyond)
}

/// One block per local: the answer of a `--symbol` no definition has, and what a name
/// with definitions still owes for the locals that share it.
pub fn local_blocks(locals: Vec<LocalOf>) -> Vec<ChangedSymbol> {
    locals
        .into_iter()
        .map(|local| {
            let gaps: Vec<_> = (local.computed.then_some(decide::gap::Gap::NotTraced))
                .into_iter()
                .collect();
            let coverage = decide::coverage::Coverages::local(&local.account, &gaps);
            ChangedSymbol {
                callers: local
                    .uses
                    .iter()
                    .map(|u| {
                        Caller::at_line(
                            local.path.clone(),
                            u.inside.clone(),
                            format!("{0}-{0}", u.line),
                            "resolved",
                            if u.call { "call" } else { "reference" },
                            u.line,
                        )
                    })
                    .collect(),
                unresolved: Some(Unknown::LocalOf {
                    name: local.name.clone(),
                    enclosing: local.enclosing,
                }),
                ..ChangedSymbol::bare(
                    local.path,
                    Some(local.name),
                    format!("{0}-{0}", local.line),
                    "local",
                    coverage,
                )
            }
        })
        .collect()
}

/// Candidates shown per changed symbol before they are counted instead.
pub(crate) const MAX_CANDIDATES_SHOWN: usize = 3;

/// Changed callers named per symbol before the rest are counted.
pub(super) const MAX_ALSO_CHANGED_SHOWN: usize = 3;

/// Unlisted definitions shown by name before they are counted.
pub(crate) const MAX_UNTESTED_NAMED: usize = 8;

/// The symbol names of `layout_only`, from its `path::symbol (lines)` labels.
pub fn layout_only_names(output: &ImpactOutput) -> impl Iterator<Item = &str> {
    output.layout_only.iter().filter_map(|l| {
        let (_, rest) = l.split_once("::")?;
        rest.rsplit_once(" (").map(|(name, _)| name)
    })
}
