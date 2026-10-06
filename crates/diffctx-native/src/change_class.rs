//! What kind of change a file carries, stated explicitly so the selection
//! policy can rank evidence by it and the artifact can say what it decided.
//!
//! A range that mixes an image updater's one-line `tag:` bumps with a
//! hand-written manifest rewrite used to lose the manifests: every core
//! competed on token cost and the cheap bumps won (#263). The class is a
//! priority, never a filter — a file of any class stays in the inventory —
//! and `Unknown` is treated like `Content`, because a wrong "mechanical"
//! verdict on a real one-line fix would cost the reader the fix.

use once_cell::sync::Lazy;
use regex::Regex;
use schemars::JsonSchema;
use serde::Serialize;

use crate::types::DiffHunk;

#[derive(Serialize, JsonSchema, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// Hand-written, multi-line or otherwise substantive.
    Content,
    /// Not classified with confidence; ranked with `Content`.
    Unknown,
    /// One-line pin/tag/version/digest edits of the kind an updater writes.
    Mechanical,
    /// A file whose header says it is generated.
    Generated,
    /// Only whitespace, comments, quote style, trailing commas, semicolons
    /// or import order changed: a formatter run, not a change a caller sees.
    Layout,
}

impl ChangeClass {
    /// Lower sorts first when evidence competes for budget.
    pub fn priority(self) -> u8 {
        match self {
            ChangeClass::Content | ChangeClass::Unknown => 0,
            ChangeClass::Mechanical | ChangeClass::Layout => 1,
            ChangeClass::Generated => 2,
        }
    }
}

/// A single-line change that names a version, a tag, a digest or a hash.
static MECHANICAL_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?ix)
        (^\s*(tag|version|rev|ref|sha|digest|image|newTag|newName|commit|checksum|integrity)\s*[:=])
        | (sha256:[0-9a-f]{12,})
        | (\bv?\d+\.\d+\.\d+(?:[-+.][0-9A-Za-z.]+)?\b)
        | (\b[0-9a-f]{7,64}\b)
        "#,
    )
    .expect("mechanical line regex")
});

pub fn classify(
    hunks: &[&DiffHunk],
    changed_lines: &[String],
    generated: bool,
    layout: bool,
) -> (ChangeClass, &'static str) {
    if generated {
        return (ChangeClass::Generated, "generated-file header");
    }
    if hunks.is_empty() {
        return (ChangeClass::Unknown, "no hunks");
    }
    if layout {
        return (
            ChangeClass::Layout,
            "whitespace, comments, quotes or import order only",
        );
    }
    let single_line = hunks.iter().all(|h| h.new_len <= 1 && h.old_len <= 1);
    if !single_line {
        return (ChangeClass::Content, "multi-line hunks");
    }
    if changed_lines.is_empty() {
        return (ChangeClass::Unknown, "single-line hunks, no diff text");
    }
    if changed_lines.iter().all(|l| MECHANICAL_LINE_RE.is_match(l)) {
        (
            ChangeClass::Mechanical,
            "single-line version/tag/digest edits",
        )
    } else {
        (ChangeClass::Unknown, "single-line hunks")
    }
}

/// One run of changed lines of a unified diff with its text, for what the
/// line ranges alone cannot say: whether the code or only its layout
/// changed, and what the change removed. Context lines split a hunk into
/// runs, so the ranges are the ones `-U0` would print.
#[derive(Clone, Debug, Default)]
pub struct HunkText {
    /// The new-side path as the diff prints it (repository-relative); the
    /// old-side path for a deleted file.
    pub path: String,
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub removed: Vec<String>,
    pub added: Vec<String>,
    /// The unchanged line right above the run, when the diff shows one: a
    /// docstring is told from data by the `def`/`class` line it follows.
    pub before: Option<String>,
    /// The unchanged line right below the run: whether it continues the
    /// run's last statement (a `(`/`[` line after a removed `;`).
    pub after: Option<String>,
}

impl HunkText {
    /// The new-side lines the run occupies; a pure removal anchors on the
    /// new line right above it, as `-U0` numbers it.
    pub fn new_range(&self) -> (u32, u32) {
        if self.new_len == 0 {
            (self.new_start.max(1), self.new_start.max(1))
        } else {
            (self.new_start, self.new_start + self.new_len - 1)
        }
    }

