//! The pack artifact as each door delivers it.
//!
//! One renderer per format and one fit to `--budget`, measured on the text
//! it returns: the native binary, the Python CLI and the MCP server all hand
//! their document here (#507). They used to fit in three places on three
//! different texts, and the Python copy cut JSON and YAML by position,
//! production before docs (#438). The document is the typed
//! [`DiffContextOutput`]; a dict from the Python bridge parses into it first
//! ([`fit_value`]).

use std::fmt::Write as _;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};
use serde_json::Value;

use crate::answer::v2::Range;
use crate::change_class::ChangeClass;
use crate::render::{CONTEXT_SCHEMA, DiffContextOutput, FragmentEntry};
use crate::resource::{CoverageReport, CoverageStatus, LimitReason, ResourceUsage};
use crate::tokenizer::count_tokens;

/// More tokens than the `needs` line of a document ever takes.
const NEEDS_LINE_TOKENS: u32 = 16;

/// What `--budget -1` resolves to in every door: a ceiling no fit applies.
pub const UNLIMITED_BUDGET_TOKENS: u32 = 10_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Yaml,
    Json,
    Md,
    Txt,
}

impl Format {
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "yaml" => Some(Self::Yaml),
            "json" => Some(Self::Json),
            "md" => Some(Self::Md),
            "txt" => Some(Self::Txt),
            _ => None,
        }
    }
}

/// A dict a door holds, as the document it denotes. A document of another schema
/// is refused, never read as this one.
pub fn parse(doc: &Value) -> Result<DiffContextOutput, String> {
    let pack: DiffContextOutput = serde_json::from_value(doc.clone())
        .map_err(|e| format!("not a {CONTEXT_SCHEMA} document: {e}"))?;
    if pack.schema != CONTEXT_SCHEMA {
        return Err(format!(
            "unknown schema {:?}: this build reads {CONTEXT_SCHEMA}",
            pack.schema
        ));
    }
    Ok(pack)
}

/// [`fit`] on a dict: parsed, cut and rendered, with the fitted document written
/// back over it.
pub fn fit_value(doc: &mut Value, format: Format) -> Result<String, String> {
    let mut pack = parse(doc)?;
    let rendered = fit(&mut pack, format);
    *doc = serde_json::to_value(&pack).map_err(|e| e.to_string())?;
    Ok(rendered)
}

/// The text `--budget` bounds, cut until it fits.
///
/// The whole document as the reader receives it, cut to the budget the run
/// recorded in `fit` (auto included). The selection charged an estimate of the
/// envelope; this is the measurement. Context goes from the tail first, then changed fragments lowest evidence
/// tier first; the inventory, coverage and provenance never go. The raw
/// diff is verbatim input, documented as not charged to `--budget`: `fit.exempt`
/// says how much it is.
pub fn fit(pack: &mut DiffContextOutput, format: Format) -> String {
    let Some(cap) = recorded_budget(pack) else {
        return render(pack, format);
    };
    // The patch's weight is stated after the cut, never charged by it: the line
    // that declares an exemption is part of it, so the same budget keeps the same
    // fragments with the patch carried and without.
    let exempt = pack.raw_diff.as_deref().map_or(0, count_tokens);
    if let Some(fit) = pack.fit.as_mut() {
        fit.exempt = 0;
    }
    let measure = |p: &DiffContextOutput| count_tokens(&render_with(p, format, false));
    // What showing everything would take, to the next hundred tokens.
    let whole = measure(&*pack);
    if let Some(fit) = pack.fit.as_mut() {
        fit.needs = whole.div_ceil(100) * 100;
    }
    // The `needs` line is part of the document: a margin it cannot outgrow spares
    // measuring again.
    if whole + NEEDS_LINE_TOKENS <= cap || measure(&*pack) <= cap {
        return declare_exempt(pack, exempt, format);
    }
    crate::fit::shed_to_fit(pack, cap, &measure);
    // The least the artifact can say may outgrow the budget: said, not cut into.
    let least = measure(&*pack);
    if least > cap {
        if let Some(fit) = pack.fit.as_mut() {
            fit.least = Some(least);
        }
    }
    declare_exempt(pack, exempt, format)
}

