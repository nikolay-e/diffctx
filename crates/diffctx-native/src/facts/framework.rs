//! Framework models: how a framework enters code that no caller names. A rule is
//! a row of [`RULES`]: the decorator or annotation that registers a definition,
//! the module the file must import for the name to be the framework's, and what
//! the entry is (a route and its verb and path, a command, a tool, a scheduled
//! job, a fixture). A new framework is a new row; the reader of a decorator is
//! the same for every language, since an extractor already holds the decorators
//! of a definition as text (`app.get("/x")`) or as names alone (`GetMapping`).
//!
//! Every match is the same fact, an [`EntryPoint`] whose `framework` says who
//! calls the definition and under which key, so a reader of facts (the impact
//! answer, a test-reach edge from a route to the tests that post to it) never
//! learns a framework's name to use it.

pub mod jvm;

use crate::facts::ids::Name;
use crate::facts::language::Language;
use crate::facts::model::{BlobFacts, Def, DefKind, EntryKind, EntryPoint, Fact, Framework, Scope};

const PYTHON: &[Language] = &[Language::Python];
const JVM: &[Language] = &[Language::Java, Language::Kotlin];
const TS: &[Language] = &[Language::TypeScript, Language::JavaScript];
const PHP: &[Language] = &[Language::Php];

/// Where a route's HTTP method comes from.
enum Verb {
    /// A route for any method, or an entry that has none.
    Unstated,
    /// The decorator's own name, upper case, without the suffix: `get`, or
    /// `GetMapping` with the suffix `Mapping`.
    Callee(&'static str),
    /// The strings of a keyword argument (`methods=["POST"]`), one entry each;
    /// the default when it is absent.
    Kwarg(&'static str, Option<&'static str>),
    /// The methods a keyword argument names (`method = RequestMethod.POST`,
    /// `method = {GET, POST}`), one entry each; any method when it is absent.
    Methods(&'static str),
}

/// What the entry is called.
enum Label {
    /// Not stated, or not readable from the source.
    Unstated,
    /// The first positional string argument (a route's path, a job's trigger).
    First,
    /// The first positional string argument, else the keyword `name`, else the
    /// definition's own name (a command, a tool, a fixture).
    Named,
    /// A Spring-style mapping: the strings of the first positional argument, or
    /// of `value` or `path`, each under the path of the enclosing class's
    /// `RequestMapping`; a mapping that states none is the class's path.
    Mapping,
    /// A JAX-RS route: the definition's `@Path` under the enclosing class's.
    Path,
}

struct Rule {
    framework: &'static str,
    languages: &'static [Language],
    /// The file imports a module that is one of these or lies beneath one, so a
    /// method named `get` of some other object is no route.
    modules: &'static [&'static str],
    /// The decorator's last name, as written.
    decorators: &'static [&'static str],
    kind: EntryKind,
    verb: Verb,
    label: Label,
}

const VERBS: &[&str] = &["get", "post", "put", "delete", "patch"];

const fn route(
    framework: &'static str,
    languages: &'static [Language],
    modules: &'static [&'static str],
    decorators: &'static [&'static str],
    verb: Verb,
    label: Label,
) -> Rule {
    Rule {
        framework,
        languages,
        modules,
        decorators,
        kind: EntryKind::Route,
        verb,
        label,
    }
}

const fn named(
    framework: &'static str,
    modules: &'static [&'static str],
    decorators: &'static [&'static str],
    kind: EntryKind,
) -> Rule {
    Rule {
        framework,
        languages: PYTHON,
        modules,
        decorators,
        kind,
        verb: Verb::Unstated,
        label: Label::Named,
    }
}

const fn job(
    framework: &'static str,
    languages: &'static [Language],
    modules: &'static [&'static str],
    decorators: &'static [&'static str],
    label: Label,
) -> Rule {
    Rule {
        framework,
        languages,
        modules,
        decorators,
        kind: EntryKind::Scheduled,
        verb: Verb::Unstated,
        label,
    }
}

