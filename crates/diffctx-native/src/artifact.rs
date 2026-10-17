//! The pack artifact as each door delivers it.
//!
//! One renderer per format and one fit to `--budget`, measured on the text
//! it returns: the native binary, the Python CLI and the MCP server all hand
//! their serialized document here (#507). They used to fit in three places
//! on three different texts, and the Python copy cut JSON and YAML by
//! position, production before docs (#438).

use std::fmt::Write as _;

use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

use crate::change_class::ChangeClass;
use crate::resource::{CoverageReport, LimitReason, ResourceUsage};
use crate::tokenizer::count_tokens;

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

/// The text `--budget` bounds, cut until it fits.
///
/// The whole document as the reader receives it, cut to the budget the run
/// recorded (auto included). The selection charged an estimate of the
/// envelope; this is the measurement. Context goes from the tail first, then changed fragments lowest evidence
/// tier first; the inventory, coverage and provenance never go. The raw
/// diff is verbatim input, documented as not charged to `--budget`.
pub fn fit(doc: &mut Value, format: Format) -> String {
    let mut rendered = render(doc, format);
    let Some(budget) = recorded_budget(doc) else {
        return rendered;
    };
    while charged_tokens(doc, format, &rendered) > budget && drop_one_fragment(doc) {
        rendered = render(doc, format);
    }
    rendered
}

pub fn render(doc: &Value, format: Format) -> String {
    render_with(doc, format, true)
}

fn recorded_budget(doc: &Value) -> Option<u32> {
    doc.pointer("/provenance/selection/budget_tokens")?
        .as_u64()
        .filter(|&b| b > 0 && b < u64::from(UNLIMITED_BUDGET_TOKENS))
        .map(|b| b as u32)
}

fn charged_tokens(doc: &Value, format: Format, rendered: &str) -> u32 {
    if truthy(doc.get("raw_diff")) {
        count_tokens(&render_with(doc, format, false))
    } else {
        count_tokens(rendered)
    }
}

fn render_with(doc: &Value, format: Format, raw_diff: bool) -> String {
    let delivered = Delivered { doc, raw_diff };
    match format {
        Format::Json => format!(
            "{}\n",
            serde_json::to_string_pretty(&delivered).unwrap_or_default()
        ),
        Format::Yaml => yaml(&delivered),
        Format::Md => markdown(doc, raw_diff),
        Format::Txt => text(doc, raw_diff),
    }
}

/// The document without `latency`, telemetry the Python bridge carries
/// beside the artifact, and with or without the raw diff.
struct Delivered<'a> {
    doc: &'a Value,
    raw_diff: bool,
}

impl Delivered<'_> {
    fn entries(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.doc
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(k, _)| k.as_str() != "latency" && (self.raw_diff || k.as_str() != "raw_diff"))
    }
}

impl Serialize for Delivered<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.entries())
    }
}