fn declare_exempt(pack: &mut DiffContextOutput, exempt: u32, format: Format) -> String {
    if let Some(fit) = pack.fit.as_mut() {
        fit.exempt = exempt;
    }
    render(pack, format)
}

/// A pack document, shed a fragment at a time.
impl crate::fit::Shed for DiffContextOutput {
    fn shed_one(&mut self) -> bool {
        drop_one_fragment(self)
    }
}

/// The line that says the budget could not hold even the least the artifact says.
fn budget_note(pack: &DiffContextOutput) -> Option<String> {
    let fit = pack.fit.as_ref()?;
    let needed = fit.least?;
    Some(format!(
        "(budget too small: the least this artifact can say takes {needed} tokens, over the {} asked; `--budget {needed}` or a larger one delivers it without this note)",
        fit.cap
    ))
}

pub fn render(pack: &DiffContextOutput, format: Format) -> String {
    render_with(pack, format, true)
}

fn recorded_budget(pack: &DiffContextOutput) -> Option<u32> {
    pack.fit
        .as_ref()
        .map(|f| f.cap)
        .filter(|&b| b > 0 && b < UNLIMITED_BUDGET_TOKENS)
}

fn render_with(pack: &DiffContextOutput, format: Format, raw_diff: bool) -> String {
    match format {
        Format::Json => format!(
            "{}\n",
            serde_json::to_string_pretty(&delivered(pack, raw_diff)).unwrap_or_default()
        ),
        Format::Yaml => yaml(&delivered(pack, raw_diff)),
        Format::Md => markdown(pack, raw_diff),
        Format::Txt => text(pack, raw_diff),
    }
}

/// The document as serialized, with or without the raw diff. `latency`,
/// telemetry the Python bridge carries beside the artifact, is never in it.
fn delivered(pack: &DiffContextOutput, raw_diff: bool) -> Value {
    let mut value = serde_json::to_value(pack).unwrap_or(Value::Null);
    if !raw_diff {
        if let Some(map) = value.as_object_mut() {
            map.shift_remove("raw_diff");
        }
    }
    value
}

/// Block YAML that a YAML 1.1 reader (`PyYAML`) and a 1.2 one read alike: a
/// string is plain only when no resolver can take it for anything else — a
/// symbol named `off` or `y` stays a string, and `=` does not crash the
/// reader — and a multi-line value is a literal block.
fn yaml(document: &Value) -> String {
    let mut out = String::new();
    for (key, value) in document.as_object().into_iter().flatten() {
        yaml_entry(&mut out, key, value, "");
    }
    out
}

fn yaml_entry(out: &mut String, key: &str, value: &Value, indent: &str) {
    let key = yaml_scalar(&Value::String(key.to_string()));
    match value {
        Value::Object(map) if map.is_empty() => {
            let _ = writeln!(out, "{indent}{key}: {{}}");
        }
        Value::Array(items) if items.is_empty() => {
            let _ = writeln!(out, "{indent}{key}: []");
        }
        Value::Object(map) => {
            let _ = writeln!(out, "{indent}{key}:");
            let inner = format!("{indent}  ");
            for (k, v) in map {
                yaml_entry(out, k, v, &inner);
            }
        }
        Value::Array(items) => {
            let _ = writeln!(out, "{indent}{key}:");
            for item in items {
                yaml_item(out, item, indent);
            }
        }
        Value::String(s) if s.contains('\n') => yaml_block(out, &key, s, indent),
        scalar => {
            let _ = writeln!(out, "{indent}{key}: {}", yaml_scalar(scalar));
        }
    }
}

fn yaml_item(out: &mut String, item: &Value, indent: &str) {
    match item {
        Value::Object(map) if !map.is_empty() => {
            let inner = format!("{indent}    ");
            let mut entries = String::new();
            for (k, v) in map {
                yaml_entry(&mut entries, k, v, &inner);
            }
            let _ = write!(out, "{indent}  - {}", &entries[inner.len()..]);
        }
        Value::Object(_) | Value::Array(_) => {
            let _ = writeln!(out, "{indent}  - {item}");
        }
        scalar => {
            let _ = writeln!(out, "{indent}  - {}", yaml_scalar(scalar));
        }
    }
}

