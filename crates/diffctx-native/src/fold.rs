//! Which rows of a changed definition the change cannot have affected, by the
//! folding table of the 2.0 plan (Revision 12.1): definition kind x what changed
//! x relation kind x whether its type is declared. A folded row is counted in a
//! line of its own, never dropped; a case the table does not name is listed.
//! When unsure, nothing folds.

#![deny(clippy::disallowed_methods)]

use crate::facts::{DefKind, Language};
use crate::resolve::Relation;

/// Why a row is folded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// An override replaces the body: it does not run it.
    OverrideNotRun,
    /// An import or a re-export names the definition, not what it does.
    ImportsABody,
    /// A type annotation of a definition whose types are written down.
    DeclaredType,
    /// A call that still works: only parameters with defaults were added.
    CheckTheDefault,
    /// A row only a name links, through a receiver nothing types.
    UntypedReceiver,
    /// Only comments changed: the definition's code is as it was.
    NoCodeChange,
}

impl Rule {
    pub const fn name(self) -> &'static str {
        match self {
            Self::OverrideNotRun => "override-not-run",
            Self::ImportsABody => "imports-a-body",
            Self::DeclaredType => "declared-type",
            Self::CheckTheDefault => "check-the-default",
            Self::UntypedReceiver => "untyped-receiver",
            Self::NoCodeChange => "no-code-change",
        }
    }
}

/// What the range did to one definition, as far as the table reads it. Only an
/// edit in place has one: a removal, a rename, a move, a change of visibility
/// and an ambiguous pairing have none, and nothing of them folds. A basis with no
/// signature, body or decorator change is an edit of comments alone: the caller
/// builds none when a docstring changed, which a decorator may read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Basis {
    pub kind: DefKind,
    pub signature: bool,
    pub body: bool,
    pub decorators: bool,
    /// The parameters a signature gained, when that is all it did and each has a
    /// default: the calls that worked still do.
    pub compatible: Option<Vec<String>>,
    /// The definition's types are written down.
    pub typed: bool,
}

/// The rule that folds a row of a definition with this `basis`, if the table has
/// one. `candidate`: the row is a candidate. `runtime`: the row is a Python
/// annotation a framework may execute.
pub fn rule(basis: &Basis, relation: Relation, candidate: bool, runtime: bool) -> Option<Rule> {
    use DefKind::{
        Class, Constant, Constructor, Field, Function, Method, Property, Struct, Variable,
    };
    // A comment is stored nowhere a caller or a decorator can read: the object
    // the decorator wraps and every annotation a framework runs are the same. A
    // candidate keeps its own rule below.
    let code_untouched = !(basis.signature || basis.body || basis.decorators);
    if code_untouched && !candidate && matches!(basis.kind, Function | Method | Constructor | Class)
    {
        return Some(Rule::NoCodeChange);
    }
    if runtime && relation == Relation::TypeUses {
        return None;
    }
    // A candidate is folded as one wherever the table folds anything of the
    // definition: it is not a caller whose kind a rule could judge.
    let foldable_kind = matches!(
        basis.kind,
        DefKind::Function
            | DefKind::Method
            | DefKind::Constructor
            | DefKind::Class
            | DefKind::Struct
            | DefKind::Variable
            | DefKind::Constant
            | DefKind::Field
            | DefKind::Property
    );
    if candidate && foldable_kind {
        return Some(Rule::UntypedReceiver);
    }
    // A dynamic import (`import(…)`, `React.lazy`, `importlib.import_module`) is a
    // use of what it reaches, never an import: it is the only `Imports` row a
    // definition has (a static import is no row, R8), and it is listed. Only a
    // re-export is folded as naming a body.
    let imports = relation == Relation::Reexports;
    match basis.kind {
        Function | Method | Constructor => {
            if basis.decorators {
                imports.then_some(Rule::ImportsABody)
            } else if basis.signature {
                if basis.compatible.is_some()
                    && matches!(relation, Relation::Calls | Relation::Instantiates)
                {
                    Some(Rule::CheckTheDefault)
                } else {
                    imports.then_some(Rule::ImportsABody)
                }
            } else if basis.body {
                match relation {
                    Relation::Overrides => Some(Rule::OverrideNotRun),
                    Relation::TypeUses if basis.typed => Some(Rule::DeclaredType),
                    _ => imports.then_some(Rule::ImportsABody),
                }
            } else {
                None
            }
        }
        Class | Struct => (basis.body && !basis.signature && !basis.decorators && imports)
            .then_some(Rule::ImportsABody),
        Variable | Constant | Field | Property => {
            if basis.signature || basis.decorators {
                None
            } else if relation == Relation::TypeUses && basis.typed {
                Some(Rule::DeclaredType)
            } else {
                imports.then_some(Rule::ImportsABody)
            }
        }
        // A type's body is the type: what it changes, nothing here says it left
        // alone.
        _ => None,
    }
}