/// Block YAML that a YAML 1.1 reader (`PyYAML`) and a 1.2 one read alike: a
/// string is plain only when no resolver can take it for anything else — a
/// symbol named `off` or `y` stays a string, and `=` does not crash the
/// reader — and a multi-line value is a literal block.
fn yaml(delivered: &Delivered) -> String {
    let mut out = String::new();
    for (key, value) in delivered.entries() {
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

fn yaml_problematic(c: char) -> bool {
    matches!(c, '\x00'..='\x08' | '\x0b'..='\x1f' | '\x7f'..='\u{9f}' | '\u{2028}' | '\u{2029}')
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
fn drop_one_fragment(doc: &mut Value) -> bool {
    let Some(obj) = doc.as_object_mut() else {
        return false;
    };
    let classes: FxHashMap<String, ChangeClass> = list(obj.get("changes"))
        .iter()
        .filter_map(|c| {
            let class = serde_json::from_value(c.get("class")?.clone()).ok()?;
            Some((str_at(c, "path").to_string(), class))
        })
        .collect();
    let Some(fragments) = obj.get_mut("fragments").and_then(Value::as_array_mut) else {
        return false;
    };
    let Some(index) = drop_index(fragments, &classes) else {
        return false;
    };
    // A changed fragment is shortened from its tail before it goes: a larger
    // budget must never show less of the change. Dropped whole, the rewrite
    // of `plan` vanished at the budget where a smaller one showed its
    // signature, while the one-line bumps above it stayed (#438). Context
    // has no such floor and drops whole.
    let changed = str_at(&fragments[index], "role") == "changed";
    if !(changed && halve_witness(&mut fragments[index])) {
        fragments.remove(index);
    }
    let represented: FxHashSet<String> = fragments
        .iter()
        .map(|f| str_at(f, "path").to_string())
        .collect();
    let count = fragments.len();
    obj.insert("fragment_count".to_string(), count.into());
    let mut unrepresented = false;
    if let Some(changes) = obj.get_mut("changes").and_then(Value::as_array_mut) {
        for change in changes.iter_mut().filter_map(Value::as_object_mut) {
            let shown = change
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|p| represented.contains(p));
            unrepresented |= !shown && !truthy(change.get("no_fragments"));
            change.insert("represented".to_string(), shown.into());
        }
    }
    record_cut(obj, unrepresented);
    true
}

fn record_cut(obj: &mut Map<String, Value>, unrepresented: bool) {
    if !obj.get("coverage").is_some_and(Value::is_object) {
        let fresh = CoverageReport {
            status: "partial",
            limit_reasons: Vec::new(),
            capped: Vec::new(),
            limited_files: Vec::new(),
            resources: ResourceUsage::default(),
            lossy_files: Vec::new(),
        };
        let at = obj
            .keys()
            .position(|k| k == "redactions" || k == "latency")
            .unwrap_or(obj.len());
        obj.shift_insert(
            at,
            "coverage".to_string(),
            serde_json::to_value(fresh).unwrap_or_default(),
        );
    }
    let Some(coverage) = obj.get_mut("coverage").and_then(Value::as_object_mut) else {
        return;
    };
    let cut = serde_json::to_value(LimitReason::SelectionBudgetExceeded).unwrap_or_default();
    if !coverage.contains_key("limit_reasons") {
        let after_status = coverage
            .keys()
            .position(|k| k == "status")
            .map_or(0, |i| i + 1);
        coverage.shift_insert(
            after_status,
            "limit_reasons".to_string(),
            Value::Array(Vec::new()),
        );
    }
    if let Some(reasons) = coverage
        .get_mut("limit_reasons")
        .and_then(Value::as_array_mut)
    {
        if !reasons.contains(&cut) {
            reasons.push(cut);
        }
    }
    let status = if unrepresented {
        Some("degraded")
    } else if coverage.get("status").and_then(Value::as_str) == Some("complete") {
        Some("partial")
    } else {
        None
    };
    if let Some(status) = status {
        coverage.insert("status".to_string(), status.into());
    }
}

const CLIPPED_MARKER_PREFIX: &str = "… [";
const CLIPPED_MARKER_SUFFIX: &str = " more lines of this change]";

fn halve_witness(fragment: &mut Value) -> bool {
    let Some(content) = fragment.get("content").and_then(Value::as_str) else {
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
    let start: usize = str_at(fragment, "lines")
        .split('-')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    fragment["lines"] = format!("{start}-{}", start + keep - 1).into();
    fragment["content"] = clipped.into();
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
fn drop_index(fragments: &[Value], classes: &FxHashMap<String, ChangeClass>) -> Option<usize> {
    if let Some(i) = fragments
        .iter()
        .rposition(|f| str_at(f, "role") != "changed")
    {
        return Some(i);
    }
    let tier = |f: &Value| {
        let path = str_at(f, "path");
        let class = classes.get(path).copied().unwrap_or(ChangeClass::Content);
        crate::pipeline::evidence_tier(path, class)
    };
    let mut per_file: FxHashMap<&str, usize> = FxHashMap::default();
    for f in fragments {
        *per_file.entry(str_at(f, "path")).or_default() += 1;
    }
    let lowest_from_the_tail = |second: bool| {
        (0..fragments.len())
            .filter(|&i| !second || per_file[str_at(&fragments[i], "path")] > 1)
            .max_by_key(|&i| (tier(&fragments[i]), i))
    };
    lowest_from_the_tail(true).or_else(|| lowest_from_the_tail(false))
}

fn str_at<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn list(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// Python's truthiness, which decided what the reader formats showed.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn shown(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn has_diff_metadata(doc: &Value) -> bool {
    [
        "fragments",
        "deleted_files",
        "renamed_files",
        "changed_files",
        "commit_message",
        "lockfile_changes",
        "ignored_changes",
        "policy_excluded_count",
        "raw_diff",
    ]
    .iter()
    .any(|k| truthy(doc.get(*k)))
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
fn changed_file_marks(doc: &Value) -> (FxHashSet<String>, FxHashSet<String>) {
    let changes = list(doc.get("changes"));
    let omitted = if changes.is_empty() {
        let represented: FxHashSet<&str> = list(doc.get("fragments"))
            .iter()
            .map(|f| str_at(f, "path"))
            .collect();
        list(doc.get("changed_files"))
            .iter()
            .map(shown)
            .filter(|p| !represented.contains(p.as_str()))
            .collect()
    } else {
        changes
            .iter()
            .filter(|c| {
                c.get("represented").and_then(Value::as_bool) == Some(false)
                    && !truthy(c.get("no_fragments"))
            })
            .map(|c| str_at(c, "path").to_string())
            .collect()
    };
    let fragmentless = changes
        .iter()
        .filter(|c| truthy(c.get("no_fragments")))
        .map(|c| str_at(c, "path").to_string())
        .collect();
    (omitted, fragmentless)
}

fn mark<'a>(
    path: &str,
    (omitted, fragmentless): &(FxHashSet<String>, FxHashSet<String>),
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

fn commit_count_shown(doc: &Value, listed: usize) -> String {
    match doc.get("commit_count").and_then(Value::as_u64) {
        Some(total) if total > listed as u64 => format!("newest {listed} of {total}"),
        _ => listed.to_string(),
    }
}

fn markdown(doc: &Value, raw_diff: bool) -> String {
    let mut out = format!("# diff context: {}\n\n", str_at(doc, "name"));
    if !has_diff_metadata(doc) {
        return out;
    }
    md_commit_messages(&mut out, doc);
    if let Some(note) = coverage_note(doc) {
        let _ = write!(out, "*{note}*\n\n");
    }
    md_changed_files(&mut out, doc);
    md_path_list(&mut out, doc, "deleted_files", "Deleted files");
    let renames = list(doc.get("renamed_files"));
    if !renames.is_empty() {
        out.push_str("**Renamed files:**\n\n");
        for pair in renames {
            let _ = writeln!(
                out,
                "- {} \u{2192} {}",
                inline_code(str_at(pair, "from")),
                inline_code(str_at(pair, "to"))
            );
        }
        out.push('\n');
    }
    md_path_list(&mut out, doc, "lockfile_changes", "Lock files changed");
    md_path_list(
        &mut out,
        doc,
        "ignored_changes",
        "Changed but excluded by ignore rules",
    );
    if let Some(n) = doc.get("policy_excluded_count").filter(|v| truthy(Some(v))) {
        let _ = write!(
            out,
            "*{n} changed file(s) withheld by exclusion policy (`.diffctx/ignore` or secret paths).*\n\n"
        );
    }
    if raw_diff && truthy(doc.get("raw_diff")) {
        out.push_str("## Raw diff\n\n");
        code_block(&mut out, str_at(doc, "raw_diff"), "diff");
    }
    for frag in list(doc.get("fragments")) {
        md_fragment(&mut out, frag);
    }
    out
}

fn md_commit_messages(out: &mut String, doc: &Value) {
    let messages = list(doc.get("commit_messages"));
    if messages.is_empty() {
        if truthy(doc.get("commit_message")) {
            let _ = write!(out, "> {}\n\n", str_at(doc, "commit_message"));
        }
        return;
    }
    if let [only] = messages {
        md_quoted(out, &shown(only), "> ");
    } else {
        let _ = writeln!(
            out,
            "> {} commits:",
            commit_count_shown(doc, messages.len())
        );
        for message in messages {
            let message = shown(message);
            let (subject, body) = message.split_once('\n').unwrap_or((&message, ""));
            let _ = writeln!(out, "> - **{subject}**");
            md_quoted(out, body.trim_matches('\n'), ">   ");
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

fn coverage_note(doc: &Value) -> Option<String> {
    let coverage = doc.get("coverage").filter(|c| truthy(Some(c)))?;
    let status = coverage
        .get("status")
        .map_or_else(|| "partial".to_string(), shown);
    if status == "complete" {
        return None;
    }
    let reasons: Vec<String> = list(coverage.get("limit_reasons"))
        .iter()
        .map(shown)
        .collect();
    let files: Vec<String> = list(coverage.get("limited_files"))
        .iter()
        .map(|f| format!("{} ({})", str_at(f, "path"), str_at(f, "reason")))
        .collect();
    let place = if files.is_empty() {
        String::new()
    } else {
        format!(" in {}", files.join(", "))
    };
    Some(format!(
        "Coverage: {status} — the run hit a limit ({}){place}; context may be missing.",
        reasons.join(", ")
    ))
}

fn md_changed_files(out: &mut String, doc: &Value) {
    let changed = list(doc.get("changed_files"));
    if changed.is_empty() {
        return;
    }
    let marks = changed_file_marks(doc);
    out.push_str("**Changed files:**\n\n");
    for path in changed {
        let path = shown(path);
        let _ = writeln!(
            out,
            "- {}{}",
            inline_code(&path),
            mark(&path, &marks, " — no fragments", " — omitted")
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

fn md_path_list(out: &mut String, doc: &Value, key: &str, title: &str) {
    let paths = list(doc.get(key));
    if paths.is_empty() {
        return;
    }
    let _ = write!(out, "**{title}:**\n\n");
    for path in paths {
        let _ = writeln!(out, "- {}", inline_code(&shown(path)));
    }
    out.push('\n');
}

fn md_fragment(out: &mut String, frag: &Value) {
    let path = str_at(frag, "path");
    let mut header = inline_code(&format!("{path}:{}", str_at(frag, "lines")));
    let symbol = str_at(frag, "symbol");
    if !symbol.is_empty() {
        let _ = write!(header, " **{}**", escape_markdown(symbol));
    }
    let kind = str_at(frag, "kind");
    if !kind.is_empty() {
        let _ = write!(header, " _{}_", escape_markdown(kind));
    }
    if str_at(frag, "role") == "changed" {
        header.push_str(" — **changed**");
    }
    let _ = write!(out, "## {header}\n\n");
    let content = str_at(frag, "content");
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

fn text(doc: &Value, raw_diff: bool) -> String {
    let mut out = format!("diff context: {}\n", str_at(doc, "name"));
    if !has_diff_metadata(doc) {
        return out;
    }
    let messages = list(doc.get("commit_messages"));
    if !messages.is_empty() {
        let _ = writeln!(
            out,
            "  commits: {}",
            commit_count_shown(doc, messages.len())
        );
        for message in messages {
            for (i, line) in splitlines(&shown(message)).into_iter().enumerate() {
                let _ = writeln!(out, "    {}{line}", if i == 0 { "- " } else { "  " });
            }
        }
    } else if truthy(doc.get("commit_message")) {
        let _ = writeln!(out, "  change: {}", str_at(doc, "commit_message"));
    }
    let changed = list(doc.get("changed_files"));
    if !changed.is_empty() {
        let marks = changed_file_marks(doc);
        out.push_str("  changed files:\n");
        for path in changed {
            let path = shown(path);
            let _ = writeln!(
                out,
                "    {}{}",
                escape_text_path(&path),
                mark(&path, &marks, NO_FRAGMENTS_TEXT_MARK, OMITTED_TEXT_MARK)
            );
        }
    }
    text_path_list(&mut out, doc, "deleted_files", "deleted files");
    for pair in list(doc.get("renamed_files")) {
        let _ = writeln!(
            out,
            "  renamed: {} -> {}",
            str_at(pair, "from"),
            str_at(pair, "to")
        );
    }
    text_path_list(&mut out, doc, "lockfile_changes", "lock files changed");
    text_path_list(
        &mut out,
        doc,
        "ignored_changes",
        "changed but excluded by ignore rules",
    );
    if let Some(n) = doc.get("policy_excluded_count").filter(|v| truthy(Some(v))) {
        let _ = writeln!(out, "  changed files withheld by exclusion policy: {n}");
    }
    if raw_diff && truthy(doc.get("raw_diff")) {
        out.push_str("  raw diff:\n");
        for line in str_at(doc, "raw_diff").trim_end_matches('\n').split('\n') {
            if line.is_empty() {
                out.push('\n');
            } else {
                let _ = writeln!(out, "    {line}");
            }
        }
    }
    for frag in list(doc.get("fragments")) {
        text_fragment(&mut out, frag);
    }
    out
}

fn text_fragment(out: &mut String, frag: &Value) {
    let mut header = format!("{}:{}", str_at(frag, "path"), str_at(frag, "lines"));
    for (key, open, close) in [
        ("symbol", " (", ")"),
        ("kind", " [", "]"),
        ("role", " <", ">"),
    ] {
        let value = str_at(frag, key);
        if !value.is_empty() {
            let _ = write!(header, "{open}{value}{close}");
        }
    }
    let _ = writeln!(out, "  {header}");
    let content = str_at(frag, "content");
    if !content.is_empty() {
        for line in content.trim_end_matches('\n').split('\n') {
            let _ = writeln!(out, "    {line}");
        }
    }
}

fn text_path_list(out: &mut String, doc: &Value, key: &str, label: &str) {
    let paths = list(doc.get(key));
    if paths.is_empty() {
        return;
    }
    let _ = writeln!(out, "  {label}:");
    for path in paths {
        let _ = writeln!(out, "    {}", escape_text_path(&shown(path)));
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

    fn entry(path: &str, role: Option<&str>, lines: usize) -> Value {
        let body: String = (0..lines).map(|i| format!("line_{i}\n")).collect();
        let mut f = serde_json::json!({
            "path": path,
            "lines": format!("1-{lines}"),
            "kind": "function",
            "content": body,
        });
        if let Some(role) = role {
            f["role"] = role.into();
        }
        f
    }

    #[test]
    fn budget_fitting_drops_context_whole_and_shortens_only_the_change() {
        let mut doc = serde_json::json!({
            "name": "repo",
            "type": "diff_context",
            "fragment_count": 2,
            "fragments": [entry("app.py", Some("changed"), 40), entry("lib/helpers.py", None, 40)],
        });

        assert!(drop_one_fragment(&mut doc));
        let fragments = doc["fragments"].as_array().unwrap();
        assert_eq!(fragments.len(), 1, "context must drop, not shrink");
        assert_eq!(fragments[0]["path"], "app.py");

        assert!(drop_one_fragment(&mut doc));
        let witness = &doc["fragments"][0];
        assert_eq!(witness["lines"], "1-30");
        assert!(
            witness["content"]
                .as_str()
                .unwrap()
                .ends_with("… [10 more lines of this change]\n")
        );
        assert_eq!(
            doc["coverage"]["limit_reasons"][0],
            "selection_budget_exceeded"
        );
    }

    #[test]
    fn yaml_quotes_what_a_yaml_1_1_reader_would_resolve() {
        let doc = serde_json::json!({
            "schema": "diffctx.context.v1",
            "changed_files": ["off", "y", "=", "2026-10-15", "src/app.py", "tab\there"],
            "fragments": [{"path": "a.py", "symbol": "on", "content": "x\n\n  y\n"}],
        });
        let text = render(&doc, Format::Yaml);
        assert!(text.starts_with("schema: diffctx.context.v1\n"), "{text}");
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