fn yaml_scalar(value: &Value) -> String {
    match value {
        Value::String(s) if yaml_plain(s) => s.clone(),
        Value::String(s) => format!("\"{}\"", yaml_escape(s, false)),
        other => other.to_string(),
    }
}

fn yaml_plain(s: &str) -> bool {
    const RESOLVED: &[&str] = &[
        "y", "Y", "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "true", "True", "TRUE", "false",
        "False", "FALSE", "on", "On", "ON", "off", "Off", "OFF", "null", "Null", "NULL",
    ];
    s.starts_with(|c: char| c.is_ascii_alphabetic())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
        && !RESOLVED.contains(&s)
}

/// YAML's printable set: a reader rejects any other character outright,
/// even inside a literal block (a file holding U+FFFE broke every reader).
fn yaml_printable(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\x20'..='\x7e' | '\u{85}' | '\u{a0}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..)
}

/// A character a literal block cannot carry as itself: one outside the
/// printable set, or a line break YAML reads its own way.
fn yaml_problematic(c: char) -> bool {
    matches!(c, '\r' | '\u{85}' | '\u{2028}' | '\u{2029}') || !yaml_printable(c)
}

fn yaml_escape(s: &str, named_tab: bool) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\0' => escaped.push_str("\\0"),
            '\x08' => escaped.push_str("\\b"),
            '\x0c' => escaped.push_str("\\f"),
            '\t' if named_tab => escaped.push_str("\\t"),
            '\u{2028}' => escaped.push_str("\\u2028"),
            '\u{2029}' => escaped.push_str("\\u2029"),
            c if c.is_control() => {
                let _ = write!(escaped, "\\x{:02x}", c as u32);
            }
            c if !yaml_printable(c) => {
                let _ = write!(escaped, "\\u{:04x}", c as u32);
            }
            c => escaped.push(c),
        }
    }
    escaped
}

/// A literal block whose chomping keeps the tail exactly: clip would turn
/// any tail into one newline, adding one to a file without it.
fn yaml_block(out: &mut String, key: &str, s: &str, indent: &str) {
    if s.chars().any(yaml_problematic) || s.trim().is_empty() {
        let _ = writeln!(out, "{indent}{key}: \"{}\"", yaml_escape(s, true));
        return;
    }
    let body = s.strip_suffix('\n').unwrap_or(s);
    let chomp = match s.len() - s.trim_end_matches('\n').len() {
        0 => "-",
        1 => "",
        _ => "+",
    };
    let _ = writeln!(out, "{indent}{key}: |2{chomp}");
    for line in body.split('\n') {
        if line.is_empty() {
            out.push('\n');
        } else {
            let _ = writeln!(out, "{indent}  {line}");
        }
    }
}

/// One fragment fewer, or a changed one shortened; `false` once nothing is
/// left to drop.
fn drop_one_fragment(pack: &mut DiffContextOutput) -> bool {
    let classes: FxHashMap<&str, ChangeClass> = pack
        .changes
        .iter()
        .map(|c| (c.path.as_str(), c.class))
        .collect();
    let Some(index) = drop_index(&pack.fragments, &classes) else {
        return false;
    };
    // A changed fragment is shortened from its tail before it goes: a larger
    // budget must never show less of the change. Dropped whole, the rewrite
    // of `plan` vanished at the budget where a smaller one showed its
    // signature, while the one-line bumps above it stayed (#438). Context
    // has no such floor and drops whole.
    let changed = pack.fragments[index].role.as_deref() == Some("changed");
    if !(changed && halve_witness(&mut pack.fragments[index])) {
        pack.fragments.remove(index);
    }
    let represented: FxHashSet<&str> = pack.fragments.iter().map(|f| f.path.as_str()).collect();
    let mut unrepresented = false;
    for change in &mut pack.changes {
        let shown = represented.contains(change.path.as_str());
        unrepresented |= !shown && !change.no_fragments;
        change.represented = shown;
    }
    record_cut(pack, unrepresented);
    true
}

