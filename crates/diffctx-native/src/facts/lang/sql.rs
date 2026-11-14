//! What a SQL file states: the routines and views it defines, and the calls of
//! routines and reads of views it makes (#459), read off the tokens. Comments
//! and string literals are no code; the dollar-quoted body of a function and a
//! `DO` block is. tree-sitter-sequel was measured and is not used: it has no
//! `CREATE PROCEDURE`, and PL/pgSQL statements (`PERFORM`, `IF`, `FOR … LOOP`)
//! break its parse, so the calls a migration makes from a function body are
//! where its tree has an ERROR.
//!
//! Conventions a reader of the facts cannot recover from the types:
//!
//! - `CREATE [OR REPLACE] FUNCTION|PROCEDURE [schema.]name` is a `Function` and
//!   `CREATE [OR REPLACE] VIEW [schema.]name` an `Object`; the schema, when
//!   written, is the one container name. A name folds to lower case unless
//!   quoted, as the database folds it. The span is the whole statement, a
//!   routine's body included; the signature is its header up to `AS`, without
//!   `OR REPLACE`.
//! - `[schema.]name(` outside a definition's own head is a `Call` of that name,
//!   and a relation named after `FROM`, `JOIN`, `UPDATE` or a comma of a `FROM`
//!   list a `Read`; the schema is the one qualifier. What a `CREATE`, `DROP`,
//!   `ALTER`, `COMMENT ON`, `INSERT INTO` or `REFERENCES` names is declared, not
//!   used. A name inside a definition is owned by it, one in a statement of its
//!   own by the module; a definition's own name inside its body is no use of it.
//! - A call in a host language's string is not read.

use std::ops::Range;

use crate::facts::{
    BlobFacts, Def, DefKind, Extractor, Fact, Language, LocalKey, Name, Namespace, Receiver, Ref,
    Scope, Span, UseKind, Visibility, expired, hash_text, hash_tokens,
};
use crate::snapshot::RunBudget;

pub struct SqlExtractor;

impl Extractor for SqlExtractor {
    fn language(&self) -> Language {
        Language::Sql
    }