    pub fn is_layout(&self) -> bool {
        layout_equal(
            &self.path,
            self.before.as_deref(),
            self.after.as_deref(),
            &self.removed,
            &self.added,
        )
    }
}

fn hunk_start(spec: &str) -> Option<u32> {
    spec.split(',').next()?.parse().ok()
}

pub fn hunk_texts(diff_text: &str) -> Vec<HunkText> {
    let mut out: Vec<HunkText> = Vec::new();
    let mut old_path: Option<String> = None;
    let mut new_path: Option<String> = None;
    // `"b/src/caf\303\251.js"`: git quotes a non-ASCII path, and a quoted
    // path matched no fragment, so every check on that file went dark.
    let side = |rest: &str, prefix: &str| {
        let p = crate::git::unquote_c_style(rest.split('\t').next().unwrap_or(""));
        (p != "/dev/null").then(|| p.strip_prefix(prefix).unwrap_or(&p).to_string())
    };
    // The next old and new line numbers, and whether the last body line
    // extended the current run.
    let mut old_no = 0u32;
    let mut new_no = 0u32;
    let mut open = false;
    let mut in_hunk = false;
    let mut last_context: Option<String> = None;
    for line in diff_text.lines() {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let mut parts = rest.split_whitespace();
            let old = parts.next().and_then(hunk_start);
            let new = parts
                .next()
                .and_then(|n| n.strip_prefix('+'))
                .and_then(hunk_start);
            if let (Some(o), Some(n)) = (old, new) {
                old_no = o;
                new_no = n;
                in_hunk = true;
                open = false;
                last_context = None;
            }
            continue;
        }
        if line.starts_with("diff ") {
            old_path = None;
            new_path = None;
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if let Some(rest) = line.strip_prefix("--- ") {
                old_path = side(rest, "a/");
            } else if let Some(rest) = line.strip_prefix("+++ ") {
                new_path = side(rest, "b/");
            }
            continue;
        }
        let (removed, text) = match line.as_bytes().first() {
            Some(b'-') => (true, &line[1..]),
            Some(b'+') => (false, &line[1..]),
            Some(b'\\') => continue,
            _ => {
                old_no += 1;
                new_no += 1;
                let text = line.get(1..).unwrap_or("").to_string();
                if open {
                    if let Some(h) = out.last_mut() {
                        h.after = Some(text.clone());
                    }
                }
                open = false;
                last_context = Some(text);
                continue;
            }
        };
        if !open {
            // A removal-only run sits after the new line before it, as -U0
            // numbers it.
            out.push(HunkText {
                path: new_path
                    .clone()
                    .or_else(|| old_path.clone())
                    .unwrap_or_default(),
                old_start: old_no,
                new_start: new_no,
                before: last_context.take(),
                ..HunkText::default()
            });
            open = true;
        }
        let h = out.last_mut().expect("an open run");
        if removed {
            h.removed.push(text.to_string());
            h.old_len += 1;
            old_no += 1;
        } else {
            if h.new_len == 0 {
                h.new_start = new_no;
            }
            h.added.push(text.to_string());
            h.new_len += 1;
            new_no += 1;
        }
    }
    for h in &mut out {
        if h.new_len == 0 {
            h.new_start = h.new_start.saturating_sub(1);
        }
        if h.old_len == 0 {
            h.old_start = h.old_start.saturating_sub(1);
        }
    }
    out
}

/// Whether two versions of some lines differ only in what a formatter
/// changes. Tokens are compared, so whitespace never joins two words. Import
/// statements compare as a set of statements, each with its names sorted —
/// organising imports reorders both, and swapping a name between two
/// statements is not that. A triple-quoted string right after a `def`/`class`
/// header is a docstring and counts as a comment; anywhere else it is data.
/// A comment marker counts only in a language that has it: `//` is floor
/// division in Python and part of a URL in YAML. Where indentation is syntax,
/// the nesting of the lines (the unchanged line above them included) must
/// match: a consistent reindent keeps it, a line leaving its block does not.
pub fn layout_equal(
    path: &str,
    before: Option<&str>,
    after: Option<&str>,
    old: &[String],
    new: &[String],
) -> bool {
    let ext = std::path::Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let ext = ext.as_str();
    let syntax = CommentSyntax {
        hash: HASH_COMMENT_EXTENSIONS.contains(&ext),
        hash_mid_word: HASH_MID_WORD_EXTENSIONS.contains(&ext),
        slash: SLASH_COMMENT_EXTENSIONS.contains(&ext),
        python: matches!(ext, "py" | "pyi"),
        asi: ASI_EXTENSIONS.contains(&ext),
        indented: INDENTED_EXTENSIONS.contains(&ext),
    };
    layout_form(old, syntax, before, after) == layout_form(new, syntax, before, after)
}

