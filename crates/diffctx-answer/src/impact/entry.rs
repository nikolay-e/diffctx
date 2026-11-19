//! What makes a definition an entry point nobody calls: decorators that shape it, JVM entry points,
//! fixtures, handlers and the parameter lists a change may extend.

use std::path::Path;

use diffctx_base::types::Fragment;

use diffctx_facts::facts::{Import, Star, framework};

use super::callers::calls_word;
use super::changes::declaration_head;
use super::dispatch::{base_symbol, enclosing_class, supertypes};

/// Decorators that shape a definition without handing it to anyone.
const SHAPING_DECORATORS: &[&str] = &[
    "property",
    "staticmethod",
    "classmethod",
    "dataclass",
    "abstractmethod",
    "override",
    "cached_property",
    "wraps",
    "lru_cache",
    "cache",
    "total_ordering",
    "final",
    "overload",
    "contextmanager",
    "asynccontextmanager",
    "Override",
    "Deprecated",
    "SuppressWarnings",
    "FunctionalInterface",
    // `@value.setter`, `@attr.s`, `@define`, `@enum.unique`, Lombok: they
    // shape what they decorate and register it nowhere (#414).
    "setter",
    "getter",
    "deleter",
    "s",
    "attrs",
    "define",
    "frozen",
    "mutable",
    "unique",
    "Data",
    "Value",
    "Getter",
    "Setter",
    "Builder",
    "Slf4j",
    "ToString",
    "EqualsAndHashCode",
    "NoArgsConstructor",
    "AllArgsConstructor",
    "RequiredArgsConstructor",
];

/// The decorator that hands a definition to a registry, a router or a
/// container, on the definition or on the class around it.
/// JVM annotations that hand a method or a bean to a framework that calls
/// it: request mappings, schedulers, listeners, bean factories. A
/// class-level `@Component` registers the class, not every method on it.
const JVM_ENTRY_POINTS: &[&str] = &[
    "RequestMapping",
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "Scheduled",
    "EventListener",
    "TransactionalEventListener",
    "KafkaListener",
    "RabbitListener",
    "JmsListener",
    "SqsListener",
    "ExceptionHandler",
    "MessageMapping",
    "SubscribeMapping",
    "QueryMapping",
    "MutationMapping",
    "SubscriptionMapping",
    "SchemaMapping",
    "BatchMapping",
    "Bean",
    "PostConstruct",
    "PreDestroy",
    "Tool",
    "GET",
    "POST",
    "PUT",
    "DELETE",
    "PATCH",
    "HEAD",
    "OPTIONS",
    "Get",
    "Post",
    "Put",
    "Delete",
    "Patch",
    "Autowired",
    "Inject",
    "Resource",
    "Value",
    "InitBinder",
    "ModelAttribute",
    "Around",
    "Before",
    "After",
    "AfterReturning",
    "AfterThrowing",
    "Provides",
    "Subscribe",
    "PrePersist",
    "PostPersist",
    "PreUpdate",
    "PostUpdate",
    "PreRemove",
    "PostRemove",
    "PostLoad",
    "JsonCreator",
];

/// Packages whose annotations are trusted as entry points when imported.
const JVM_FRAMEWORK_PACKAGES: &[&str] = &[
    "org.springframework.",
    "jakarta.",
    "javax.",
    "io.micronaut.",
    "io.quarkus.",
    "org.eclipse.microprofile.",
    "org.aspectj.",
    "dagger.",
    "com.google.inject.",
    "com.google.common.eventbus.",
    "org.greenrobot.eventbus.",
    "com.fasterxml.jackson.annotation.",
];

/// Packages whose annotations share an entry point's name and enter nothing.
const JVM_NEUTRAL_PACKAGES: &[&str] = &["lombok.", "java.", "kotlin."];

pub(super) fn is_jvm_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    [".kt", ".kts", ".java", ".scala"]
        .iter()
        .any(|e| p.ends_with(e))
}

