use std::sync::Arc;

use rustc_hash::FxHashSet;

use crate::types::{Fragment, FragmentId, FragmentKind};

const MIN_LINES_FOR_SIGNATURE: u32 = 5;

fn is_signature_eligible(kind: FragmentKind) -> bool {
    matches!(
        kind,
        FragmentKind::Function
            | FragmentKind::Class
            | FragmentKind::Struct
            | FragmentKind::Interface
            | FragmentKind::Enum
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

fn find_signature_end(lines: &[&str]) -> usize {
    let mut paren_depth = 0i32;
    let mut seen_open_paren = false;

    for (i, line) in lines.iter().enumerate() {
        let (op, cp, ob, cb) = count_brackets_outside_strings(line);
        paren_depth += op - cp;
        if op > 0 {
            seen_open_paren = true;
        }
        if ob - cb > 0 {
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
        if frag.line_count() < MIN_LINES_FOR_SIGNATURE {
            continue;
        }
        let lines: Vec<&str> = frag.content.lines().collect();
        let sig_end = find_signature_end(&lines);
        let sig_content: String = lines[..sig_end].join("\n");
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