#[derive(Clone, Copy)]
struct CommentSyntax {
    hash: bool,
    /// `#` starts a comment even inside a word (`x=1#c`); elsewhere only at
    /// a word start, so `${#a}` and `$#array` stay code.
    hash_mid_word: bool,
    slash: bool,
    /// Docstrings, and a one-element tuple's comma.
    python: bool,
    /// A line opening with `(`, `[` or a template continues the line above
    /// unless a `;` ended it.
    asi: bool,
    indented: bool,
}

const HASH_COMMENT_EXTENSIONS: &[&str] = &[
    "py", "pyi", "rb", "sh", "bash", "zsh", "pl", "r", "jl", "ex", "exs", "nim", "yml", "yaml",
    "toml", "mk", "cmake",
];

const HASH_MID_WORD_EXTENSIONS: &[&str] =
    &["py", "pyi", "rb", "toml", "nim", "jl", "ex", "exs", "r"];

const SLASH_COMMENT_EXTENSIONS: &[&str] = &[
    "c", "h", "cc", "cpp", "cxx", "hpp", "hh", "cs", "java", "kt", "kts", "scala", "sc", "groovy",
    "gradle", "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "go", "rs", "swift", "dart",
    "php", "css", "scss", "less", "proto", "zig", "m", "mm", "vue", "svelte",
];

const ASI_EXTENSIONS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "vue", "svelte",
];

const INDENTED_EXTENSIONS: &[&str] = &[
    "py", "pyi", "yml", "yaml", "nim", "coffee", "sass", "pug", "haml", "fs", "fsx",
];

const TRIPLE_QUOTES: [&str; 2] = ["\"\"\"", "'''"];

/// Imports as a set, the code's tokens, and the nesting of its lines.
type LayoutForm = (Vec<Vec<String>>, Vec<String>, Vec<usize>);

fn indent_width(line: &str) -> usize {
    line.chars()
        .take_while(|c| c.is_whitespace())
        .map(|c| if c == '\t' { 8 } else { 1 })
        .sum()
}

fn is_def_header(line: &str) -> bool {
    let line = line.trim();
    (line.starts_with("def ") || line.starts_with("async def ") || line.starts_with("class "))
        && line.ends_with(':')
}

