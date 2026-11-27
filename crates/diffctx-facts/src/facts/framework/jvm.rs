//! What a Java or Kotlin definition holds of its annotations: the extractors
//! keep their names alone, so the arguments are read from the header they are
//! written in, and a path that names a constant is resolved to its literal
//! (`@GetMapping(Routes.ITEMS)`).
//!
//! A constant of another file cannot be read here, so the route's path then
//! carries a reference, `{@demo.Routes#ITEMS}`, which a reader that holds the
//! repository settles with [`resolve_constants`].

use once_cell::sync::Lazy;
use regex::Regex;

use crate::facts::model::{BlobFacts, Def, Fact};

use super::{Call, keyword_of, quoted, top_level};

/// An imported name: `import demo.Routes` is local `Routes` of module `demo`; a
/// static import or a Kotlin member import is the constant's own name.
struct Imported {
    local: String,
    module: String,
    name: String,
}

pub(super) struct Jvm<'a> {
    blob: &'a BlobFacts,
    source: &'a [u8],
    imported: Vec<Imported>,
    /// The file's `package`, where a class it names unimported is declared.
    package: Option<String>,
}

/// The mapping a class puts before its handlers' paths.
enum Prefix {
    Absent,
    Known(String),
    Unknown,
}

impl<'a> Jvm<'a> {
    pub(super) fn new(blob: &'a BlobFacts, source: &'a [u8]) -> Self {
        let imported = blob
            .facts
            .iter()
            .filter_map(|fact| match fact {
                Fact::Import(import) => Some(import),
                _ => None,
            })
            .flat_map(|import| {
                import.names.iter().map(|(name, alias)| Imported {
                    local: alias.as_ref().unwrap_or(name).as_str().to_string(),
                    module: import.module.as_str().to_string(),
                    name: name.as_str().to_string(),
                })
            })
            .collect();
        let package = String::from_utf8_lossy(source).lines().find_map(|line| {
            let name = line.trim_start().strip_prefix("package ")?;
            Some(name.trim().trim_end_matches(';').trim().to_string())
        });
        Self {
            blob,
            source,
            imported,
            package,
        }
    }

    /// A decorator of `def` with the arguments it is written with:
    /// `GetMapping` is `GetMapping("/x")`. A decorator written without any, or
    /// not found in the header, is returned as it is.
    pub(super) fn written(&self, def: &Def, decorator: &str) -> String {
        let Some(bytes) = self
            .source
            .get(def.span.bytes.start as usize..def.name_span.bytes.start as usize)
        else {
            return decorator.to_string();
        };
        let header = String::from_utf8_lossy(bytes);
        let mut rest = header.as_ref();
        while let Some(at) = rest.find('@') {
            let annotation = &rest[at + 1..];
            let name_end = annotation
                .find(|c: char| !identifier(c) && c != '.')
                .unwrap_or(annotation.len());
            let after = &annotation[name_end..];
            let arguments = after.trim_start().strip_prefix('(').and_then(|open| {
                let end = balanced(open)?;
                let skipped = after.len() - after.trim_start().len() + 1;
                Some((&open[..end], skipped + end + 1))
            });
            if annotation[..name_end].rsplit('.').next() == Some(decorator) {
                return match arguments {
                    Some((arguments, _)) => format!("{decorator}({arguments})"),
                    None => decorator.to_string(),
                };
            }
            rest = &annotation[name_end + arguments.map_or(0, |(_, consumed)| consumed)..];
        }
        decorator.to_string()
    }

