//! SQL: `CREATE [OR REPLACE] FUNCTION|PROCEDURE|VIEW [schema.]name` defines
//! the name, and `[schema.]name(` in other SQL calls it (#459), read off the
//! tokens: comments and string literals are no code, and the dollar-quoted
//! body of a function or a `DO` block is. tree-sitter-sequel was measured
//! and is not used: it has no `CREATE PROCEDURE`, and PL/pgSQL statements
//! (`PERFORM`, `IF`, `FOR … LOOP`) break its parse, so the calls a migration
//! makes from a function body are where its tree has an ERROR.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use rustc_hash::FxHashMap;

use super::{At, DefId, LanguageFacts, Relation, Table, UseSite};
use crate::bindings::{Evidence, Finding, Relation as Use};
use crate::source::Source;
use crate::types::FragmentKind;

pub(super) fn is_sql(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
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
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

fn lex(text: &str, first_line: u32) -> Vec<Token<'_>> {
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
                line += newlines(&text[start..i]);
            }
            b'\'' => {
                // `E'…'` escapes with backslashes; the `E` lexed as a word.
                let escapes = matches!(
                    out.last(),
                    Some(Token { tok: Tok::Word(w), .. }) if w.eq_ignore_ascii_case("e")
                ) && start > 0
                    && b[start - 1].eq_ignore_ascii_case(&b'e');
                if escapes {
                    out.pop();
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
                });
            }
            _ => {
                out.push(Token {
                    tok: Tok::Punct(c),
                    line,
                    end: line,
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

/// `a.b.c` from `at`: its schema, the name as the database folds it and as
/// written, and the index past it.
fn qualified<'a>(
    tokens: &[Token<'a>],
    at: usize,
) -> Option<(Option<String>, String, &'a str, usize)> {
    let mut parts = vec![ident(tokens.get(at)?.tok)?];
    let mut i = at + 1;
    while tokens.get(i).is_some_and(|t| t.tok == Tok::Punct(b'.')) {
        let Some(next) = tokens.get(i + 1).and_then(|t| ident(t.tok)) else {
            break;
        };
        parts.push(next);
        i += 2;
    }
    let spelled = match tokens[i - 1].tok {
        Tok::Word(w) | Tok::Quoted(w) => w,
        _ => "",
    };
    let name = parts.pop()?;
    Some((parts.pop(), name, spelled, i))
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Routine,
    View,
}

#[derive(Debug)]
struct Definition {
    schema: Option<String>,
    name: String,
    /// The name as written, which a reader asks for.
    spelled: String,
    kind: Kind,
    lines: (u32, u32),
}

/// A name spelled in code, outside a definition's own head.
#[derive(Debug)]
struct Named {
    schema: Option<String>,
    name: String,
    line: u32,
    called: bool,
}

#[derive(Default, Debug)]
struct SqlFacts {
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

fn read(text: &str) -> SqlFacts {
    let mut facts = SqlFacts::default();
    let tokens = lex(text, 1);
    let mut run: Option<(u32, u32)> = None;
    for statement in tokens.split(|t| t.tok == Tok::Punct(b';')) {
        let (Some(first), Some(last)) = (statement.first(), statement.last()) else {
            continue;
        };
        let defined = facts.definitions.len();
        if is_word(first.tok, "create") {
            let object = statement
                .iter()
                .take(8)
                .position(|t| OBJECTS.iter().any(|o| is_word(t.tok, o)));
            let kind = object.and_then(|at| match statement[at].tok {
                Tok::Word(w) if w.eq_ignore_ascii_case("view") => Some(Kind::View),
                Tok::Word(w)
                    if w.eq_ignore_ascii_case("function")
                        || w.eq_ignore_ascii_case("procedure") =>
                {
                    Some(Kind::Routine)
                }
                _ => None,
            });
            if let (Some(at), Some(kind)) = (object, kind) {
                let mut at = at + 1;
                if statement.get(at).is_some_and(|t| is_word(t.tok, "if")) {
                    at += 3;
                }
                if let Some((schema, name, spelled, _)) = qualified(statement, at) {
                    facts.definitions.push(Definition {
                        schema,
                        name,
                        spelled: spelled.to_string(),
                        kind,
                        lines: (first.line, last.end),
                    });
                }
            }
        }
        if facts.definitions.len() > defined {
            facts.between.extend(run.take());
        } else {
            run = Some((run.map_or(first.line, |r| r.0), last.end));
        }
        names_in(statement, &mut facts.names);
    }
    facts.between.extend(run);
    facts
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

/// Every name the code of `tokens` spells, a routine's or `DO` block's body
/// included.
fn names_in(tokens: &[Token], out: &mut Vec<Named>) {
    let mut i = 0;
    while i < tokens.len() {
        match tokens[i].tok {
            Tok::Dollar(body) => {
                let prev = i.checked_sub(1).map(|p| tokens[p].tok);
                if prev.is_some_and(|p| is_word(p, "as") || is_word(p, "do")) {
                    let inner = lex(body, tokens[i].line);
                    for statement in inner.split(|t| t.tok == Tok::Punct(b';')) {
                        names_in(statement, out);
                    }
                }
                i += 1;
            }
            Tok::Word(_) | Tok::Quoted(_) => {
                let Some((schema, name, _, next)) = qualified(tokens, i) else {
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
                if !head {
                    out.push(Named {
                        schema,
                        name,
                        line: tokens[i].line,
                        called: tokens.get(next).is_some_and(|t| t.tok == Tok::Punct(b'(')),
                    });
                }
                i = next;
            }
            _ => i += 1,
        }
    }
}

/// The definitions of `text` as `(name, lines, kind)`, for the fragments
/// an impact run lays over the text chunks of a SQL file, and the code
/// between them, which a chunk spanning a definition would otherwise hide:
/// a call there is a caller of its own.
pub(super) fn declarations(text: &str) -> Vec<(Option<String>, (u32, u32), FragmentKind)> {
    let facts = read(text);
    let between = facts
        .between
        .into_iter()
        .map(|lines| (None, lines, FragmentKind::Chunk));
    facts
        .definitions
        .into_iter()
        .map(|d| {
            let kind = match d.kind {
                Kind::Routine => FragmentKind::Function,
                Kind::View => FragmentKind::Definition,
            };
            (Some(d.spelled), d.lines, kind)
        })
        .chain(between)
        .collect()
}

/// The schema a definition starting on `line` names, `None` when it names
/// none.
pub(super) fn schema_at(text: &str, line: u32) -> Option<String> {
    read(text)
        .definitions
        .into_iter()
        .find(|d| d.lines.0 == line)
        .and_then(|d| d.schema)
}

/// A redefinition (`CREATE OR REPLACE` in a later migration) is the same
/// object: one handle per schema, name and kind.
#[derive(Clone, PartialEq, Eq, Hash)]
struct SqlDef {
    schema: Option<String>,
    name: String,
    kind: Kind,
}

pub(super) struct SqlModel<'a> {
    root: &'a Path,
    source: &'a Source,
    facts: RefCell<FxHashMap<String, Option<Rc<SqlFacts>>>>,
    defs: Table<SqlDef, SqlDef>,
}

impl<'a> SqlModel<'a> {
    pub(super) fn new(root: &'a Path, source: &'a Source) -> Self {
        Self {
            root,
            source,
            facts: RefCell::default(),
            defs: Table::new(),
        }
    }

    fn facts(&self, rel: &str) -> Option<Rc<SqlFacts>> {
        if let Some(known) = self.facts.borrow().get(rel) {
            return known.clone();
        }
        let read = self
            .source
            .read_to_string(&self.root.join(rel))
            .map(|text| Rc::new(read(&text)));
        self.facts
            .borrow_mut()
            .insert(rel.to_string(), read.clone());
        read
    }
}

/// How a spelled name reaches a definition. An unqualified name resolves
/// through the search path, which only the session knows; `public` is the
/// schema an unqualified definition lands in by default.
fn evidence(named: &Named, def: &SqlDef) -> Option<Evidence> {
    let public = |s: &Option<String>| s.as_deref().is_none_or(|s| s == "public");
    match (&named.schema, &def.schema) {
        (a, b) if a == b => Some(Evidence::Resolved),
        (a, b) if public(a) && public(b) => Some(Evidence::Resolved),
        (None, _) | (_, None) => Some(Evidence::Candidate("search_path")),
        _ => None,
    }
}

const MODELLED: &[Relation] = &[Relation::Callers, Relation::Uses];

impl LanguageFacts for SqlModel<'_> {
    fn modelled(&self) -> &'static [Relation] {
        MODELLED
    }

    fn covers(&self, path: &str) -> bool {
        is_sql(path)
    }

    fn definition(&self, at: &At) -> Option<DefId> {
        let facts = self.facts(at.path)?;
        let d = facts.definitions.iter().find(|d| {
            d.lines.0 == at.frag.start_line()
                && Some(d.spelled.as_str()) == at.frag.symbol_name.as_deref()
        })?;
        let def = SqlDef {
            schema: d.schema.clone(),
            name: d.name.clone(),
            kind: d.kind,
        };
        self.defs.intern(def.clone(), || Some(def))
    }

    fn classify(&self, site: &UseSite, def: DefId) -> Option<Finding> {
        let def = self.defs.get(def);
        let facts = self.facts(site.path)?;
        // A definition's own lines are no use of it, a redefinition's
        // neither.
        let defining = |line: u32| {
            facts.definitions.iter().any(|d| {
                d.name == def.name && d.kind == def.kind && d.lines.0 <= line && line <= d.lines.1
            })
        };
        let mut best: Option<Evidence> = None;
        let mut lines = Vec::new();
        for named in facts.names.iter().filter(|n| {
            n.name == def.name
                && n.called == (def.kind == Kind::Routine)
                && (site.lines.0..=site.lines.1).contains(&n.line)
                && !defining(n.line)
        }) {
            let Some(found) = evidence(named, &def) else {
                continue;
            };
            best = Some(best.map_or(found, |b: Evidence| b.max(found)));
            lines.push(named.line);
        }
        lines.dedup();
        Some(Finding {
            evidence: best?,
            relation: match def.kind {
                Kind::Routine => Use::Call,
                Kind::View => Use::Reference,
            },
            lines,
        })
    }

    /// A SQL test reaches what it calls; no file includes another.
    fn reaches_file(&self, _test: &str, _caller: &str) -> bool {
        false
    }

    fn use_lines(&self, path: &str, def: DefId) -> Option<Vec<u32>> {
        let def = self.defs.get(def);
        let facts = self.facts(path)?;
        let mut lines: Vec<u32> = facts
            .names
            .iter()
            .filter(|n| n.name == def.name)
            .map(|n| n.line)
            .collect();
        lines.dedup();
        Some(lines)
    }
}
