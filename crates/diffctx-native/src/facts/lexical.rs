//! Where a blob's bytes are not code, read once off the tree an extractor parsed:
//! one walk for every language, the grammar's node kind names as data. A string
//! literal is recorded as the pieces around its code (an f-string's `{x}`, a
//! template's `${x}`), never whole.

use std::ops::Range;

use tree_sitter::Node;

use crate::facts::language::Language;
use crate::facts::model::{BlobFacts, Lexical};

type Ranges = Vec<(Range<u32>, Lexical)>;

/// What one grammar calls the nodes of `lexical`, each a space-separated list of
/// node kinds. A language with no row has every byte as code: no tree here, or
/// strings that name things (Shell and HCL keep their comments out, Shell its heredoc bodies).
struct Row {
    languages: &'static [Language],
    comments: &'static str,
    /// A literal. Its named children that are not `text` are code: a kind left
    /// out of `text` only widens what an occurrence can be.
    strings: &'static str,
    text: &'static str,
    /// Ancestors whose literals are code: a Rust macro takes `{name}` in them.
    code_in: &'static str,
}

#[rustfmt::skip]
const TABLE: &[Row] = &[
    Row { languages: &[Language::Python], comments: "comment", strings: "string", text: "string_start string_content string_end", code_in: "" },
    Row { languages: &[Language::JavaScript, Language::TypeScript, Language::Html, Language::Astro], comments: "comment html_comment", strings: "string template_string template_literal_type", text: "string_fragment escape_sequence html_character_reference", code_in: "" },
    Row { languages: &[Language::Rust], comments: "line_comment block_comment", strings: "string_literal raw_string_literal char_literal", text: "string_content escape_sequence", code_in: "token_tree" },
    Row { languages: &[Language::Kotlin], comments: "line_comment multiline_comment", strings: "string_literal character_literal", text: "string_content character_escape_seq", code_in: "" },
    Row { languages: &[Language::Java], comments: "line_comment block_comment", strings: "string_literal character_literal", text: "string_fragment multiline_string_fragment escape_sequence", code_in: "" },
    Row { languages: &[Language::Go], comments: "comment", strings: "interpreted_string_literal raw_string_literal rune_literal", text: "interpreted_string_literal_content raw_string_literal_content escape_sequence", code_in: "" },
    Row { languages: &[Language::Php], comments: "comment", strings: "string encapsed_string heredoc_body nowdoc_body", text: "string_content escape_sequence nowdoc_string", code_in: "" },
    Row { languages: &[Language::C, Language::Cpp], comments: "comment", strings: "string_literal raw_string_literal system_lib_string char_literal", text: "string_content raw_string_content raw_string_delimiter character escape_sequence", code_in: "" },
    Row { languages: &[Language::Swift], comments: "comment multiline_comment", strings: "line_string_literal multi_line_string_literal raw_string_literal", text: "line_str_text multi_line_str_text str_escaped_char raw_str_part raw_str_end_part raw_str_continuing_indicator", code_in: "" },
    Row { languages: &[Language::Shell], comments: "comment", strings: "heredoc_body", text: "heredoc_content", code_in: "" },
    Row { languages: &[Language::Terraform], comments: "comment", strings: "", text: "", code_in: "" },
];

fn listed(kinds: &str, kind: &str) -> bool {
    kinds.split(' ').any(|listed| listed == kind)
}

impl BlobFacts {
    /// Records the comments and strings of `root`, the tree this blob's language
    /// was parsed into.
    pub(crate) fn record_lexical(&mut self, root: Node<'_>) {
        if let Some(kinds) = TABLE
            .iter()
            .find(|row| row.languages.contains(&self.language))
        {
            self.lexical = scan(root, kinds);
        }
    }
}

fn scan(root: Node<'_>, kinds: &Row) -> Ranges {
    let mut found = Vec::new();
    let mut stack = vec![(root, false)];
    while let Some((node, in_code)) = stack.pop() {
        let kind = node.kind();
        if listed(kinds.comments, kind) {
            record(
                &mut found,
                node.start_byte(),
                node.end_byte(),
                Lexical::Comment,
            );
        } else if listed(kinds.strings, kind) {
            if !in_code {
                pieces(node, kinds, &mut found, &mut stack);
            }
        } else {
            let in_code = in_code || listed(kinds.code_in, kind);
            let mut cursor = node.walk();
            stack.extend(node.children(&mut cursor).map(|child| (child, in_code)));
        }
    }
    found.sort_unstable_by_key(|(range, _)| range.start);
    found
}

/// A literal's bytes between its code, which goes back on the walk: a string
/// inside an interpolation is a string of its own.
fn pieces<'t>(
    literal: Node<'t>,
    kinds: &Row,
    found: &mut Ranges,
    stack: &mut Vec<(Node<'t>, bool)>,
) {
    let mut from = literal.start_byte();
    let mut cursor = literal.walk();
    for code in literal
        .children(&mut cursor)
        .filter(|child| child.is_named() && !listed(kinds.text, child.kind()))
    {
        record(found, from, code.start_byte(), Lexical::String);
        from = code.end_byte();
        stack.push((code, false));
    }
    record(found, from, literal.end_byte(), Lexical::String);
}

fn record(found: &mut Ranges, from: usize, to: usize, kind: Lexical) {
    if from < to {
        found.push((from as u32..to as u32, kind));
    }
}
