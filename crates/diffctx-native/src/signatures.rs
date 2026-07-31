use std::sync::Arc;

use rustc_hash::FxHashSet;

use crate::config::graph_filtering::GRAPH_FILTERING;
use crate::types::{Fragment, FragmentId, FragmentKind};

fn is_signature_eligible(kind: FragmentKind) -> bool {
    // Variable covers TS/JS arrow-function bindings (`const f = (...) => {...}`);
    // without a stub variant a large changed arrow function that misses the core
    // budget vanishes from the output entirely (#106).
    matches!(
        kind,
        FragmentKind::Function
            | FragmentKind::Class
            | FragmentKind::Struct
            | FragmentKind::Interface
            | FragmentKind::Enum
            | FragmentKind::Variable
    )
}

fn signature_kind(kind: FragmentKind) -> FragmentKind {
    match kind {
        FragmentKind::Function => FragmentKind::FunctionSignature,
        FragmentKind::Class => FragmentKind::ClassSignature,
        FragmentKind::Struct => FragmentKind::StructSignature,
        FragmentKind::Interface => FragmentKind::InterfaceSignature,
        FragmentKind::Enum => FragmentKind::EnumSignature,
        _ => FragmentKind::FunctionSignature,
    }
}

fn count_brackets_outside_strings(line: &str) -> (i32, i32, i32, i32) {
    let mut open_parens = 0i32;
    let mut close_parens = 0i32;
    let mut open_braces = 0i32;
    let mut close_braces = 0i32;
    let mut in_string: Option<char> = None;
    let mut escaped = false;

    for ch in line.chars() {
        if let Some(quote) = in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                in_string = None;
            }
            continue;
        }
        match ch {
            '\'' | '"' | '`' => {
                in_string = Some(ch);
                escaped = false;
            }
            '(' => open_parens += 1,
            ')' => close_parens += 1,
            '{' => open_braces += 1,
            '}' => close_braces += 1,
            _ => {}
        }
    }

    (open_parens, close_parens, open_braces, close_braces)
}

fn decorator_prefix_len(lines: &[&str]) -> usize {
    let mut i = 0;
    let mut paren_depth = 0i32;
    while i < lines.len() {
        let trimmed = lines[i].trim_start();
        let starts_decorator = trimmed.starts_with('@') || trimmed.starts_with("#[");
        if paren_depth <= 0 && !starts_decorator {
            break;
        }
        let (op, cp, _, _) = count_brackets_outside_strings(lines[i]);
        paren_depth += op - cp;
        i += 1;
    }
    if i >= lines.len() { 0 } else { i }
}

fn find_signature_end(lines: &[&str]) -> usize {
    let mut paren_depth = 0i32;
    let mut seen_open_paren = false;

    for (i, line) in lines.iter().enumerate() {
        let (op, cp, ob, cb) = count_brackets_outside_strings(line);
        paren_depth += op - cp;
        if op > 0 {
            seen_open_paren = true;
        }
        // A body-opening brace only ends the signature once we are outside the
        // parameter list. Braces inside parameter defaults or annotations
        // (e.g. Python `def f(x={}):`) appear while `paren_depth > 0` and must
        // not truncate the signature mid-parameter-list.
        if paren_depth <= 0 && ob - cb > 0 {
            return i + 1;
        }
        if seen_open_paren && paren_depth <= 0 {
            return i + 1;
        }
    }

    2.min(lines.len())
}