fn layout_form(
    lines: &[String],
    syntax: CommentSyntax,
    before: Option<&str>,
    after: Option<&str>,
) -> LayoutForm {
    let mut imports: Vec<Vec<String>> = Vec::new();
    let mut statement: Vec<String> = Vec::new();
    let mut code: Vec<String> = Vec::new();
    let mut in_import = false;
    let mut depth = 0i32;
    let mut docstring: Option<&str> = None;
    let mut block_comment = false;
    let mut after_header = before.is_some_and(is_def_header);
    // Widths of the lines that start a statement, the line above the run
    // first: their ranks are the nesting a formatter cannot change.
    let mut widths: Vec<usize> = before
        .filter(|b| syntax.indented && !b.trim().is_empty())
        .map(indent_width)
        .into_iter()
        .collect();
    for line in lines {
        let trimmed = line.trim_start();
        if let Some(q) = docstring {
            if trimmed.contains(q) {
                docstring = None;
            }
            continue;
        }
        if syntax.python && after_header {
            if let Some(q) = TRIPLE_QUOTES.into_iter().find(|q| trimmed.starts_with(q)) {
                if !trimmed[3..].contains(q) {
                    docstring = Some(q);
                }
                after_header = false;
                continue;
            }
        }
        let tokens = layout_tokens(line, syntax, &mut block_comment);
        if tokens.is_empty() {
            continue;
        }
        after_header = matches!(tokens[0].as_str(), "def" | "class" | "async")
            && tokens.last().is_some_and(|t| t == ":");
        if syntax.indented && depth == 0 {
            widths.push(indent_width(line));
        }
        let continues_above = syntax.asi
            && depth == 0
            && (matches!(tokens[0].as_str(), "(" | "[") || tokens[0].starts_with('`'))
            && code
                .last()
                .is_some_and(|t| t != ";" && t != "{" && t != "}");
        let starts_import = matches!(tokens[0].as_str(), "import" | "from" | "use" | "require")
            || (tokens[0] == "#" && tokens.get(1).is_some_and(|t| t == "include"))
            || (tokens[0] == "export" && tokens.iter().any(|t| t == "from"));
        if starts_import && depth == 0 {
            in_import = true;
        }
        for t in &tokens {
            match t.as_str() {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth -= 1,
                _ => {}
            }
        }
        if in_import {
            statement.extend(tokens);
            if depth <= 0 {
                imports.push(import_statement(std::mem::take(&mut statement), syntax));
                in_import = false;
                depth = 0;
            }
        } else {
            if continues_above {
                code.push("<continues>".to_string());
            }
            code.extend(tokens);
        }
    }
    if !statement.is_empty() {
        imports.push(import_statement(statement, syntax));
    }
    let next_continues = after.is_some_and(|a| {
        let a = a.trim_start();
        a.starts_with('(') || a.starts_with('[') || a.starts_with('`')
    });
    if syntax.asi
        && next_continues
        && code
            .last()
            .is_some_and(|t| t != ";" && t != "{" && t != "}")
    {
        code.push("<continues>".to_string());
    }
    imports.sort();
    let mut levels: Vec<usize> = widths.clone();
    levels.sort_unstable();
    levels.dedup();
    let nesting = widths
        .iter()
        .map(|w| levels.binary_search(w).unwrap_or(0))
        .collect();
    (imports, drop_separators(code, syntax), nesting)
}

/// One import statement with its imported names in a fixed order: what
/// organise-imports may reorder (`{ b, a }`, `import b, a`) compares equal,
/// the module each name comes from does not move.
fn import_statement(tokens: Vec<String>, syntax: CommentSyntax) -> Vec<String> {
    let tokens = drop_separators(tokens, syntax);
    let split = tokens
        .iter()
        .position(|t| t == "{")
        .or_else(|| tokens.iter().position(|t| t == "import").map(|i| i + 1));
    let Some(at) = split else {
        return tokens;
    };
    let close = tokens[at..]
        .iter()
        .position(|t| t == "}")
        .map_or(tokens.len(), |c| at + c);
    let mut names: Vec<String> = tokens[at..close]
        .iter()
        .filter(|t| !matches!(t.as_str(), "{" | "," | "(" | ")"))
        .cloned()
        .collect();
    names.sort();
    let mut out: Vec<String> = tokens[..at].to_vec();
    out.extend(names);
    out.extend(tokens[close..].iter().cloned());
    out
}

/// `;` is optional where formatters disagree, and a comma before a closing
/// bracket is a trailing comma — except the one comma of a Python tuple,
/// `(x,)`, which is what makes it a tuple.
fn drop_separators(tokens: Vec<String>, syntax: CommentSyntax) -> Vec<String> {
    // Per open bracket: whether it is a bare `(` (a tuple, not a call) and
    // how many commas it has held.
    let mut groups: Vec<(bool, usize)> = Vec::new();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    for (i, t) in tokens.iter().enumerate() {
        match t.as_str() {
            "(" | "[" | "{" => {
                let callee = i.checked_sub(1).is_some_and(|p| {
                    let p = &tokens[p];
                    p == ")"
                        || p == "]"
                        || p.chars()
                            .next()
                            .is_some_and(|c| c.is_alphanumeric() || c == '_')
                });
                groups.push((t == "(" && !callee, 0));
            }
            ")" | "]" | "}" => {
                groups.pop();
            }
            "," => {
                if let Some(g) = groups.last_mut() {
                    g.1 += 1;
                }
            }
            _ => {}
        }
        let closes_next = tokens
            .get(i + 1)
            .is_some_and(|n| matches!(n.as_str(), ")" | "]" | "}"));
        let tuple_comma = syntax.python
            && groups
                .last()
                .is_some_and(|&(bare, commas)| bare && commas == 1);
        if t == ";" || (t == "," && closes_next && !tuple_comma) {
            continue;
        }
        out.push(t.clone());
    }
    out
}