static RULES: &[Rule] = &[
    route(
        "flask",
        PYTHON,
        &["flask"],
        &["route"],
        Verb::Kwarg("methods", Some("GET")),
        Label::First,
    ),
    route(
        "flask",
        PYTHON,
        &["flask"],
        VERBS,
        Verb::Callee(""),
        Label::First,
    ),
    route(
        "quart",
        PYTHON,
        &["quart"],
        &["route"],
        Verb::Kwarg("methods", Some("GET")),
        Label::First,
    ),
    route(
        "quart",
        PYTHON,
        &["quart"],
        VERBS,
        Verb::Callee(""),
        Label::First,
    ),
    route(
        "fastapi",
        PYTHON,
        &["fastapi"],
        &["get", "post", "put", "delete", "patch", "head", "options"],
        Verb::Callee(""),
        Label::First,
    ),
    route(
        "fastapi",
        PYTHON,
        &["fastapi"],
        &["api_route"],
        Verb::Kwarg("methods", None),
        Label::First,
    ),
    route(
        "sanic",
        PYTHON,
        &["sanic"],
        &["route"],
        Verb::Kwarg("methods", Some("GET")),
        Label::First,
    ),
    route(
        "sanic",
        PYTHON,
        &["sanic"],
        VERBS,
        Verb::Callee(""),
        Label::First,
    ),
    route(
        "aiohttp",
        PYTHON,
        &["aiohttp"],
        VERBS,
        Verb::Callee(""),
        Label::First,
    ),
    route(
        "litestar",
        PYTHON,
        &["litestar"],
        VERBS,
        Verb::Callee(""),
        Label::First,
    ),
    named(
        "click",
        &["click"],
        &["command", "group"],
        EntryKind::Command,
    ),
    named("typer", &["typer"], &["command"], EntryKind::Command),
    named("mcp", &["mcp", "fastmcp"], &["tool"], EntryKind::Tool),
    named(
        "pytest",
        &["pytest", "pytest_asyncio"],
        &["fixture"],
        EntryKind::Fixture,
    ),
    job(
        "apscheduler",
        PYTHON,
        &["apscheduler"],
        &["scheduled_job"],
        Label::First,
    ),
    route(
        "spring",
        JVM,
        &["org.springframework.web"],
        &[
            "GetMapping",
            "PostMapping",
            "PutMapping",
            "DeleteMapping",
            "PatchMapping",
        ],
        Verb::Callee("Mapping"),
        Label::Mapping,
    ),
    route(
        "spring",
        JVM,
        &["org.springframework.web"],
        &["RequestMapping"],
        Verb::Methods("method"),
        Label::Mapping,
    ),
    job(
        "spring",
        JVM,
        &["org.springframework.scheduling"],
        &["Scheduled"],
        Label::Unstated,
    ),
    job(
        "quarkus",
        JVM,
        &["io.quarkus.scheduler"],
        &["Scheduled"],
        Label::Unstated,
    ),
    route(
        "jax-rs",
        JVM,
        &["jakarta.ws.rs", "javax.ws.rs"],
        &["GET", "POST", "PUT", "DELETE", "PATCH"],
        Verb::Callee(""),
        Label::Path,
    ),
    route(
        "micronaut",
        JVM,
        &["io.micronaut.http.annotation"],
        &["Get", "Post", "Put", "Delete", "Patch"],
        Verb::Callee(""),
        Label::Unstated,
    ),
    route(
        "nestjs",
        TS,
        &["@nestjs/common"],
        &["Get", "Post", "Put", "Delete", "Patch"],
        Verb::Callee(""),
        Label::Unstated,
    ),
    route(
        "symfony",
        PHP,
        &["Symfony\\Component\\Routing"],
        &["Route"],
        Verb::Unstated,
        Label::First,
    ),
    job(
        "nestjs",
        TS,
        &["@nestjs/schedule"],
        &["Cron", "Interval", "Timeout"],
        Label::Unstated,
    ),
];