/// What an annotation of the JVM definition says of who calls it: a framework
/// entry point, its package a framework's, or an annotation of the name an
/// entry point has whose package the file does not settle as a framework's
/// (`@Provides` of a library the table does not know, a star import, a name
/// written with its package, none at all), which may be one.
pub(super) fn jvm_entry_point(
    f: &Fragment,
    imports: &[Import],
    annotations: Option<&[String]>,
) -> Option<String> {
    let written: Vec<(Option<String>, String)> = match annotations {
        Some(names) => names.iter().map(|n| (None, n.clone())).collect(),
        None => f
            .content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .filter_map(|l| {
                let name = l
                    .strip_prefix('@')?
                    .split(|c: char| c == '(' || c.is_whitespace())
                    .next()?;
                Some(match name.rsplit_once('.') {
                    Some((package, name)) => (Some(package.to_string()), name.to_string()),
                    None => (None, name.to_string()),
                })
            })
            .collect(),
    };
    let in_list = |package: &str, list: &[&str]| {
        let dotted = format!("{package}.");
        list.iter().any(|p| dotted.starts_with(p))
    };
    let stars: Vec<&str> = imports
        .iter()
        .filter(|i| i.star == Star::Glob)
        .map(|i| i.module.as_str())
        .collect();
    let origins: Vec<(String, Origin)> = written
        .into_iter()
        .filter_map(|(package, name)| {
            let named = imports.iter().find_map(|i| {
                i.names
                    .iter()
                    .find(|(target, alias)| alias.as_ref().unwrap_or(target).as_str() == name)
                    .map(|(target, _)| (i.module.as_str(), target.as_str()))
            });
            let (name, package) = match (&package, named) {
                (Some(package), _) => (name, Some(package.as_str())),
                (None, Some((module, target))) => (target.to_string(), Some(module)),
                (None, None) => (name, None),
            };
            if !JVM_ENTRY_POINTS.contains(&name.as_str()) {
                return None;
            }
            let origin = match package {
                Some(p) if in_list(p, JVM_FRAMEWORK_PACKAGES) => Origin::Framework,
                Some(p) if in_list(p, JVM_NEUTRAL_PACKAGES) => return None,
                Some(p) => Origin::Package(Some(p.to_string())),
                None if stars.iter().any(|p| in_list(p, JVM_FRAMEWORK_PACKAGES)) => {
                    Origin::Framework
                }
                None => match stars.iter().find(|p| !in_list(p, JVM_NEUTRAL_PACKAGES)) {
                    Some(p) => Origin::Package(Some((*p).to_string())),
                    None if !stars.is_empty() => return None,
                    None => Origin::Package(None),
                },
            };
            Some((name, origin))
        })
        .collect();
    origins
        .iter()
        .find(|(_, origin)| matches!(origin, Origin::Framework))
        .map(|(name, _)| name.clone())
        .or_else(|| {
            origins.into_iter().find_map(|(name, origin)| match origin {
                Origin::Package(from) => Some(format!(
                    "{}may be entered by the framework (annotation `@{name}`{})",
                    framework::ENTRY_PREFIX,
                    from.map(|p| format!(" from {p}")).unwrap_or_default()
                )),
                Origin::Framework => None,
            })
        })
}

/// Where an entry-named annotation comes from, as far as the file says.
enum Origin {
    Framework,
    Package(Option<String>),
}

/// A Python function decorated `@pytest.fixture` / `@fixture`, with or
/// without arguments.
pub(super) fn is_pytest_fixture(f: &Fragment) -> bool {
    f.path().ends_with(".py")
        && f.content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .any(|l| {
                let d = l.trim_start_matches('@');
                d.starts_with("pytest.fixture") || d.starts_with("fixture")
            })
}