    /// The paths of a handler: those `own` states (none stated is the class's
    /// path alone) under the path of the nearest enclosing class that carries
    /// `class_decorator`. A path that is no literal and no constant is none.
    pub(super) fn routes(
        &self,
        def: &Def,
        own: Option<&Call<'_>>,
        class_decorator: &str,
    ) -> Vec<Option<String>> {
        let base = match self.class_prefix(def, class_decorator) {
            Prefix::Unknown => return vec![None],
            Prefix::Absent => String::new(),
            Prefix::Known(prefix) => prefix,
        };
        let Some(items) = own.and_then(|call| self.items(call)) else {
            return vec![Some(base).filter(|path| !path.is_empty())];
        };
        let paths: Vec<Option<String>> = items
            .into_iter()
            .flatten()
            .map(|path| Some(join_path(&base, &path)))
            .collect();
        if paths.is_empty() { vec![None] } else { paths }
    }

    fn class_prefix(&self, def: &Def, decorator: &str) -> Prefix {
        let container = &def.local.container;
        for depth in (1..=container.len()).rev() {
            let Some(owner) = self.blob.defs().find(|d| {
                d.local.name == container[depth - 1]
                    && d.local.container[..] == container[..depth - 1]
            }) else {
                continue;
            };
            if !owner.decorators.iter().any(|d| d.as_str() == decorator) {
                continue;
            }
            let written = self.written(owner, decorator);
            let items = self.items(&Call::read(&written));
            return match items {
                None => Prefix::Absent,
                Some(items) => items
                    .into_iter()
                    .flatten()
                    .next()
                    .map_or(Prefix::Unknown, Prefix::Known),
            };
        }
        Prefix::Absent
    }

    /// What a mapping states as its path, one item per string or constant of
    /// the first positional argument or of `value` or `path`: `None` where it
    /// states none, an item `None` where it is neither a literal nor a constant.
    fn items(&self, call: &Call<'_>) -> Option<Vec<Option<String>>> {
        let stated = call
            .arguments
            .iter()
            .find_map(|argument| match keyword_of(argument) {
                None => Some(*argument),
                Some(("value" | "path", value)) => Some(value),
                Some(_) => None,
            })?;
        let list = stated.trim();
        let list = list
            .strip_prefix(['{', '['])
            .and_then(|inner| inner.strip_suffix(['}', ']']))
            .unwrap_or(list);
        Some(
            top_level(list)
                .into_iter()
                .map(|item| self.item(item))
                .collect(),
        )
    }

    fn item(&self, item: &str) -> Option<String> {
        let item = item.trim();
        if let Some(literal) = literal(item) {
            return (!literal.contains('$')).then_some(literal);
        }
        reference(item).then(|| self.constant(item)).flatten()
    }

    /// The literal of the constant `reference` names, or a pointer at the file
    /// that declares it when the imports say which one.
    fn constant(&self, reference: &str) -> Option<String> {
        let text = String::from_utf8_lossy(self.source);
        let (qualifier, name) = match reference.rsplit_once('.') {
            Some((qualifier, name)) => (Some(qualifier), name),
            None => (None, reference),
        };
        let own_file = match qualifier {
            None => true,
            Some(qualifier) => declares_type(&text, qualifier.rsplit('.').next()?),
        };
        if own_file {
            if let Some(literal) = constant_literal(&text, name) {
                return Some(literal);
            }
        }
        let (owner, name) = match qualifier {
            Some(qualifier) => match self.imported.iter().find(|i| i.local == qualifier) {
                Some(imported) => (join_module(&imported.module, &imported.name), name),
                // Unimported, a class is of this file's package or named in full.
                None if !qualifier.contains('.') => match &self.package {
                    Some(package) => (join_module(package, qualifier), name),
                    None => (qualifier.to_string(), name),
                },
                None if qualifier.starts_with(|c: char| c.is_ascii_lowercase()) => {
                    (qualifier.to_string(), name)
                }
                None => return None,
            },
            None => {
                let imported = self.imported.iter().find(|i| i.local == name)?;
                (imported.module.clone(), imported.name.as_str())
            }
        };
        (!owner.is_empty()).then(|| format!("{{@{owner}#{name}}}"))
    }
}

fn identifier(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

fn join_module(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}.{name}")
    }
}