/// Adds the entry each rule makes of a decorated definition of `blob`. Called by
/// the extractors of the languages that hold decorators, before the source
/// order is fixed; it reads only what the blob already states.
pub fn derive(blob: &mut BlobFacts) {
    derive_in(blob, &[]);
}

/// [`derive`] for a language whose definitions hold annotations as names alone
/// (Java, Kotlin): the arguments an annotation carries are read from `source`,
/// the bytes the blob was extracted from.
pub fn derive_in(blob: &mut BlobFacts, source: &[u8]) {
    let modules: Vec<String> = blob
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Import(import) => Some(import.module.as_str().to_string()),
            _ => None,
        })
        .collect();
    let jvm = jvm::Jvm::new(blob, source);
    let mut entries = Vec::new();
    for def in blob.defs() {
        for decorator in &def.decorators {
            let written = jvm.written(def, decorator.as_str());
            let call = Call::read(&written);
            let rule = RULES.iter().find(|rule| {
                rule.languages.contains(&blob.language)
                    && rule.decorators.contains(&call.name)
                    && rule.modules.iter().any(|m| imports(&modules, m))
            });
            let Some(rule) = rule else {
                continue;
            };
            let paths = match rule.label {
                // A class's own mapping is the prefix of its handlers, no handler.
                Label::Mapping | Label::Path
                    if !matches!(def.kind, DefKind::Function | DefKind::Method) =>
                {
                    continue;
                }
                Label::Mapping => jvm.routes(def, Some(&call), "RequestMapping"),
                Label::Path => {
                    let own = def
                        .decorators
                        .iter()
                        .find(|d| d.as_str() == "Path")
                        .map(|d| jvm.written(def, d.as_str()));
                    jvm.routes(def, own.as_deref().map(Call::read).as_ref(), "Path")
                }
                _ => Vec::new(),
            };
            entries.extend(entries_of(rule, &call, def, paths));
        }
    }
    blob.facts.extend(entries.into_iter().map(Fact::EntryPoint));
}

/// How an answer begins when it states an entry a framework makes.
pub const ENTRY_PREFIX: &str = "entry point: ";

/// The one wording of an entry a framework makes, who it is for the reader:
/// `entry point: route GET /reviews (fastapi)`.
pub fn describe(kind: EntryKind, framework: &Framework) -> String {
    let what = match kind {
        EntryKind::Route => "route",
        EntryKind::Command => "command",
        EntryKind::Tool => "tool",
        EntryKind::Scheduled => "scheduled job",
        EntryKind::Fixture => "fixture",
        EntryKind::Migration => "migration",
        EntryKind::Main | EntryKind::ScriptGuard | EntryKind::Test => "entry",
    };
    let mut words = vec![what.to_string()];
    words.extend(framework.verb.iter().map(ToString::to_string));
    // A path that names a constant of another file is settled by whoever holds
    // the repository; spelled out here it would be the reference.
    words.extend(
        framework
            .label
            .clone()
            .filter(|label| !label.contains("{@")),
    );
    format!("{ENTRY_PREFIX}{} ({})", words.join(" "), framework.name)
}

/// Whether some import is `wanted` or beneath it (`flask.views`, `@nestjs/common/x`).
fn imports(modules: &[String], wanted: &str) -> bool {
    modules.iter().any(|m| {
        m.strip_prefix(wanted)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '/', '\\']))
    })
}