/// The line of `f`'s own `def` whose parameter list names `name`: how a
/// test or a fixture asks pytest for a fixture.
pub(super) fn names_parameter(f: &Fragment, name: &str) -> Option<u32> {
    let header = f.content.lines().position(|l| {
        let t = l.trim_start();
        t.starts_with("def ") || t.starts_with("async def ")
    })?;
    let signature: String = f
        .content
        .lines()
        .skip(header)
        .take(6)
        .collect::<Vec<_>>()
        .join(" ");
    let params = signature.split_once('(')?.1.split_once(')')?.0;
    params
        .split(',')
        .map(|p| p.trim().split([':', '=']).next().unwrap_or("").trim())
        .any(|p| p == name)
        .then(|| f.start_line() + header as u32)
}

/// How a definition is entered by something that is no caller: its module's
/// own script guard, or a curried registration call (`tool("name")(fn)`).
pub(super) enum Entered {
    Script,
    Registered(String),
}

/// A statement of `f` that enters the definition `name` of its own file:
/// the module's `if __name__ == "__main__":` guard calling it, or a curried
/// registration call naming it. Both are how it is run, not who calls it;
/// so is the entry of a name-keyed dict (`super::runs::dict_registrations_of`).
pub(super) fn entered_by(f: &Fragment, name: &str) -> Option<Entered> {
    let first = f.content.lines().find(|l| !l.trim().is_empty())?.trim();
    let guard = first.starts_with("if __name__ ==")
        && (first.contains("\"__main__\"") || first.contains("'__main__'"));
    if guard && (f.path().ends_with(".py") || f.path().ends_with(".pyi")) {
        return f
            .content
            .lines()
            .any(|l| calls_word(l, name))
            .then_some(Entered::Script);
    }
    super::runs::curried_registrations(f)
        .find(|(_, registered)| *registered == name)
        .map(|(call, _)| Entered::Registered(call.to_string()))
}

/// The class of a `do_<METHOD>` handler a standard-library HTTP server
/// dispatches by method name: the base it names.
pub(super) fn http_handler_base(core: &Fragment, file: &[&Fragment]) -> Option<&'static str> {
    let name = base_symbol(core.symbol_name.as_deref()?);
    let method = name.strip_prefix("do_")?;
    if method.is_empty() || !method.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    let class = enclosing_class(core, file)?;
    let supers = supertypes(&class.content, "py");
    [
        "BaseHTTPRequestHandler",
        "SimpleHTTPRequestHandler",
        "CGIHTTPRequestHandler",
    ]
    .into_iter()
    .find(|base| supers.iter().any(|s| s == base))
}

/// `main` of a program in a language whose runtime calls it.
pub(super) fn is_program_main(core: &Fragment) -> bool {
    let Some(name) = core.symbol_name.as_deref().map(base_symbol) else {
        return false;
    };
    let ext = Path::new(core.path())
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "go" | "java" | "kt" | "kts" | "scala" | "rs" | "c" | "cc" | "cpp" | "swift" => {
            name == "main"
        }
        "cs" => name == "Main",
        _ => false,
    }
}

pub(super) fn registering_decorator(f: &Fragment, file: &[&Fragment]) -> Option<String> {
    let of = |content: &str| {
        content
            .lines()
            .map(str::trim_start)
            .take_while(|l| l.is_empty() || l.starts_with('@'))
            .filter_map(|l| {
                let name = l
                    .strip_prefix('@')?
                    .split(|c: char| c == '(' || c.is_whitespace())
                    .next()?;
                let last = name.rsplit('.').next().unwrap_or(name);
                (!name.is_empty()
                    && !SHAPING_DECORATORS.contains(&last)
                    && !name.starts_with("pytest."))
                .then(|| name.to_string())
            })
            .next()
    };
    of(&f.content).or_else(|| enclosing_class(f, file).and_then(|c| of(&c.content)))
}

/// A field or parameter line that brings its own default (`x: int = 0`,
/// `x = 0`, TypeScript `x?: T`): adding one breaks no call site.
static DEFAULTED_LINE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r"^\s*(readonly\s+)?[A-Za-z_$][\w$]*\s*(\?\s*:[^=]*|:[^=]*=\s*\S.*|=\s*[^=\s].*)$",
    )
    .expect("defaulted line regex")
});