/// Words, string literals (single and double quotes as one style; a
/// template literal stays its own), and every other non-space character,
/// with the language's comments dropped.
fn layout_tokens(line: &str, syntax: CommentSyntax, block_comment: &mut bool) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if *block_comment {
            if c == '*' && chars.get(i + 1) == Some(&'/') {
                *block_comment = false;
                i += 1;
            }
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let word_start = i == 0 || chars[i - 1].is_whitespace() || chars[i - 1] == ';';
        if (syntax.slash && c == '/' && chars.get(i + 1) == Some(&'/'))
            || (syntax.hash && c == '#' && (syntax.hash_mid_word || word_start))
        {
            break;
        }
        if syntax.slash && c == '/' && chars.get(i + 1) == Some(&'*') {
            *block_comment = true;
            i += 2;
            continue;
        }
        if matches!(c, '"' | '\'' | '`') {
            let quote = if c == '`' { '`' } else { '"' };
            let mut lit = String::from(quote);
            i += 1;
            while i < chars.len() && chars[i] != c {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    lit.push(chars[i]);
                    i += 1;
                }
                lit.push(chars[i]);
                i += 1;
            }
            lit.push(quote);
            out.push(lit);
            i += 1;
            continue;
        }
        if c.is_alphanumeric() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
            continue;
        }
        out.push(c.to_string());
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn hunk(new_len: u32, old_len: u32) -> DiffHunk {
        DiffHunk {
            path: Arc::from("x"),
            new_start: 1,
            new_len,
            old_start: 1,
            old_len,
        }
    }

    #[test]
    fn a_tag_bump_is_mechanical_and_a_one_line_fix_is_unknown() {
        let h = hunk(1, 1);
        let bump = vec![
            "    tag: main-abc1234".into(),
            "    tag: main-def5678".into(),
        ];
        assert_eq!(
            classify(&[&h], &bump, false, false).0,
            ChangeClass::Mechanical
        );
        let fix = vec!["    return x + 1".into(), "    return x + 2".into()];
        assert_eq!(classify(&[&h], &fix, false, false).0, ChangeClass::Unknown);
        let digest = vec!["image: r/app@sha256:0123456789abcdef".into()];
        assert_eq!(
            classify(&[&h], &digest, false, false).0,
            ChangeClass::Mechanical
        );
    }

    #[test]
    fn multi_line_hunks_are_content_and_generated_wins() {
        let h = hunk(5, 2);
        assert_eq!(classify(&[&h], &[], false, false).0, ChangeClass::Content);
        assert_eq!(classify(&[&h], &[], true, false).0, ChangeClass::Generated);
        assert!(ChangeClass::Unknown.priority() == ChangeClass::Content.priority());
        assert!(ChangeClass::Mechanical.priority() > ChangeClass::Content.priority());
    }

    #[test]
    fn hunk_texts_carry_both_sides_under_the_surviving_path() {
        let diff = "diff --git a/a.yaml b/a.yaml\n--- a/a.yaml\n+++ b/a.yaml\n@@ -1 +1 @@\n-tag: 1\n+tag: 2\ndiff --git a/gone b/gone\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
        let hunks = hunk_texts(diff);
        assert_eq!(hunks[0].path, "a.yaml");
        assert_eq!(hunks[0].removed, vec!["tag: 1".to_string()]);
        assert_eq!(hunks[0].added, vec!["tag: 2".to_string()]);
        assert_eq!((hunks[1].path.as_str(), hunks[1].new_len), ("gone", 0));
        let context = "--- a/f\n+++ b/f\n@@ -1,6 +1,6 @@\n a\n-b\n+B\n c\n d\n-e\n+E\n f\n";
        let runs: Vec<(u32, u32)> = hunk_texts(context).iter().map(|h| h.new_range()).collect();
        assert_eq!(runs, vec![(2, 2), (5, 5)]);
    }
}