/// One parameter of a signature.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Param {
    /// Without whitespace: two parameters are the same when these are equal.
    text: String,
    /// The name; empty for a bare `*` or `/`.
    name: String,
    /// A call may leave it out: it has a default, is optional or takes the rest.
    optional: bool,
}

/// The text before the parameter list, the parameters, and the text after it.
fn parameters(signature: &str, name: &str) -> Option<(String, Vec<Param>, String)> {
    let squeezed = |s: &str| s.split_whitespace().collect::<String>();
    let open = opening_paren(signature, name)?;
    let mut depth = 0usize;
    let mut close = None;
    for (at, c) in signature[open..].char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    close = Some(open + at);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let inner = &signature[open + 1..close];
    let mut params = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let mut previous = ' ';
    for (at, c) in inner.char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            '>' if previous != '-' && previous != '=' => depth -= 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                params.push(param(&inner[start..at]));
                start = at + 1;
            }
            _ => {}
        }
        previous = c;
    }
    if !inner[start..].trim().is_empty() {
        params.push(param(&inner[start..]));
    }
    Some((
        squeezed(&signature[..open]),
        params,
        squeezed(&signature[close + 1..]),
    ))
}

/// The `(` that opens the parameters of the definition called `name`.
fn opening_paren(signature: &str, name: &str) -> Option<usize> {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(found) = signature[from..].find(name) {
        let at = from + found;
        let end = at + name.len();
        let before = signature[..at].chars().next_back();
        let after = &signature[end..];
        from = end;
        if before.is_some_and(word) || after.chars().next().is_some_and(word) {
            continue;
        }
        let rest = after.trim_start();
        let rest = if rest.starts_with('<') {
            let mut depth = 0i32;
            let mut cut = None;
            for (i, c) in rest.char_indices() {
                match c {
                    '<' => depth += 1,
                    '>' => {
                        depth -= 1;
                        if depth == 0 {
                            cut = Some(i + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            rest[cut?..].trim_start()
        } else {
            rest
        };
        if rest.starts_with('(') {
            return Some(signature.len() - rest.len());
        }
    }
    // A constructor named by its keyword.
    signature.find('(')
}

/// Words that stand before a parameter's name and are no part of it.
const MODIFIERS: &[&str] = &[
    "vararg",
    "val",
    "var",
    "mut",
    "readonly",
    "public",
    "private",
    "protected",
    "override",
    "noinline",
    "crossinline",
];

fn param(text: &str) -> Param {
    let squeezed: String = text.split_whitespace().collect();
    let rest_marker = squeezed.starts_with('*') || squeezed.starts_with("...");
    let mut words = text.split_whitespace().peekable();
    let mut vararg = false;
    while let Some(word) = words.next_if(|w| MODIFIERS.contains(w)) {
        vararg |= word == "vararg";
    }
    let remaining: String = words.collect();
    let marked = remaining.trim_start_matches(['*', '.']);
    let name: String = marked
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    let questioned = marked[name.len()..].starts_with('?');
    Param {
        optional: has_default(&squeezed)
            || questioned
            || rest_marker
            || vararg
            || (name.is_empty() && !squeezed.is_empty()),
        text: squeezed,
        name,
    }
}

/// An `=` that assigns a default: not `==`, `=>`, `<=`, `>=` or `!=`.
fn has_default(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(i, b)| {
        *b == b'='
            && bytes.get(i + 1) != Some(&b'=')
            && bytes.get(i + 1) != Some(&b'>')
            && !matches!(
                i.checked_sub(1).map(|p| bytes[p]),
                Some(b'=' | b'<' | b'>' | b'!')
            )
    })
}

/// The parameters `head` gained over `base` when that is all the signature did
/// and a call that worked still does: the base list is a prefix of the head's
/// with the same names, defaults and types, every added parameter has a default,
/// is optional or takes the rest, and what surrounds the list is the same. Never
/// in Rust or Go, which have neither.
pub fn compatible(base: &str, head: &str, name: &str, language: Language) -> Option<Vec<String>> {
    if matches!(language, Language::Rust | Language::Go) {
        return None;
    }
    let (base_before, base_params, base_after) = parameters(base, name)?;
    let (head_before, head_params, head_after) = parameters(head, name)?;
    if base_before != head_before
        || base_after != head_after
        || head_params.len() <= base_params.len()
    {
        return None;
    }
    if !base_params.iter().zip(&head_params).all(|(a, b)| a == b) {
        return None;
    }
    let added = &head_params[base_params.len()..];
    if !added.iter().all(|p| p.optional) {
        return None;
    }
    let names: Vec<String> = added
        .iter()
        .filter(|p| !p.name.is_empty())
        .map(|p| p.name.clone())
        .collect();
    (!names.is_empty()).then_some(names)
}

/// Whether a definition's types are written down: what a body change cannot
/// move. Python: every parameter but `self` and `cls` annotated and a return
/// type; TypeScript: an explicit return type; Rust and Go: always; a variable: an
/// annotation (Go: a type written after the name). Kotlin and the rest: not said, so not declared.
pub fn declared_types(
    kind: DefKind,
    language: Language,
    signature: Option<&str>,
    name: &str,
) -> bool {
    match kind {
        DefKind::Variable | DefKind::Constant | DefKind::Field | DefKind::Property => {
            match language {
                Language::Rust => true,
                // `x int =`: a name and a type; `x =`: the type is inferred.
                Language::Go => signature
                    .is_some_and(|s| s.split_whitespace().filter(|t| *t != "=").count() > 1),
                _ => signature.is_some_and(|s| s.contains(':')),
            }
        }
        DefKind::Function | DefKind::Method | DefKind::Constructor => match language {
            Language::Rust | Language::Go => true,
            Language::Python => signature.is_some_and(|s| {
                parameters(s, name).is_some_and(|(_, params, after)| {
                    after.contains("->")
                        && params
                            .iter()
                            .filter(|p| !p.name.is_empty() && p.name != "self" && p.name != "cls")
                            .all(|p| p.text.contains(':'))
                })
            }),
            Language::TypeScript => signature.is_some_and(|s| {
                parameters(s, name).is_some_and(|(_, _, after)| after.starts_with(':'))
            }),
            _ => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trailing_default_is_compatible_and_everything_else_is_not() {
        let py = |b, h| compatible(b, h, "f", Language::Python);
        assert_eq!(
            py("def f(a, b)", "def f(a, b, c=1)"),
            Some(vec!["c".into()])
        );
        assert_eq!(
            py("def f(a)", "def f(a, *args, **kw)"),
            Some(vec!["args".into(), "kw".into()])
        );
        assert_eq!(py("def f(a)", "def f(a, *, k=1)"), Some(vec!["k".into()]));
        assert_eq!(py("def f(a)", "def f(a, b)"), None);
        assert_eq!(py("def f(a, b)", "def f(b, a, c=1)"), None);
        assert_eq!(py("def f(a)", "def f(a) -> int"), None);
        let ts = |b, h| compatible(b, h, "f", Language::TypeScript);
        assert_eq!(
            ts(
                "function f ( a : number )",
                "function f ( a : number , b ? : string )"
            ),
            Some(vec!["b".into()])
        );
        assert_eq!(
            ts(
                "function f ( a : Map < string , number > )",
                "function f ( a : Map < string , number > , ...rest : number [] )"
            ),
            Some(vec!["rest".into()])
        );
        assert_eq!(
            compatible("fn f(a: u32)", "fn f(a: u32, b: u32)", "f", Language::Rust),
            None
        );
    }

    #[test]
    fn types_are_declared_only_where_the_signature_says_so() {
        let typed = |sig, lang| declared_types(DefKind::Function, lang, Some(sig), "f");
        assert!(typed("def f(a: int) -> int", Language::Python));
        assert!(!typed("def f(a) -> int", Language::Python));
        assert!(!typed("def f(a: int)", Language::Python));
        assert!(typed("function f ( a ) : number", Language::TypeScript));
        assert!(!typed("function f ( a : number )", Language::TypeScript));
        assert!(typed("fn f(a: u32)", Language::Rust));
        assert!(!typed("fun f(a: Int): Int", Language::Kotlin));
    }

    #[test]
    fn a_comment_only_edit_folds_every_relation_of_a_function_method_or_class() {
        let comment_only = |kind| Basis {
            kind,
            signature: false,
            body: false,
            decorators: false,
            compatible: None,
            typed: false,
        };
        for kind in [
            DefKind::Function,
            DefKind::Method,
            DefKind::Constructor,
            DefKind::Class,
        ] {
            for relation in [
                Relation::Calls,
                Relation::Reads,
                Relation::Writes,
                Relation::Instantiates,
                Relation::PassesAsValue,
                Relation::TypeUses,
                Relation::Overrides,
                Relation::Reexports,
            ] {
                assert_eq!(
                    rule(&comment_only(kind), relation, false, false),
                    Some(Rule::NoCodeChange),
                    "{kind:?} {relation:?}"
                );
            }
        }
        // An annotation a framework runs is the same code; a candidate keeps the
        // rule of its own.
        let function = comment_only(DefKind::Function);
        assert_eq!(
            rule(&function, Relation::TypeUses, false, true),
            Some(Rule::NoCodeChange)
        );
        assert_eq!(
            rule(&function, Relation::Calls, true, false),
            Some(Rule::UntypedReceiver)
        );
        // Code that changed is the table's other rows, never this one.
        for edited in [
            Basis {
                body: true,
                ..function.clone()
            },
            Basis {
                signature: true,
                ..function.clone()
            },
            Basis {
                decorators: true,
                ..function.clone()
            },
        ] {
            assert_ne!(
                rule(&edited, Relation::Calls, false, false),
                Some(Rule::NoCodeChange)
            );
        }
        // Only the kinds the row names.
        assert_ne!(
            rule(
                &comment_only(DefKind::Interface),
                Relation::TypeUses,
                false,
                false
            ),
            Some(Rule::NoCodeChange)
        );
    }
}