/// The parameters between a declaration's first `(` and its match, split at
/// top-level commas, whitespace squashed.
fn parameter_list(text: &str) -> Option<(String, Vec<String>, String)> {
    let open = text.find('(')?;
    let mut depth = 0i32;
    let mut close = None;
    for (i, c) in text[open..].char_indices() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                if depth == 0 && c == ')' {
                    close = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut params = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in text[open + 1..close].chars() {
        match c {
            '(' | '[' | '{' | '<' => depth += 1,
            ')' | ']' | '}' | '>' => depth -= 1,
            ',' if depth == 0 => {
                params.push(squash(&cur));
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    params.push(squash(&cur));
    params.retain(|p| !p.is_empty());
    Some((squash(&text[..open]), params, squash(&text[close..])))
}

fn defaulted(param: &str) -> bool {
    (param.contains('=') && !param.contains("=>") && !requires_value(param))
        || param.contains("?:")
        || param.starts_with('*')
        || param.starts_with("...")
}

/// `x: int = Field(...)` and `x = ...` declare a default that is a
/// required marker, not a value.
fn requires_value(line: &str) -> bool {
    let Some((_, value)) = line.split_once('=') else {
        return false;
    };
    let value = value.trim().trim_end_matches(',').trim();
    value == "..." || value.starts_with("Field(...") || value.starts_with("Field( ...")
}

/// A line that adds a field or a parameter with its own default.
fn adds_defaulted(line: &str) -> bool {
    // `__slots__ = ("y",)` or `__hash__ = None` changes how every instance
    // is built or compared; a default does not make it compatible.
    let dunder = line.trim_start().starts_with("__");
    DEFAULTED_LINE.is_match(line) && !dunder && !line.contains("=>") && !requires_value(line)
}

/// A field or parameter declaration of any kind: what may not follow an
/// inserted one, since everything after it would shift position.
fn declares_member(line: &str) -> bool {
    static MEMBER: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^\s*(readonly\s+)?\*{0,2}[A-Za-z_$][\w$]*\s*(\??\s*:|=|,\s*$)")
            .expect("member regex")
    });
    MEMBER.is_match(line)
}

/// Whether every hunk in the definition only added fields or parameters
/// that carry a default (#388): the call sites keep working, so they are
/// counted instead of listed.
pub(super) fn compatible_extension(
    core: &Fragment,
    hunks: &[&diffctx_base::changed_lines::HunkText],
) -> bool {
    let head_end = core.start_line() + declaration_head(&core.content).lines().count() as u32;
    !hunks.is_empty()
        && hunks.iter().all(|h| {
            let (_, end) = h.new_range();
            if h.removed.is_empty() {
                // Appended only: the next declaration line after the run
                // must close the list, or the inserted member shifts every
                // positional argument behind it.
                let after = core
                    .content
                    .lines()
                    .skip((end + 1).saturating_sub(core.start_line()) as usize)
                    .find(|l| !l.trim().is_empty());
                let appended = after.is_none_or(|l| !declares_member(l));
                (core.kind.is_container() || end < head_end)
                    && appended
                    && h.added
                        .iter()
                        .filter(|l| !l.trim().is_empty())
                        .all(|l| adds_defaulted(l))
            } else {
                // A parameter list lives in the declaration head; the same
                // shape in a body line is a call (`compute(a)` →
                // `compute(a, strict=True)`), and it breaks nothing it adds.
                let (start, _) = h.new_range();
                if start < core.start_line() || end >= head_end {
                    return false;
                }
                let (Some((old_pre, old, old_post)), Some((new_pre, new, new_post))) = (
                    parameter_list(&h.removed.join("\n")),
                    parameter_list(&h.added.join("\n")),
                ) else {
                    return false;
                };
                old_pre == new_pre
                    && old_post == new_post
                    && new.len() > old.len()
                    && new[..old.len()] == old[..]
                    && new[old.len()..].iter().all(|p| defaulted(p))
            }
        })
}