fn record_cut(pack: &mut DiffContextOutput, unrepresented: bool) {
    let coverage = pack.coverage.get_or_insert_with(|| CoverageReport {
        status: CoverageStatus::Partial,
        limits: Vec::new(),
        capped: Vec::new(),
        limited_files: Vec::new(),
        resources: ResourceUsage::default(),
        unknown: vec!["resources".to_string()],
        lossy_files: Vec::new(),
    });
    let cut = LimitReason::SelectionBudgetExceeded;
    if !coverage.limits.contains(&cut) {
        coverage.limits.push(cut);
    }
    if unrepresented {
        coverage.status = CoverageStatus::Degraded;
    } else if coverage.status == CoverageStatus::Complete {
        coverage.status = CoverageStatus::Partial;
    }
}

const CLIPPED_MARKER_PREFIX: &str = "… [";
const CLIPPED_MARKER_SUFFIX: &str = " more lines of this change]";

fn halve_witness(fragment: &mut FragmentEntry) -> bool {
    let Some(content) = fragment.content.as_deref() else {
        return false;
    };
    let mut lines: Vec<&str> = content.lines().collect();
    let mut hidden = 0usize;
    if let Some(n) = lines
        .last()
        .and_then(|l| l.strip_prefix(CLIPPED_MARKER_PREFIX))
        .and_then(|l| l.strip_suffix(CLIPPED_MARKER_SUFFIX))
        .and_then(|n| n.parse::<usize>().ok())
    {
        hidden = n;
        lines.pop();
    }
    if lines.len() < 2 {
        return false;
    }
    let keep = (lines.len() * 3 / 4).clamp(1, lines.len() - 1);
    hidden += lines.len() - keep;
    let clipped = format!(
        "{}\n{CLIPPED_MARKER_PREFIX}{hidden}{CLIPPED_MARKER_SUFFIX}\n",
        lines[..keep].join("\n")
    );
    let start: usize = fragment
        .lines
        .split('-')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    fragment.lines = format!("{start}-{}", start + keep - 1);
    fragment.content = Some(Arc::from(clipped.as_str()));
    true
}

/// What goes next when the document is over its budget: context from the
/// tail; then a changed file's second fragment, before any file loses its
/// last; then those last witnesses. Changed fragments go lowest evidence
/// tier first — generated files, mechanical bumps, config and docs, tests,
/// production last — the reverse of the order the selection placed them
/// in, and from the tail within a tier. By position alone the production
/// file went first because `zsrc/` sorts after `README.md` and `tests/`
/// (#411, #438).
fn drop_index(
    fragments: &[FragmentEntry],
    classes: &FxHashMap<&str, ChangeClass>,
) -> Option<usize> {
    if let Some(i) = fragments
        .iter()
        .rposition(|f| f.role.as_deref() != Some("changed"))
    {
        return Some(i);
    }
    let tier = |f: &FragmentEntry| {
        let class = classes
            .get(f.path.as_str())
            .copied()
            .unwrap_or(ChangeClass::Content);
        crate::pipeline::evidence_tier(&f.path, class)
    };
    let mut per_file: FxHashMap<&str, usize> = FxHashMap::default();
    for f in fragments {
        *per_file.entry(f.path.as_str()).or_default() += 1;
    }
    let lowest_from_the_tail = |second: bool| {
        (0..fragments.len())
            .filter(|&i| !second || per_file[fragments[i].path.as_str()] > 1)
            .max_by_key(|&i| (tier(&fragments[i]), i))
    };
    lowest_from_the_tail(true).or_else(|| lowest_from_the_tail(false))
}

/// Whether the document names anything beyond its heading.
fn has_diff_metadata(pack: &DiffContextOutput) -> bool {
    !pack.fragments.is_empty()
        || !pack.inventory.is_empty()
        || !pack.range.messages.is_empty()
        || pack.raw_diff.as_deref().is_some_and(|d| !d.is_empty())
}