/// A string literal that is the whole of `item`, not a concatenation.
fn literal(item: &str) -> Option<String> {
    let inner = item.strip_prefix('"')?.strip_suffix('"')?;
    (quoted(item) == [inner]).then(|| inner.to_string())
}

/// A name or a dotted chain of names: the spelling of a constant.
fn reference(item: &str) -> bool {
    !item.is_empty()
        && item.split('.').all(|part| {
            part.chars().next().is_some_and(|c| !c.is_ascii_digit()) && part.chars().all(identifier)
        })
}

/// The type and constant patterns hold no name: one is compared after a
/// match, so no pattern is built from a file's text and none can fail.
static TYPE_KEYWORD: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\b(?:class|interface|object|enum)\s+").expect("a constant pattern"));
static WORD_CHAR: Lazy<Regex> = Lazy::new(|| Regex::new(r"\A\w\z").expect("a constant pattern"));
static CONSTANT_HEAD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?:\bstatic\s+final|\bfinal\s+static)\s+String\s+|(\bconst\s+val\s+)")
        .expect("a constant pattern")
});
/// What follows the name: Java's `= "…";`, Kotlin's with an optional `: String`.
static CONSTANT_VALUE: Lazy<[Regex; 2]> = Lazy::new(|| {
    [
        r#"(?m)\A\s*=\s*"([^"\\\n]*)"[ \t]*(?:;|$)"#,
        r#"(?m)\A\s*(?::\s*String\s*)?=\s*"([^"\\\n]*)"[ \t]*(?:;|$)"#,
    ]
    .map(|pattern| Regex::new(pattern).expect("a constant pattern"))
});

/// `\b` at `at`: a word character on one side of it only.
fn word_boundary(text: &str, at: usize) -> bool {
    let word = |c: Option<char>| c.is_some_and(|c| WORD_CHAR.is_match(c.encode_utf8(&mut [0; 4])));
    word(text[..at].chars().next_back()) != word(text[at..].chars().next())
}

fn declares_type(text: &str, name: &str) -> bool {
    TYPE_KEYWORD.find_iter(text).any(|head| {
        text[head.end()..].starts_with(name) && word_boundary(text, head.end() + name.len())
    })
}

/// The literal a `static final String` (Java) or a `const val` (Kotlin) of
/// this name holds in `text`; a value that is not one string is none.
pub fn constant_literal(text: &str, name: &str) -> Option<String> {
    let literal = CONSTANT_HEAD.captures_iter(text).find_map(|head| {
        let end = head.get(0)?.end();
        let rest = text[end..].strip_prefix(name)?;
        let value = &CONSTANT_VALUE[usize::from(head.get(1).is_some())];
        Some(value.captures(rest)?.get(1)?.as_str())
    })?;
    (!literal.contains('$')).then(|| literal.to_string())
}

/// The reference `{@owner#NAME}` of a path settled: each one is what `lookup`
/// gives for the owner's module path and the name. A path that holds one
/// `lookup` cannot settle is none.
pub fn resolve_constants(
    path: &str,
    lookup: impl Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let mut out = path.to_string();
    while let Some(start) = out.find("{@") {
        let end = start + out[start..].find('}')?;
        let (owner, name) = out[start + 2..end].split_once('#')?;
        let literal = lookup(owner, name)?;
        out.replace_range(start..=end, &literal);
    }
    while out.contains("//") {
        out = out.replace("//", "/");
    }
    Some(out)
}

fn join_path(prefix: &str, path: &str) -> String {
    let path = path.trim_start_matches('/');
    let prefix = prefix.trim_end_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (_, true) => prefix.to_string(),
        (true, false) => format!("/{path}"),
        (false, false) => format!("{prefix}/{path}"),
    }
}

/// The offset of the parenthesis that closes the one just opened in `text`,
/// strings respected.
pub fn balanced(text: &str) -> Option<usize> {
    let mut depth = 1;
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if let Some(q) = quote {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}