fn entries_of(
    rule: &Rule,
    call: &Call<'_>,
    def: &Def,
    paths: Vec<Option<String>>,
) -> Vec<EntryPoint> {
    let verbs: Vec<Option<String>> = match &rule.verb {
        Verb::Unstated => vec![None],
        Verb::Callee(suffix) => {
            let name = call.name.strip_suffix(*suffix).unwrap_or(call.name);
            vec![Some(name.to_ascii_uppercase())]
        }
        Verb::Kwarg(key, default) => {
            let stated: Vec<Option<String>> = call
                .keyword(key)
                .map_or_else(Vec::new, quoted)
                .into_iter()
                .map(|verb| Some(verb.to_ascii_uppercase()))
                .collect();
            if stated.is_empty() {
                vec![default.map(str::to_string)]
            } else {
                stated
            }
        }
        Verb::Methods(key) => {
            let stated: Vec<Option<String>> = call
                .keyword(key)
                .map_or_else(Vec::new, http_methods)
                .into_iter()
                .map(Some)
                .collect();
            if stated.is_empty() {
                vec![None]
            } else {
                stated
            }
        }
    };
    let labels: Vec<Option<String>> = match rule.label {
        Label::Unstated => vec![None],
        Label::First => vec![call.positional_string()],
        Label::Named => vec![
            call.positional_string()
                .or_else(|| {
                    call.keyword("name")
                        .and_then(|v| quoted(v).into_iter().next())
                })
                .or_else(|| Some(def.local.name.as_str().to_string())),
        ],
        Label::Mapping | Label::Path => paths,
    };
    verbs
        .into_iter()
        .flat_map(|verb| {
            labels
                .iter()
                .map(move |label| (verb.clone(), label.clone()))
        })
        .map(|(verb, label)| EntryPoint {
            site: def.name_span.clone(),
            kind: rule.kind,
            target: Scope::Def(def.local.clone()),
            framework: Some(Framework {
                name: Name::new(rule.framework),
                verb: verb.map(Name::new),
                label,
            }),
        })
        .collect()
}

/// The HTTP methods an argument names, in order, each once: `RequestMethod.GET`
/// and `{GET, POST}` are `GET` and `GET, POST`.
fn http_methods(text: &str) -> Vec<String> {
    const METHODS: &[&str] = &[
        "GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "TRACE",
    ];
    let mut found: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        if METHODS.contains(&word) && !found.iter().any(|m| m == word) {
            found.push(word.to_string());
        }
    }
    found
}

/// A decorator as written: `pytest.fixture(scope="module")`, `GetMapping`.
struct Call<'a> {
    /// The last name of the callee.
    name: &'a str,
    arguments: Vec<&'a str>,
}

impl<'a> Call<'a> {
    fn read(text: &'a str) -> Self {
        let text = text.trim();
        let (callee, rest) = match text.split_once('(') {
            Some((callee, rest)) => (callee, rest.trim_end().trim_end_matches(')')),
            None => (text, ""),
        };
        let callee = callee.trim();
        Self {
            name: callee.rsplit('.').next().unwrap_or(callee).trim(),
            arguments: top_level(rest),
        }
    }

    fn positional_string(&self) -> Option<String> {
        let first = self.arguments.first()?;
        if keyword_of(first).is_some() {
            return None;
        }
        quoted(first).into_iter().next()
    }

    fn keyword(&self, key: &str) -> Option<&'a str> {
        self.arguments.iter().find_map(|argument| {
            let (name, value) = keyword_of(argument)?;
            (name == key).then_some(value)
        })
    }
}

/// `name=value`, with no `==` and a plain identifier on the left.
fn keyword_of(argument: &str) -> Option<(&str, &str)> {
    let (name, value) = argument.split_once('=')?;
    let name = name.trim();
    let plain = !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_');
    (plain && !value.starts_with('=')).then_some((name, value.trim()))
}

/// The arguments split at commas outside brackets and strings.
fn top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
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
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..at].trim());
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
}

/// The plain string literals of `text`, in order (`["POST", "PUT"]` is two). A
/// string with a prefix other than `r` (`f"..."`, `b"..."`) is no plain text.
fn quoted(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    let mut previous = ' ';
    while let Some(c) = chars.next() {
        if matches!(c, '\'' | '"') {
            let mut value = String::new();
            let mut closed = false;
            for d in chars.by_ref() {
                if d == c {
                    closed = true;
                    break;
                }
                value.push(d);
            }
            let prefixed = previous.is_alphabetic() && !matches!(previous, 'r' | 'R');
            if closed && !prefixed {
                out.push(value);
            }
            previous = c;
            continue;
        }
        previous = c;
    }
    out
}