/// `str.splitlines()`: the commit messages were split by it, `\r` and the
/// Unicode separators included.
fn splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            lines.push(&s[start..i]);
            let mut end = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
                chars.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// Changed files no fragment of which is in the document, and those that
/// yielded none to select.
fn changed_file_marks(pack: &DiffContextOutput) -> (FxHashSet<&str>, FxHashSet<&str>) {
    let omitted = if pack.changes.is_empty() {
        let represented: FxHashSet<&str> = pack.fragments.iter().map(|f| f.path.as_str()).collect();
        pack.inventory
            .changed
            .items
            .iter()
            .map(String::as_str)
            .filter(|p| !represented.contains(p))
            .collect()
    } else {
        pack.changes
            .iter()
            .filter(|c| !c.represented && !c.no_fragments)
            .map(|c| c.path.as_str())
            .collect()
    };
    let fragmentless = pack
        .changes
        .iter()
        .filter(|c| c.no_fragments)
        .map(|c| c.path.as_str())
        .collect();
    (omitted, fragmentless)
}

fn mark<'a>(
    path: &str,
    (omitted, fragmentless): &(FxHashSet<&str>, FxHashSet<&str>),
    no_fragments: &'a str,
    omitted_mark: &'a str,
) -> &'a str {
    if fragmentless.contains(path) {
        no_fragments
    } else if omitted.contains(path) {
        omitted_mark
    } else {
        ""
    }
}

/// How many commits the messages stand for: all of them, or the newest few.
fn commit_count_shown(range: &Range) -> String {
    let listed = range.messages.len();
    if range.commits > listed {
        format!("newest {listed} of {}", range.commits)
    } else {
        listed.to_string()
    }
}

fn markdown(pack: &DiffContextOutput, raw_diff: bool) -> String {
    let mut out = format!("# diff context: {}\n\n", pack.name);
    if !has_diff_metadata(pack) {
        return out;
    }
    md_commit_messages(&mut out, &pack.range);
    if let Some(note) = coverage_note(pack) {
        let _ = write!(out, "*{note}*\n\n");
    }
    if let Some(note) = budget_note(pack) {
        let _ = write!(out, "*{note}*\n\n");
    }
    md_changed_files(&mut out, pack);
    md_path_list(&mut out, &pack.inventory.deleted.items, "Deleted files");
    let renames = &pack.inventory.renamed.items;
    if !renames.is_empty() {
        out.push_str("**Renamed files:**\n\n");
        for pair in renames {
            let _ = writeln!(
                out,
                "- {} \u{2192} {}",
                inline_code(&pair.from),
                inline_code(&pair.to)
            );
        }
        out.push('\n');
    }
    md_path_list(
        &mut out,
        &pack.inventory.lockfiles.items,
        "Lock files changed",
    );
    md_path_list(
        &mut out,
        &pack.inventory.ignored.items,
        "Changed but excluded by ignore rules",
    );
    if pack.inventory.withheld > 0 {
        let _ = write!(
            out,
            "*{} changed file(s) withheld by exclusion policy (`.diffctx/ignore` or secret paths).*\n\n",
            pack.inventory.withheld
        );
    }
    if let Some(patch) = pack
        .raw_diff
        .as_deref()
        .filter(|d| raw_diff && !d.is_empty())
    {
        out.push_str("## Raw diff\n\n");
        code_block(&mut out, patch, "diff");
    }
    for frag in &pack.fragments {
        md_fragment(&mut out, frag);
    }
    out
}

fn md_commit_messages(out: &mut String, range: &Range) {
    match range.messages.as_slice() {
        [] => return,
        [only] => md_quoted(out, only, "> "),
        messages => {
            let _ = writeln!(out, "> {} commits:", commit_count_shown(range));
            for message in messages {
                let (subject, body) = message.split_once('\n').unwrap_or((message.as_str(), ""));
                let _ = writeln!(out, "> - **{subject}**");
                md_quoted(out, body.trim_matches('\n'), ">   ");
            }
        }
    }
    out.push('\n');
}