    fn extract(&self, text: &[u8], budget: &RunBudget) -> BlobFacts {
        let mut blob = BlobFacts::new(Language::Sql);
        if expired(budget) {
            blob.deadline();
            return blob;
        }
        let Ok(source) = std::str::from_utf8(text) else {
            blob.unsupported();
            return blob;
        };
        blob.facts = facts_of(source);
        blob.in_source_order();
        blob
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tok<'a> {
    /// An identifier or keyword, as spelled.
    Word(&'a str),
    /// A `"quoted identifier"`, its case kept.
    Quoted(&'a str),
    Punct(u8),
    /// A string or number literal.
    Literal,
    /// A dollar-quoted string's text.
    Dollar(&'a str),
}

#[derive(Clone, Copy, Debug)]
struct Token<'a> {
    tok: Tok<'a>,
    line: u32,
    /// The line it ends on: a string or a body spans several.
    end: u32,
    /// Where it stands in the file.
    at: usize,
    to: usize,
    /// Where a dollar-quoted string's text starts.
    inner: usize,
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// The tokens of `text`, which starts at byte `base` and line `first_line` of
/// the file.
fn lex(text: &str, base: usize, first_line: u32) -> Vec<Token<'_>> {
    let b = text.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let (mut i, mut line) = (0usize, first_line);
    let newlines = |s: &str| s.matches('\n').count() as u32;
    while i < b.len() {
        let c = b[i];
        let start = i;
        match c {
            b'\n' => {
                line += 1;
                i += 1;
            }
            _ if c.is_ascii_whitespace() => i += 1,
            b'-' if b.get(i + 1) == Some(&b'-') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if b[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                line += newlines(&text[start..i.min(b.len())]);
            }
            b'\'' => {
                // `E'…'` escapes with backslashes; the `E` lexed as a word.
                let escapes = matches!(
                    out.last(),
                    Some(Token { tok: Tok::Word(w), .. }) if w.eq_ignore_ascii_case("e")
                ) && start > 0
                    && b[start - 1].eq_ignore_ascii_case(&b'e');
                let mut from = start;
                if escapes {
                    if let Some(word) = out.pop() {
                        from = word.at - base;
                    }
                }
                i += 1;
                while i < b.len() {
                    match b[i] {
                        b'\\' if escapes => i += 2,
                        b'\'' if b.get(i + 1) == Some(&b'\'') => i += 2,
                        b'\'' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
                i = i.min(b.len());
                let end = line + newlines(&text[start..i]);
                out.push(Token {
                    tok: Tok::Literal,
                    line,
                    end,
                    at: base + from,
                    to: base + i,
                    inner: 0,
                });
                line = end;
            }
            b'"' => {
                i += 1;
                while i < b.len() && !(b[i] == b'"' && b.get(i + 1) != Some(&b'"')) {
                    i += if b[i] == b'"' { 2 } else { 1 };
                }
                let close = i.min(b.len());
                i = (i + 1).min(b.len());
                let end = line + newlines(&text[start..i]);
                out.push(Token {
                    tok: Tok::Quoted(&text[start + 1..close]),
                    line,
                    end,
                    at: base + start,
                    to: base + i,
                    inner: 0,
                });
                line = end;
            }
            b'$' if !b.get(i + 1).is_some_and(u8::is_ascii_digit) => {
                let tag_end = b[i + 1..]
                    .iter()
                    .position(|&c| !(c.is_ascii_alphanumeric() || c == b'_'))
                    .map(|n| i + 1 + n);
                match tag_end.filter(|&e| b[e] == b'$') {
                    Some(e) => {
                        let tag = &text[i..=e];
                        let body = e + 1;
                        let close = text[body..].find(tag).map_or(b.len(), |n| body + n);
                        i = (close + tag.len()).min(b.len());
                        let end = line + newlines(&text[start..i]);
                        out.push(Token {
                            tok: Tok::Dollar(&text[body..close]),
                            line,
                            end,
                            at: base + start,
                            to: base + i,
                            inner: base + body,
                        });
                        line = end;
                    }
                    None => i += 1,
                }
            }
            _ if c.is_ascii_digit() => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.') {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Literal,
                    line,
                    end: line,
                    at: base + start,
                    to: base + i,
                    inner: 0,
                });
            }
            _ if is_ident(c) => {
                while i < b.len() && is_ident(b[i]) {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Word(&text[start..i]),
                    line,
                    end: line,
                    at: base + start,
                    to: base + i,
                    inner: 0,
                });
            }
            _ => {
                out.push(Token {
                    tok: Tok::Punct(c),
                    line,
                    end: line,
                    at: base + start,
                    to: base + start + 1,
                    inner: 0,
                });
                i += 1;
            }
        }
    }
    out
}

/// A name as the database resolves it: an unquoted one folds to lower case.
fn ident(tok: Tok) -> Option<String> {
    match tok {
        Tok::Word(w) => Some(w.to_lowercase()),
        Tok::Quoted(q) => Some(q.replace("\"\"", "\"")),
        _ => None,
    }
}

fn is_word(tok: Tok, word: &str) -> bool {
    matches!(tok, Tok::Word(w) if w.eq_ignore_ascii_case(word))
}

/// `a.b.c` from `at`: its schema, the name as the database folds it, and the
/// index past it.
fn qualified(tokens: &[Token<'_>], at: usize) -> Option<(Option<String>, String, usize)> {
    let mut parts = vec![ident(tokens.get(at)?.tok)?];
    let mut i = at + 1;
    while tokens.get(i).is_some_and(|t| t.tok == Tok::Punct(b'.')) {
        let Some(next) = tokens.get(i + 1).and_then(|t| ident(t.tok)) else {
            break;
        };
        parts.push(next);
        i += 2;
    }
    let name = parts.pop()?;
    Some((parts.pop(), name, i))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Routine,
    View,
}

struct Definition {
    schema: Option<String>,
    name: String,
    /// The name as written, which a fragment of the file is named by.
    spelled: String,
    kind: Kind,
    lines: (u32, u32),
    bytes: Range<usize>,
    name_at: Range<usize>,
    name_lines: (u32, u32),
    /// The header, `OR REPLACE` left out, and the tokens of the rest.
    signature: String,
    body: Vec<Range<usize>>,
}

/// A name spelled in code, outside a definition's own head.
struct Named {
    schema: Option<String>,
    name: String,
    at: Range<usize>,
    lines: (u32, u32),
    called: bool,
    /// The definition the statement that spells it defines, by index.
    owner: Option<usize>,
}

#[derive(Default)]
struct Read {
    definitions: Vec<Definition>,
    names: Vec<Named>,
    /// Each run of statements between definitions, by its lines.
    between: Vec<(u32, u32)>,
}

/// The objects whose keyword `CREATE` names before the name it creates.
const OBJECTS: &[&str] = &[
    "function",
    "procedure",
    "view",
    "table",
    "index",
    "trigger",
    "type",
    "schema",
    "sequence",
    "extension",
    "domain",
    "role",
    "policy",
    "rule",
    "aggregate",
    "operator",
    "cast",
    "database",
    "user",
    "server",
    "publication",
    "subscription",
];

fn read(text: &str) -> Read {
    let mut facts = Read::default();
    let tokens = lex(text, 0, 1);
    let mut run: Option<(u32, u32)> = None;
    for statement in tokens.split(|t| t.tok == Tok::Punct(b';')) {
        let (Some(first), Some(last)) = (statement.first(), statement.last()) else {
            continue;
        };
        let defined = facts.definitions.len();
        if is_word(first.tok, "create") {
            if let Some(definition) = definition_of(text, statement) {
                facts.definitions.push(definition);
            }
        }
        let owner = (facts.definitions.len() > defined).then_some(defined);
        if owner.is_some() {
            facts.between.extend(run.take());
        } else {
            run = Some((run.map_or(first.line, |r| r.0), last.end));
        }
        let mut found = Vec::new();
        names_in(statement, &mut found);
        facts
            .names
            .extend(found.into_iter().map(|named| Named { owner, ..named }));
    }
    facts.between.extend(run);
    facts
}

fn definition_of(text: &str, statement: &[Token<'_>]) -> Option<Definition> {
    let (first, last) = (statement.first()?, statement.last()?);
    let object = statement
        .iter()
        .take(8)
        .position(|t| OBJECTS.iter().any(|o| is_word(t.tok, o)))?;
    let kind = match statement[object].tok {
        Tok::Word(w) if w.eq_ignore_ascii_case("view") => Kind::View,
        Tok::Word(w)
            if w.eq_ignore_ascii_case("function") || w.eq_ignore_ascii_case("procedure") =>
        {
            Kind::Routine
        }
        _ => return None,
    };
    let mut at = object + 1;
    if statement.get(at).is_some_and(|t| is_word(t.tok, "if")) {
        at += 3;
    }
    let (schema, name, next) = qualified(statement, at)?;
    let name_token = statement.get(next - 1)?;
    let spelled = match name_token.tok {
        Tok::Word(w) | Tok::Quoted(w) => w.to_string(),
        _ => return None,
    };
    let head_end = statement
        .iter()
        .position(|t| matches!(t.tok, Tok::Dollar(_)) || is_word(t.tok, "as"))
        .unwrap_or(statement.len());
    let header: Vec<&str> = statement[..head_end]
        .iter()
        .enumerate()
        .filter(|(i, t)| !(*i > 0 && *i < 3 && (is_word(t.tok, "or") || is_word(t.tok, "replace"))))
        .map(|(_, t)| &text[t.at..t.to])
        .collect();
    Some(Definition {
        schema,
        name,
        spelled,
        kind,
        lines: (first.line, last.end),
        bytes: first.at..last.to,
        name_at: name_token.at..name_token.to,
        name_lines: (name_token.line, name_token.end),
        signature: header.join(" "),
        body: statement[head_end..].iter().map(|t| t.at..t.to).collect(),
    })
}

/// Words after which a name is declared or a relation's, never called.
const NAMING: &[&str] = &[
    "function",
    "procedure",
    "view",
    "table",
    "into",
    "references",
    "exists",
    "type",
    "copy",
];

/// The words after which a relation is named.
const RELATIONS: &[&str] = &[
    "from", "join", "update", "into", "table", "only", "lateral", "using",
];

/// The words that end what `FROM` lists.
const CLAUSES: &[&str] = &[
    "where",
    "group",
    "order",
    "having",
    "select",
    "set",
    "values",
    "on",
    "limit",
    "union",
    "intersect",
    "except",
    "returning",
    "window",
    "offset",
    "fetch",
    "for",
    "with",
    "when",
    "then",
    "else",
];

/// Every name the code of `tokens` uses, a routine's or `DO` block's body
/// included: a call of `name(`, a relation after `FROM`.
fn names_in(tokens: &[Token<'_>], out: &mut Vec<Named>) {
    let (mut list, mut expect) = (false, false);
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].tok {
            Tok::Dollar(body) => {
                let prev = i.checked_sub(1).map(|p| tokens[p].tok);
                if prev.is_some_and(|p| is_word(p, "as") || is_word(p, "do")) {
                    let base = tokens[i].inner;
                    let inner = lex(body, base, tokens[i].line);
                    for statement in inner.split(|t| t.tok == Tok::Punct(b';')) {
                        names_in(statement, out);
                    }
                }
                i += 1;
            }
            Tok::Punct(b',') => {
                expect = list;
                i += 1;
            }
            Tok::Word(w) if RELATIONS.iter().any(|k| w.eq_ignore_ascii_case(k)) => {
                list = w.eq_ignore_ascii_case("from") || w.eq_ignore_ascii_case("using");
                expect = true;
                i += 1;
            }
            Tok::Word(w) if CLAUSES.iter().any(|k| w.eq_ignore_ascii_case(k)) => {
                (list, expect) = (false, false);
                i += 1;
            }
            Tok::Word(_) | Tok::Quoted(_) => {
                let Some((schema, name, next)) = qualified(tokens, i) else {
                    i += 1;
                    continue;
                };
                // `FUNCTION f(…)` after CREATE, DROP, ALTER or COMMENT ON
                // names the routine, `TABLE t (…)` and `INTO t (…)` a
                // relation; `EXECUTE FUNCTION f()` calls it.
                let head = i.checked_sub(1).is_some_and(|p| {
                    NAMING.iter().any(|k| is_word(tokens[p].tok, k))
                        && !p
                            .checked_sub(1)
                            .is_some_and(|q| is_word(tokens[q].tok, "execute"))
                });
                let called = tokens.get(next).is_some_and(|t| t.tok == Tok::Punct(b'('));
                if !head && (called || expect) {
                    let last = &tokens[next - 1];
                    out.push(Named {
                        schema,
                        name,
                        at: last.at..last.to,
                        lines: (last.line, last.end),
                        called,
                        owner: None,
                    });
                }
                expect = false;
                i = next;
            }
            _ => i += 1,
        }
    }
}

/// A definition of a file, for the fragments an impact run lays over the text
/// chunks of a SQL file.
pub struct Defined {
    pub schema: Option<String>,
    pub spelled: String,
    pub routine: bool,
    pub lines: (u32, u32),
}

/// The definitions of `text`, and the lines of the runs of code between them,
/// which a chunk spanning a definition would otherwise hide: a call there is a
/// caller of its own.
pub fn defined(text: &str) -> (Vec<Defined>, Vec<(u32, u32)>) {
    let read = read(text);
    let definitions = read
        .definitions
        .into_iter()
        .map(|d| Defined {
            schema: d.schema,
            spelled: d.spelled,
            routine: d.kind == Kind::Routine,
            lines: d.lines,
        })
        .collect();
    (definitions, read.between)
}

fn facts_of(text: &str) -> Vec<Fact> {
    let read = read(text);
    let keys: Vec<LocalKey> = read
        .definitions
        .iter()
        .map(|d| {
            let container: Vec<Name> = d.schema.iter().map(|s| Name::new(s.as_str())).collect();
            let ns = match d.kind {
                Kind::Routine => Namespace::Value,
                Kind::View => Namespace::Type,
            };
            LocalKey::new(container, Name::new(d.name.as_str()), ns)
        })
        .collect();
    let mut facts: Vec<Fact> = Vec::new();
    for (d, key) in read.definitions.iter().zip(&keys) {
        let bytes = |range: &Range<usize>| &text.as_bytes()[range.clone()];
        facts.push(Fact::Definition(Def {
            local: key.clone(),
            kind: match d.kind {
                Kind::Routine => DefKind::Function,
                Kind::View => DefKind::Object,
            },
            span: span(d.bytes.clone(), d.lines),
            name_span: span(d.name_at.clone(), d.name_lines),
            signature: Some(d.signature.clone()),
            decorators: Vec::new(),
            visibility: Visibility::Public,
            body_hash: hash_tokens(d.body.iter().map(bytes)).unwrap_or(0),
            text_hash: hash_text(&text.as_bytes()[d.bytes.clone()]),
            doc_hash: None,
        }));
    }
    for named in &read.names {
        let owner = named.owner.map(|at| &read.definitions[at]);
        // A definition's own name in its body is no use of it.
        if owner.is_some_and(|d| d.name == named.name && named.called == (d.kind == Kind::Routine))
        {
            continue;
        }
        facts.push(Fact::Reference(Ref {
            site: span(named.at.clone(), named.lines),
            enclosing: named
                .owner
                .map_or(Scope::Module, |at| Scope::Def(keys[at].clone())),
            receiver: Receiver::None,
            use_kind: if named.called {
                UseKind::Call
            } else {
                UseKind::Read
            },
            name: Name::new(named.name.as_str()),
            qualifier: named.schema.iter().map(|s| Name::new(s.as_str())).collect(),
        }));
    }
    facts
}

fn span(bytes: Range<usize>, lines: (u32, u32)) -> Span {
    Span::new(bytes.start as u32..bytes.end as u32, lines)
}