pub fn generate_signature_variants(fragments: &[Fragment]) -> Vec<Fragment> {
    let mut signatures: Vec<Fragment> = Vec::new();
    let mut seen: FxHashSet<FragmentId> = FxHashSet::default();

    for frag in fragments {
        if !is_signature_eligible(frag.kind) {
            continue;
        }
        if frag.line_count() < GRAPH_FILTERING.min_lines_for_signature {
            continue;
        }
        let lines: Vec<&str> = frag.content.lines().collect();
        // The eligibility gate above measures the id's line span; everything
        // below indexes the actual text. A fragment whose span says five lines
        // while its content holds none would yield `sig_end = 0`, hence an id
        // whose end precedes its start — and `line_count()` on that id is a
        // subtraction overflow for every later stage that touches it.
        if lines.is_empty() {
            continue;
        }
        let decorators = decorator_prefix_len(&lines);
        let sig_end = (decorators + find_signature_end(&lines[decorators..])).max(1);
        let sig_content: String = lines[..sig_end.min(lines.len())].join("\n");
        let sig_end_line = frag.start_line() + sig_end as u32 - 1;
        let sig_id = FragmentId::new(frag.id.path.clone(), frag.start_line(), sig_end_line);

        if seen.contains(&sig_id) {
            continue;
        }
        seen.insert(sig_id.clone());

        signatures.push(Fragment {
            id: sig_id,
            kind: signature_kind(frag.kind),
            content: Arc::from(sig_content),
            identifiers: frag.identifiers.clone(),
            token_count: 0,
            symbol_name: frag.symbol_name.clone(),
        });
    }

    signatures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(kind: FragmentKind, start: u32, content: &str) -> Fragment {
        let line_count = content.lines().count() as u32;
        Fragment {
            id: FragmentId::new(Arc::from("a.src"), start, start + line_count - 1),
            kind,
            content: Arc::from(content),
            identifiers: FxHashSet::default(),
            token_count: 100,
            symbol_name: Some("target".into()),
        }
    }

    fn stub_of(f: &Fragment) -> String {
        let sigs = generate_signature_variants(std::slice::from_ref(f));
        assert_eq!(sigs.len(), 1, "expected exactly one signature variant");
        sigs[0].content.to_string()
    }

    #[test]
    fn python_default_containing_braces_does_not_truncate_the_parameter_list() {
        // `x={}` puts a brace inside the parameter list; truncating there would
        // ship a syntactically broken stub.
        let f = frag(
            FragmentKind::Function,
            1,
            "def f(\n    x={},\n    y=1,\n):\n    body_one()\n    body_two()\n",
        );
        let stub = stub_of(&f);
        assert!(stub.contains("x={}"), "stub lost a parameter: {stub:?}");
        assert!(stub.contains("y=1"), "stub lost a parameter: {stub:?}");
        assert!(
            stub.trim_end().ends_with("):"),
            "stub is not closed: {stub:?}"
        );
        assert!(!stub.contains("body_one"), "stub leaked the body: {stub:?}");
    }

    #[test]
    fn open_paren_inside_a_string_literal_does_not_extend_the_signature() {
        let f = frag(
            FragmentKind::Function,
            1,
            "def f(sep=\"a(b\"):\n    one()\n    two()\n    three()\n    four()\n",
        );
        let stub = stub_of(&f);
        assert!(
            stub.contains("sep=\"a(b\""),
            "stub mangled the literal: {stub:?}"
        );
        assert!(!stub.contains("one()"), "stub leaked the body: {stub:?}");
    }

    #[test]
    fn rust_attribute_prefix_is_kept_above_the_signature() {
        let f = frag(
            FragmentKind::Struct,
            10,
            "#[derive(Debug, Clone)]\npub struct S {\n    a: u32,\n    b: u32,\n    c: u32,\n}\n",
        );
        let stub = stub_of(&f);
        assert!(
            stub.starts_with("#[derive(Debug, Clone)]"),
            "lost the attribute: {stub:?}"
        );
        assert!(stub.contains("pub struct S {"), "lost the header: {stub:?}");
        assert!(!stub.contains("a: u32"), "stub leaked the body: {stub:?}");
    }

    #[test]
    fn multiline_decorator_prefix_is_kept_whole() {
        let f = frag(
            FragmentKind::Function,
            1,
            "@retry(\n    times=3,\n)\ndef f(x):\n    one()\n    two()\n",
        );
        let stub = stub_of(&f);
        assert!(stub.starts_with("@retry("), "lost the decorator: {stub:?}");
        assert!(
            stub.contains("times=3"),
            "truncated the decorator: {stub:?}"
        );
        assert!(stub.contains("def f(x):"), "lost the signature: {stub:?}");
        assert!(!stub.contains("one()"), "stub leaked the body: {stub:?}");
    }

    #[test]
    fn signature_span_matches_the_emitted_line_count() {
        // sig_end_line is derived arithmetically from start_line; if it drifts,
        // the stub's FragmentId claims lines it does not contain and the
        // interval index deduplicates against the wrong span.
        let f = frag(
            FragmentKind::Function,
            42,
            "def f(\n    x,\n):\n    one()\n    two()\n    three()\n",
        );
        let sigs = generate_signature_variants(std::slice::from_ref(&f));
        let sig = &sigs[0];
        assert_eq!(sig.id.start_line, 42);
        assert_eq!(
            sig.id.end_line - sig.id.start_line + 1,
            sig.content.lines().count() as u32
        );
    }

    #[test]
    fn kind_maps_to_its_signature_variant_and_ineligible_kinds_are_skipped() {
        let body = "\nline\nline\nline\nline\nline\n";
        for (kind, expected) in [
            (FragmentKind::Function, FragmentKind::FunctionSignature),
            (FragmentKind::Class, FragmentKind::ClassSignature),
            (FragmentKind::Struct, FragmentKind::StructSignature),
            (FragmentKind::Interface, FragmentKind::InterfaceSignature),
            (FragmentKind::Enum, FragmentKind::EnumSignature),
            // #106: TS/JS arrow bindings must get a stub or a large changed
            // arrow function disappears from the output entirely.
            (FragmentKind::Variable, FragmentKind::FunctionSignature),
        ] {
            let sigs = generate_signature_variants(&[frag(kind, 1, body)]);
            assert_eq!(sigs.len(), 1, "{kind:?} produced no signature");
            assert_eq!(sigs[0].kind, expected, "{kind:?} mapped wrong");
        }
        assert!(generate_signature_variants(&[frag(FragmentKind::Chunk, 1, body)]).is_empty());
        assert!(generate_signature_variants(&[frag(FragmentKind::Module, 1, body)]).is_empty());
    }

    #[test]
    fn fragments_shorter_than_the_threshold_get_no_stub() {
        let short = frag(FragmentKind::Function, 1, "def f():\n    one()\n");
        assert!(generate_signature_variants(&[short]).is_empty());
    }

    #[test]
    fn duplicate_signature_spans_are_emitted_once() {
        let a = frag(
            FragmentKind::Function,
            1,
            "def f(x):\n    a()\n    b()\n    c()\n    d()\n",
        );
        let b = frag(
            FragmentKind::Function,
            1,
            "def f(x):\n    a()\n    b()\n    c()\n    e()\n",
        );
        assert_eq!(generate_signature_variants(&[a, b]).len(), 1);
    }

    #[test]
    fn signatureless_content_falls_back_to_a_bounded_prefix() {
        // No parens and no brace at all: the fallback must stay inside the
        // fragment rather than claiming the whole body.
        let f = frag(
            FragmentKind::Class,
            1,
            "class C:\n    x = 1\n    y = 2\n    z = 3\n    w = 4\n",
        );
        let stub = stub_of(&f);
        assert!(
            stub.lines().count() <= 2,
            "fallback took too much: {stub:?}"
        );
        assert!(stub.starts_with("class C:"));
    }

    /// Eligibility is decided from the id's line span, the signature is cut
    /// from the content. A span claiming lines that the text does not have
    /// produced an id whose end precedes its start, and `line_count()` on such
    /// an id is a subtraction overflow in every stage downstream.
    #[test]
    fn a_span_wider_than_its_content_yields_no_malformed_signature() {
        let empty_body = Fragment {
            id: FragmentId::new(Arc::from("a.src"), 10, 40),
            kind: FragmentKind::Function,
            content: Arc::from(""),
            identifiers: FxHashSet::default(),
            token_count: 100,
            symbol_name: Some("target".into()),
        };

        for sig in generate_signature_variants(&[empty_body]) {
            assert!(
                sig.end_line() >= sig.start_line(),
                "signature id {:?} ends before it starts",
                sig.id
            );
            assert!(sig.line_count() >= 1);
        }
    }
}