fn md_quoted(out: &mut String, text: &str, prefix: &str) {
    for line in splitlines(text) {
        if line.is_empty() {
            out.push_str(">\n");
        } else {
            let _ = writeln!(out, "{prefix}{line}");
        }
    }
}

fn coverage_note(pack: &DiffContextOutput) -> Option<String> {
    let coverage = pack.coverage.as_ref()?;
    if coverage.status == CoverageStatus::Complete {
        return None;
    }
    let files: Vec<String> = coverage
        .limited_files
        .iter()
        .map(|f| format!("{} ({})", f.path, crate::impact::reason_names(&[f.reason])))
        .collect();
    let place = if files.is_empty() {
        String::new()
    } else {
        format!(" in {}", files.join(", "))
    };
    Some(format!(
        "Coverage: {} — the run hit a limit ({}){place}; context may be missing.",
        coverage.status.as_str(),
        crate::impact::reason_names(&coverage.limits)
    ))
}

fn md_changed_files(out: &mut String, pack: &DiffContextOutput) {
    let changed = &pack.inventory.changed.items;
    if changed.is_empty() {
        return;
    }
    let marks = changed_file_marks(pack);
    out.push_str("**Changed files:**\n\n");
    for path in changed {
        let _ = writeln!(
            out,
            "- {}{}",
            inline_code(path),
            mark(path, &marks, " — no fragments", " — omitted")
        );
    }
    if !marks.0.is_empty() {
        out.push_str(
            "\n*\u{201c}omitted\u{201d} = no fragment of this file is in the output (budget/selection).*\n",
        );
    }
    if !marks.1.is_empty() {
        out.push_str(
            "\n*\u{201c}no fragments\u{201d} = the file yielded nothing to select (not code, binary, or over the size cap).*\n",
        );
    }
    out.push('\n');
}

fn md_path_list(out: &mut String, paths: &[String], title: &str) {
    if paths.is_empty() {
        return;
    }
    let _ = write!(out, "**{title}:**\n\n");
    for path in paths {
        let _ = writeln!(out, "- {}", inline_code(path));
    }
    out.push('\n');
}

fn md_fragment(out: &mut String, frag: &FragmentEntry) {
    let path = frag.path.as_str();
    let mut header = inline_code(&format!("{path}:{}", frag.lines));
    if let Some(symbol) = frag.symbol.as_deref().filter(|s| !s.is_empty()) {
        let _ = write!(header, " **{}**", escape_markdown(symbol));
    }
    if !frag.kind.is_empty() {
        let _ = write!(header, " _{}_", escape_markdown(&frag.kind));
    }
    if frag.role.as_deref() == Some("changed") {
        header.push_str(" — **changed**");
    }
    let _ = write!(out, "## {header}\n\n");
    let content = frag.content.as_deref().unwrap_or_default();
    if !content.is_empty() {
        let name = path.rsplit('/').next().unwrap_or(path);
        let lang = crate::languages::get_language_for_file(name).unwrap_or_default();
        code_block(out, content, lang);
    }
}

fn longest_backtick_run(s: &str) -> usize {
    s.split(|c| c != '`').map(str::len).max().unwrap_or(0)
}

fn code_block(out: &mut String, content: &str, lang: &str) {
    let fence = "`".repeat((longest_backtick_run(content) + 1).max(3));
    let _ = writeln!(out, "{fence}{lang}");
    out.push_str(content);
    if !content.ends_with('\n') {
        out.push('\n');
    }
    let _ = write!(out, "{fence}\n\n");
}

fn inline_code(s: &str) -> String {
    if !s.contains('`') {
        return format!("`{s}`");
    }
    let fence = "`".repeat(longest_backtick_run(s) + 1);
    format!("{fence} {s} {fence}")
}

fn escape_markdown(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\`*_[]()#>+-~|{}!".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

const OMITTED_TEXT_MARK: &str = " (omitted)";
const NO_FRAGMENTS_TEXT_MARK: &str = " (no fragments)";

fn text(pack: &DiffContextOutput, raw_diff: bool) -> String {
    let mut out = format!("diff context: {}\n", pack.name);
    if !has_diff_metadata(pack) {
        return out;
    }
    if let Some(note) = budget_note(pack) {
        let _ = writeln!(out, "  {note}");
    }
    let messages = &pack.range.messages;
    if !messages.is_empty() {
        let _ = writeln!(out, "  commits: {}", commit_count_shown(&pack.range));
        for message in messages {
            for (i, line) in splitlines(message).into_iter().enumerate() {
                let _ = writeln!(out, "    {}{line}", if i == 0 { "- " } else { "  " });
            }
        }
    }
    let changed = &pack.inventory.changed.items;
    if !changed.is_empty() {
        let marks = changed_file_marks(pack);
        out.push_str("  changed files:\n");
        for path in changed {
            let _ = writeln!(
                out,
                "    {}{}",
                escape_text_path(path),
                mark(path, &marks, NO_FRAGMENTS_TEXT_MARK, OMITTED_TEXT_MARK)
            );
        }
    }
    text_path_list(&mut out, &pack.inventory.deleted.items, "deleted files");
    for pair in &pack.inventory.renamed.items {
        let _ = writeln!(out, "  renamed: {} -> {}", pair.from, pair.to);
    }
    text_path_list(
        &mut out,
        &pack.inventory.lockfiles.items,
        "lock files changed",
    );
    text_path_list(
        &mut out,
        &pack.inventory.ignored.items,
        "changed but excluded by ignore rules",
    );
    if pack.inventory.withheld > 0 {
        let _ = writeln!(
            out,
            "  changed files withheld by exclusion policy: {}",
            pack.inventory.withheld
        );
    }
    if let Some(patch) = pack
        .raw_diff
        .as_deref()
        .filter(|d| raw_diff && !d.is_empty())
    {
        out.push_str("  raw diff:\n");
        for line in patch.trim_end_matches('\n').split('\n') {
            if line.is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    for frag in &pack.fragments {
        text_fragment(&mut out, frag);
    }
    out
}

fn text_fragment(out: &mut String, frag: &FragmentEntry) {
    let mut header = format!("{}:{}", frag.path, frag.lines);
    for (value, open, close) in [
        (frag.symbol.as_deref().unwrap_or_default(), " (", ")"),
        (frag.kind.as_str(), " [", "]"),
        (frag.role.as_deref().unwrap_or_default(), " <", ">"),
    ] {
        if !value.is_empty() {
            let _ = write!(header, "{open}{value}{close}");
        }
    }
    let _ = writeln!(out, "  {header}");
    let content = frag.content.as_deref().unwrap_or_default();
    if !content.is_empty() {
        for line in content.trim_end_matches('\n').split('\n') {
            let _ = writeln!(out, "    {line}");
        }
    }
}

fn text_path_list(out: &mut String, paths: &[String], label: &str) {
    if paths.is_empty() {
        return;
    }
    let _ = writeln!(out, "  {label}:");
    for path in paths {
        let _ = writeln!(out, "    {}", escape_text_path(path));
    }
}

/// One path per line, so the two line breaks git can emit unquoted are
/// escaped, backslash first; and a file literally named `report (omitted)`
/// must not read as an omitted `report`.
fn escape_text_path(path: &str) -> String {
    let mut text = path
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    for mark in [OMITTED_TEXT_MARK, NO_FRAGMENTS_TEXT_MARK] {
        if let Some(stem) = text.strip_suffix(mark) {
            text = format!("{stem} \\{}", &mark[1..]);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, role: Option<&str>, lines: usize) -> FragmentEntry {
        let body: String = (0..lines).map(|i| format!("line_{i}\n")).collect();
        FragmentEntry {
            path: path.to_string(),
            lines: format!("1-{lines}"),
            role: role.map(str::to_string),
            kind: "function".to_string(),
            symbol: None,
            content: Some(Arc::from(body.as_str())),
        }
    }

    fn pack(fragments: Vec<FragmentEntry>, changed: &[&str]) -> DiffContextOutput {
        let mut pack = DiffContextOutput::empty("repo");
        pack.fragments = fragments;
        pack.inventory.changed =
            crate::answer::v2::Listing::of(changed.iter().map(|p| (*p).to_string()).collect(), 0);
        pack
    }

    #[test]
    fn budget_fitting_drops_context_whole_and_shortens_only_the_change() {
        let mut doc = pack(
            vec![
                entry("app.py", Some("changed"), 40),
                entry("lib/helpers.py", None, 40),
            ],
            &[],
        );

        assert!(drop_one_fragment(&mut doc));
        assert_eq!(doc.fragments.len(), 1, "context must drop, not shrink");
        assert_eq!(doc.fragments[0].path, "app.py");

        assert!(drop_one_fragment(&mut doc));
        let witness = &doc.fragments[0];
        assert_eq!(witness.lines, "1-30");
        assert!(
            witness
                .content
                .as_deref()
                .unwrap()
                .ends_with("… [10 more lines of this change]\n")
        );
        assert_eq!(
            doc.coverage.as_ref().unwrap().limits[0],
            LimitReason::SelectionBudgetExceeded
        );
    }

    /// A dict a door hands back is the document it came from, and a document of
    /// another schema is refused.
    #[test]
    fn a_dict_parses_into_the_document_and_a_foreign_schema_is_refused() {
        let doc = pack(vec![entry("app.py", Some("changed"), 3)], &["app.py"]);
        let value = serde_json::to_value(&doc).unwrap();
        let back = parse(&value).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), value);

        let mut foreign = value;
        foreign["schema"] = "diffctx.context.v1".into();
        assert!(parse(&foreign).unwrap_err().contains("unknown schema"));
    }

    #[test]
    fn yaml_quotes_what_a_yaml_1_1_reader_would_resolve() {
        let mut doc = pack(
            vec![FragmentEntry {
                symbol: Some("on".to_string()),
                content: Some(Arc::from("x\n\n  y\n")),
                ..entry("a.py", None, 1)
            }],
            &["off", "y", "=", "2026-10-15", "src/app.py", "tab\there"],
        );
        doc.fragments[0].lines = "1-1".to_string();
        let text = render(&doc, Format::Yaml);
        assert!(text.starts_with("schema: diffctx.context.v2\n"), "{text}");
        for quoted in [
            "\"off\"",
            "\"y\"",
            "\"=\"",
            "\"2026-10-15\"",
            "\"tab\\x09here\"",
            "symbol: \"on\"",
        ] {
            assert!(text.contains(quoted), "{quoted} in {text}");
        }
        assert!(text.contains("  - src/app.py\n"), "{text}");
        assert!(
            text.contains("    content: |2\n      x\n\n        y\n"),
            "{text}"
        );
    }

    /// A character outside YAML's printable set fails a reader even inside a
    /// literal block: it is escaped, in a block's text and in a plain string.
    #[test]
    fn yaml_escapes_what_falls_outside_the_printable_set() {
        let doc = pack(
            vec![FragmentEntry {
                content: Some(Arc::from("\u{fffe}\n")),
                ..entry("a.py", None, 1)
            }],
            &["a\u{ffff}b"],
        );
        let text = render(&doc, Format::Yaml);
        assert!(!text.contains(['\u{fffe}', '\u{ffff}']), "{text:?}");
        assert!(text.contains("  - \"a\\uffffb\"\n"), "{text}");
        assert!(text.contains("content: \"\\ufffe\\n\"\n"), "{text}");
    }

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(splitlines("a\r\nb\rc\n\nd\n"), ["a", "b", "c", "", "d"]);
        assert_eq!(splitlines("a\u{2028}b"), ["a", "b"]);
        assert!(splitlines("").is_empty());
    }

    #[test]
    fn a_path_spelled_like_a_mark_is_escaped() {
        assert_eq!(escape_text_path("report (omitted)"), "report \\(omitted)");
        assert_eq!(escape_text_path("a\\b\nc"), "a\\\\b\\nc");
    }
}
