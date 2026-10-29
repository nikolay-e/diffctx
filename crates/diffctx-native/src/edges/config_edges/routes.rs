//! HTTP routes as code spells them: the path a framework actually serves
//! (a router's prefix, a class-level mapping, a mount) and the paths clients
//! call (a base-URL constant, a template string, a test client).
//!
//! A handler's key is composed from what its own file says. A router mounted
//! from another file (`app.include_router(items.router, prefix="/api")`,
//! `app.use('/api', users)`) leaves a [`Role::Mount`] record that
//! `keys.rs` applies to the routes of the file it names once every file is
//! read.

use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use rustc_hash::FxHashMap;

use super::keys_extract::{CodeLang, Kind, Lines, Mount, RawRecord, Role, Strength, pattern};
use super::keys_extract::{route_key, route_shape};

fn record(key: String, role: Role, mechanism: &'static str, line: usize) -> RawRecord {
    RawRecord {
        kind: Kind::Route,
        key,
        strength: Strength::Strong,
        role,
        mechanism,
        line,
        end: line,
        anchor: None,
        artifact: None,
        router: None,
        mount: None,
    }
}

fn route(
    raw: &str,
    role: Role,
    mechanism: &'static str,
    line: usize,
    router: Option<&str>,
) -> Option<RawRecord> {
    // A router's own `/{id}` is no route until a mount puts a prefix in front.
    let key = if router.is_some() {
        route_shape(raw)?
    } else {
        route_key(raw)?
    };
    Some(RawRecord {
        router: router.map(str::to_string),
        ..record(key, role, mechanism, line)
    })
}

/// A prefix and a path as one path.
pub(super) fn join(prefix: &str, path: &str) -> String {
    let prefix = prefix.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    match (prefix.is_empty(), path.is_empty()) {
        (_, true) => prefix.to_string(),
        (true, false) => format!("/{path}"),
        (false, false) => format!("{prefix}/{path}"),
    }
}

/// The text between the parenthesis at `open` and its match, quotes respected.
fn balanced(text: &str, open: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut quote: Option<u8> = None;
    let end = bytes.len().min(open + 4000);
    let mut i = open;
    while i < end {
        let c = bytes[i];
        match quote {
            Some(q) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => match c {
                b'"' | b'\'' | b'`' => quote = Some(c),
                b'(' => depth += 1,
                b')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return text.get(open + 1..i);
                    }
                }
                _ => {}
            },
        }
        i += 1;
    }
    None
}

/// Arguments split at the commas outside any bracket or quote.
fn split_args(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for (i, c) in args.char_indices() {
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
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(args[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = args[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

fn literal(arg: &str) -> Option<&str> {
    let arg = arg.trim();
    let arg = arg.strip_prefix(['f', 'r', 'b']).unwrap_or(arg);
    let quote = arg
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    arg.strip_prefix(quote)?.strip_suffix(quote)
}

static PREFIX_KWARG: Lazy<Regex> =
    Lazy::new(|| pattern(r#"\b(?:url_prefix|prefix)\s*=\s*[rf]?['"]([^'"]*)['"]"#));

fn prefix_kwarg(args: &str) -> String {
    PREFIX_KWARG
        .captures(args)
        .map_or_else(String::new, |c| c[1].to_string())
}

// ------------------------------------------------------------- constants

static PY_CONST: Lazy<Regex> =
    Lazy::new(|| pattern(r#"(?m)^\s*([A-Za-z_]\w*)\s*(?::[^=\n]+)?=\s*f?['"]([^'"\n]*)['"]\s*$"#));
static JS_CONST: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"(?m)^\s*(?:export\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)\s*(?::[^=\n]+)?=\s*['"`]([^'"`\n$]*)['"`]\s*;?\s*$"#,
    )
});
static GO_CONST: Lazy<Regex> =
    Lazy::new(|| pattern(r#"(?m)^\s*(?:const\s+|var\s+)?([A-Za-z_]\w*)\s*(?::=|=)\s*"([^"\n]*)""#));
static JVM_CONST: Lazy<Regex> = Lazy::new(|| {
    pattern(r#"(?m)\b(?:String|val|var)\s+([A-Za-z_]\w*)\s*(?::\s*String\s*)?=\s*"([^"\n$]*)""#)
});

/// String constants that hold a path or a URL: what a client call can lean on.
fn constants(lang: CodeLang, text: &str) -> Vec<(String, String)> {
    let re: &Regex = match lang {
        CodeLang::Python => &PY_CONST,
        CodeLang::Js => &JS_CONST,
        CodeLang::Go => &GO_CONST,
        CodeLang::Jvm => &JVM_CONST,
        _ => return Vec::new(),
    };
    let mut found: Vec<(String, String)> = re
        .captures_iter(text)
        .filter(|c| c[2].starts_with('/') || c[2].contains("://"))
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect();
    found.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.cmp(b)));
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

/// A client call's literal with its base expression (`API + "/x"`) and the
/// constants of its file put in: `${API}/x`, `{API}/x` and `API + "/x"` are
/// the path the constant holds, then `/x`. An unknown base stands for a host.
fn resolve(base: Option<&str>, path: &str, consts: &[(String, String)]) -> String {
    let lookup = |name: &str| {
        let name = name.rsplit('.').next().unwrap_or(name);
        consts
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let mut s = match base {
        Some(b) => format!("{}{path}", lookup(b).unwrap_or("{}")),
        None => path.to_string(),
    };
    for (name, value) in consts {
        s = s
            .replace(&format!("${{{name}}}"), value)
            .replace(&format!("{{{name}}}"), value);
    }
    s
}

// ------------------------------------------------------------- consumers

/// An optional `NAME +` before a literal; the group's name is the caller's,
/// as a pattern may not repeat one.
fn call_base(group: &str) -> String {
    format!(r"(?:(?P<{group}>[A-Za-z_][\w.]*)\s*\+\s*)?")
}

static PY_USE: Lazy<Regex> = Lazy::new(|| {
    let base = call_base("base");
    pattern(&format!(
        r#"\b(?:requests|httpx|client|session|http|self\.client|api_client|test_client\(\))\.(?:get|post|put|delete|patch|head)\(\s*{base}f?['"](?P<path>[^'"\n]+)['"]"#
    ))
});
static JS_USE: Lazy<Regex> = Lazy::new(|| {
    let base = call_base("base");
    pattern(&format!(
        r#"\b(?:fetch|axios(?:\.(?:get|post|put|delete|patch|head))?|\$http\.\w+|api(?:Client)?\.(?:get|post|put|delete|patch)|http\.(?:get|post|put|delete|patch)|request\([^)]*\)\s*\.(?:get|post|put|delete|patch))\(\s*{base}['"`](?P<path>[^'"`\n]+)['"`]"#
    ))
});
static GO_USE: Lazy<Regex> = Lazy::new(|| {
    let (one, two) = (call_base("base"), call_base("base2"));
    pattern(&format!(
        r#"\bhttp\.(?:Get|Post|Head)\(\s*{one}"(?P<path>[^"\n]+)"|\bhttp(?:test)?\.NewRequest\(\s*[^,\n]+,\s*{two}"(?P<path2>[^"\n]+)""#
    ))
});
static JVM_USE: Lazy<Regex> = Lazy::new(|| {
    let (one, two, three) = (call_base("base"), call_base("base2"), call_base("base3"));
    pattern(&format!(
        r#"\bperform\(\s*(?:get|post|put|delete|patch)\(\s*{one}"(?P<path>[^"\n]+)"|\b(?:getForObject|getForEntity|postForObject|postForEntity|postForLocation)\(\s*{two}"(?P<path2>[^"\n]+)"|\.uri\(\s*{three}"(?P<path3>/[^"\n]*)""#
    ))
});

fn consumers(
    lang: CodeLang,
    text: &str,
    lines: &Lines,
    consts: &[(String, String)],
    out: &mut Vec<RawRecord>,
) {
    let re: &Regex = match lang {
        CodeLang::Python => &PY_USE,
        CodeLang::Js => &JS_USE,
        CodeLang::Go => &GO_USE,
        CodeLang::Jvm => &JVM_USE,
        _ => return,
    };
    for cap in re.captures_iter(text) {
        let Some(path) = ["path", "path2", "path3"].iter().find_map(|n| cap.name(n)) else {
            continue;
        };
        let base = ["base", "base2", "base3"]
            .iter()
            .find_map(|n| cap.name(n))
            .map(|m| m.as_str());
        out.extend(route(
            &resolve(base, path.as_str(), consts),
            Role::Read,
            "http-call",
            lines.of(path.start()),
            None,
        ));
    }
}

// ------------------------------------------------------------- mounts

/// The name a module goes by: the last component of a dotted module
/// (`app.routers.items`) or of a path (`./routes/users.js`, `api/items.py`)
/// without its extension; an `index` or `__init__` stands for its directory.
pub(super) fn module_stem(reference: &str) -> String {
    let reference = reference.trim_end_matches('/');
    let (dir, name) = reference.rsplit_once('/').unwrap_or(("", reference));
    let name = if reference.contains('/') || name.ends_with(".py") {
        name.rsplit_once('.')
            .filter(|(stem, _)| !stem.is_empty())
            .map_or(name, |(stem, _)| stem)
    } else {
        name.rsplit('.').next().unwrap_or(name)
    };
    if matches!(name, "index" | "__init__" | "mod") {
        return dir.rsplit('/').next().unwrap_or_default().to_string();
    }
    name.to_string()
}

/// Name -> (module the name was imported from, the name it had there).
type Imports = FxHashMap<String, (String, String)>;

static PY_IMPORT: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^\s*from\s+([\w.]+)\s+import\s+([^\n(]+)$"));
static JS_IMPORT_DEFAULT: Lazy<Regex> = Lazy::new(|| {
    pattern(r#"(?m)^\s*import\s+(\w+)\s*(?:,\s*\{[^}]*\})?\s*from\s*['"]([^'"]+)['"]"#)
});
static JS_IMPORT_NAMED: Lazy<Regex> =
    Lazy::new(|| pattern(r#"(?m)^\s*import\s*\{([^}]*)\}\s*from\s*['"]([^'"]+)['"]"#));
static JS_REQUIRE: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"(?m)^\s*(?:const|let|var)\s+(?:\{([^}]*)\}|(\w+))\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#,
    )
});

fn named_imports(list: &str, module: &str, into: &mut Imports) {
    for item in list.split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        let (original, alias) = match item.split_once(" as ") {
            Some((o, a)) => (o.trim(), a.trim()),
            None => match item.split_once(':') {
                Some((o, a)) => (o.trim(), a.trim()),
                None => (item, item),
            },
        };
        into.insert(
            alias.to_string(),
            (module_stem(module), original.to_string()),
        );
    }
}

fn imports(lang: CodeLang, text: &str) -> Imports {
    let mut found = Imports::default();
    match lang {
        CodeLang::Python => {
            for c in PY_IMPORT.captures_iter(text) {
                named_imports(&c[2], &c[1], &mut found);
            }
        }
        CodeLang::Js => {
            for c in JS_IMPORT_DEFAULT.captures_iter(text) {
                found.insert(c[1].to_string(), (module_stem(&c[2]), String::new()));
            }
            for c in JS_IMPORT_NAMED.captures_iter(text) {
                named_imports(&c[1], &c[2], &mut found);
            }
            for c in JS_REQUIRE.captures_iter(text) {
                match (c.get(1), c.get(2)) {
                    (Some(list), _) => named_imports(list.as_str(), &c[3], &mut found),
                    (None, Some(name)) => {
                        found.insert(
                            name.as_str().to_string(),
                            (module_stem(&c[3]), String::new()),
                        );
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    found
}

/// Where a mounted expression lives: `items.router`, `router`, `users`,
/// `require('./routes/users')`.
fn mount_target(expr: &str, local: &FxHashMap<String, String>, imports: &Imports) -> Mount {
    let expr = expr.trim();
    if let Some(open) = expr.find("require(") {
        let path = literal(expr[open + 8..].trim_end_matches(')')).unwrap_or_default();
        return Mount {
            modules: vec![module_stem(path)],
            router: String::new(),
        };
    }
    if local.contains_key(expr) {
        return Mount {
            modules: Vec::new(),
            router: expr.to_string(),
        };
    }
    let mut modules: Vec<String> = Vec::new();
    let mut push = |m: &str| {
        if !m.is_empty() && !modules.iter().any(|x| x == m) {
            modules.push(m.to_string());
        }
    };
    let router = if let Some((base, attr)) = expr.rsplit_once('.') {
        let head = base.rsplit('.').next().unwrap_or(base);
        push(head);
        if let Some((from, original)) = imports.get(head) {
            push(from);
            push(original);
        }
        attr.to_string()
    } else if let Some((from, original)) = imports.get(expr) {
        push(from);
        if original.is_empty() {
            String::new()
        } else {
            original.clone()
        }
    } else {
        push(expr);
        String::new()
    };
    Mount { modules, router }
}

fn mount(prefix: &str, target: Mount, line: usize) -> RawRecord {
    RawRecord {
        mount: Some(target),
        ..record(prefix.to_string(), Role::Mount, "route-mount", line)
    }
}

// ------------------------------------------------------------- python

static PY_ROUTER: Lazy<Regex> = Lazy::new(|| {
    pattern(r"(?m)^\s*(\w+)\s*(?::[^=\n]+)?=\s*(?:[\w.]+\.)?(?:APIRouter|Blueprint)\(")
});
static PY_DEF: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"(?m)^\s*@(\w+(?:\.\w+)*)\.(?:get|post|put|delete|patch|head|options|route|api_route|websocket)\(\s*(?:path\s*=\s*)?[rbf]?['"]([^'"\n]*)['"]"#,
    )
});
static PY_MOUNT: Lazy<Regex> =
    Lazy::new(|| pattern(r"\b\w+\.(?:include_router|register_blueprint)\("));

fn python(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let mut routers: FxHashMap<String, String> = FxHashMap::default();
    for m in PY_ROUTER.captures_iter(text) {
        let whole = m.get(0).expect("whole match");
        let args = balanced(text, whole.end() - 1).unwrap_or_default();
        routers.insert(m[1].to_string(), prefix_kwarg(args));
    }
    for cap in PY_DEF.captures_iter(text) {
        let var = cap[1].rsplit('.').next().unwrap_or(&cap[1]);
        let prefix = routers.get(var).map_or("", String::as_str);
        let at = cap.get(2).expect("path group");
        out.extend(route(
            &join(prefix, at.as_str()),
            Role::Set,
            "route-decorator",
            lines.of(at.start()),
            Some(var),
        ));
    }
    let imported = imports(CodeLang::Python, text);
    for m in PY_MOUNT.find_iter(text) {
        let Some(args) = balanced(text, m.end() - 1) else {
            continue;
        };
        let parts = split_args(args);
        let Some(first) = parts.first() else {
            continue;
        };
        out.push(mount(
            &prefix_kwarg(args),
            mount_target(first, &routers, &imported),
            lines.of(m.start()),
        ));
    }
}

// ------------------------------------------------------------- javascript

static JS_ROUTER: Lazy<Regex> = Lazy::new(|| {
    pattern(r"(?m)^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*=\s*(?:express\.)?Router\(")
});
static JS_DEF: Lazy<Regex> = Lazy::new(|| {
    pattern(r#"\b(\w+)\.(?:get|post|put|delete|patch|all|options|head)\(\s*['"`]([^'"`\n]*)['"`]"#)
});
static JS_MOUNT: Lazy<Regex> = Lazy::new(|| pattern(r"\b\w+\.use\("));
static APP_LIKE: Lazy<Regex> =
    Lazy::new(|| pattern(r"^(?:app|server|fastify|routes?|\w*[Rr]outer)$"));

fn javascript(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let mut routers: FxHashMap<String, String> = FxHashMap::default();
    for m in JS_ROUTER.captures_iter(text) {
        routers.insert(m[1].to_string(), String::new());
    }
    for cap in JS_DEF.captures_iter(text) {
        let var = &cap[1];
        if !routers.contains_key(var) && !APP_LIKE.is_match(var) {
            continue;
        }
        let at = cap.get(2).expect("path group");
        out.extend(route(
            at.as_str(),
            Role::Set,
            "route-registration",
            lines.of(at.start()),
            Some(var),
        ));
    }
    let imported = imports(CodeLang::Js, text);
    for m in JS_MOUNT.find_iter(text) {
        let Some(args) = balanced(text, m.end() - 1) else {
            continue;
        };
        let parts = split_args(args);
        let (Some(prefix), Some(last)) = (parts.first().and_then(|a| literal(a)), parts.last())
        else {
            continue;
        };
        if parts.len() < 2 {
            continue;
        }
        out.push(mount(
            prefix,
            mount_target(last, &routers, &imported),
            lines.of(m.start()),
        ));
    }
}

// ------------------------------------------------------------- jvm

static JVM_CLASS_MAPPING: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r"@(?:RequestMapping|Path)\(([^)]*)\)\s*(?:@\w+(?:\([^)]*\))?\s*)*(?:(?:public|abstract|final|sealed|open|internal)\s+)*(?:class|interface|object)\b",
    )
});
static JVM_MAPPING_PATH: Lazy<Regex> =
    Lazy::new(|| pattern(r#"^\s*(?:(?:value|path)\s*=\s*)?\{?\s*"([^"]*)""#));
static JVM_DEF: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"@(?:(?:Get|Post|Put|Delete|Patch|Request)Mapping\(\s*(?:(?:value|path)\s*=\s*)?\{?\s*|Path\(\s*)"([^"\n]*)""#,
    )
});
static JVM_BARE: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)@(?:Get|Post|Put|Delete|Patch)Mapping(?:\(\s*\))?[ \t]*$"));

fn jvm(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let classes: Vec<(usize, String)> = JVM_CLASS_MAPPING
        .captures_iter(text)
        .map(|c| {
            let prefix = JVM_MAPPING_PATH
                .captures(&c[1])
                .map_or_else(String::new, |p| p[1].to_string());
            (c.get(0).expect("whole match").start(), prefix)
        })
        .collect();
    let prefix_at = |offset: usize| {
        classes
            .iter()
            .rev()
            .find(|(start, _)| *start < offset)
            .map_or("", |(_, p)| p.as_str())
    };
    for cap in JVM_DEF.captures_iter(text) {
        let start = cap.get(0).expect("whole match").start();
        if classes.iter().any(|(s, _)| *s == start) {
            continue;
        }
        let at = cap.get(1).expect("path group");
        out.extend(route(
            &join(prefix_at(start), at.as_str()),
            Role::Set,
            "route-annotation",
            lines.of(at.start()),
            None,
        ));
    }
    for m in JVM_BARE.find_iter(text) {
        let prefix = prefix_at(m.start());
        if !prefix.is_empty() {
            out.extend(route(
                prefix,
                Role::Set,
                "route-annotation",
                lines.of(m.start()),
                None,
            ));
        }
    }
}

// ------------------------------------------------------------- go

static GO_GROUP: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"(?m)^\s*(\w+)\s*:?=\s*(\w+)\.(?:PathPrefix\(\s*"([^"]*)"\s*\)\.Subrouter\(\)|Group\(\s*"([^"]*)")"#,
    )
});
static GO_DEF: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"\b(\w+)\.(?:HandleFunc|Handle|Get|Post|Put|Delete|Patch|GET|POST|PUT|DELETE|PATCH|Any)\(\s*"([^"\n]+)""#,
    )
});

fn go(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let mut prefixes: FxHashMap<String, String> = FxHashMap::default();
    for c in GO_GROUP.captures_iter(text) {
        let own = c.get(3).or_else(|| c.get(4)).map_or("", |m| m.as_str());
        let parent = prefixes.get(&c[2]).map_or("", String::as_str);
        let full = join(parent, own);
        prefixes.insert(c[1].to_string(), full);
    }
    for cap in GO_DEF.captures_iter(text) {
        let prefix = prefixes.get(&cap[1]).map_or("", String::as_str);
        let at = cap.get(2).expect("path group");
        out.extend(route(
            &join(prefix, at.as_str()),
            Role::Set,
            "route-registration",
            lines.of(at.start()),
            None,
        ));
    }
}

pub(super) fn code_routes(lang: CodeLang, text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    match lang {
        CodeLang::Python => python(text, lines, out),
        CodeLang::Js => javascript(text, lines, out),
        CodeLang::Jvm => jvm(text, lines, out),
        CodeLang::Go => go(text, lines, out),
        _ => {}
    }
    let consts = constants(lang, text);
    consumers(lang, text, lines, &consts, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(lang: CodeLang, text: &str) -> Vec<(String, Role)> {
        let mut out = Vec::new();
        code_routes(lang, text, &Lines::new(text), &mut out);
        let mut v: Vec<(String, Role)> = out.into_iter().map(|r| (r.key, r.role)).collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v
    }

    #[test]
    fn a_flask_blueprint_and_a_fastapi_router_serve_their_prefix() {
        let flask = "bp = Blueprint(\"users\", __name__, url_prefix=\"/api/users\")\n\n@bp.route(\"/<int:user_id>\")\ndef one(user_id): ...\n";
        assert_eq!(
            found(CodeLang::Python, flask),
            vec![("/api/users/{}".to_string(), Role::Set)]
        );
        let fastapi = "router = APIRouter(prefix=\"/items\", tags=[\"items\"], dependencies=[Depends(auth)])\n\n@router.get(\"/{item_id}\")\nasync def one(item_id): ...\n\n@router.post(\"\")\nasync def add(): ...\n";
        assert_eq!(
            found(CodeLang::Python, fastapi),
            vec![
                ("/items".to_string(), Role::Set),
                ("/items/{}".to_string(), Role::Set)
            ]
        );
    }

    #[test]
    fn a_mount_names_its_router_by_module() {
        let main = "from app.routers import items\nfrom app.routers.users import router as users_router\napp.include_router(items.router, prefix=\"/api\")\napp.include_router(users_router, prefix=\"/api/v2\")\n";
        let mut out = Vec::new();
        code_routes(CodeLang::Python, main, &Lines::new(main), &mut out);
        let mounts: Vec<(String, Vec<String>, String)> = out
            .into_iter()
            .filter(|r| r.role == Role::Mount)
            .map(|r| {
                let m = r.mount.unwrap();
                (r.key, m.modules, m.router)
            })
            .collect();
        assert_eq!(
            mounts,
            vec![
                (
                    "/api".to_string(),
                    vec!["items".to_string(), "routers".to_string()],
                    "router".to_string()
                ),
                (
                    "/api/v2".to_string(),
                    vec!["users".to_string()],
                    "router".to_string()
                ),
            ]
        );
    }

    #[test]
    fn express_registrations_and_mounts() {
        let router_file =
            "const router = express.Router();\nrouter.get('/:id', show);\nrouter.get('/', list);\n";
        assert_eq!(
            found(CodeLang::Js, router_file),
            vec![("/".to_string(), Role::Set), ("/{}".to_string(), Role::Set)]
        );
        let main =
            "const users = require('./routes/users');\napp.use('/api/users', auth, users);\n";
        let mut out = Vec::new();
        code_routes(CodeLang::Js, main, &Lines::new(main), &mut out);
        let m = out.iter().find(|r| r.role == Role::Mount).unwrap();
        assert_eq!(m.key, "/api/users");
        assert_eq!(m.mount.as_ref().unwrap().modules, vec!["users".to_string()]);
    }

    #[test]
    fn spring_composes_class_and_method_mappings() {
        let java = "@RestController\n@RequestMapping(\"/api/orders\")\npublic class OrderController {\n    @GetMapping(\"/{id}\")\n    Order one(@PathVariable long id) { return null; }\n\n    @PostMapping\n    Order add() { return null; }\n}\n";
        assert_eq!(
            found(CodeLang::Jvm, java),
            vec![
                ("/api/orders".to_string(), Role::Set),
                ("/api/orders/{}".to_string(), Role::Set)
            ]
        );
    }

    #[test]
    fn go_subrouters_and_groups_compose() {
        let go = "api := r.PathPrefix(\"/api\").Subrouter()\nv1 := api.PathPrefix(\"/v1\").Subrouter()\nv1.HandleFunc(\"/orders/{id}\", h)\ng := e.Group(\"/admin\")\ng.GET(\"/users\", h)\nr.HandleFunc(\"/health\", h)\n";
        assert_eq!(
            found(CodeLang::Go, go),
            vec![
                ("/admin/users".to_string(), Role::Set),
                ("/api/v1/orders/{}".to_string(), Role::Set),
                ("/health".to_string(), Role::Set),
            ]
        );
    }

    #[test]
    fn a_client_call_leans_on_its_base_constant() {
        let ts = "const API = \"/api\";\nconst BASE = \"https://svc.example.com/v1\";\nfetch(`${API}/orders/${id}`);\naxios.get(BASE + \"/parts\");\nfetch(`${HOST}/other`);\n";
        assert_eq!(
            found(CodeLang::Js, ts),
            vec![
                ("/api/orders/{}".to_string(), Role::Read),
                ("/other".to_string(), Role::Read),
                ("/v1/parts".to_string(), Role::Read),
            ]
        );
        let py =
            "API = \"/api\"\nrequests.get(f\"{API}/orders/{oid}\")\nclient.get(\"/api/health\")\n";
        assert_eq!(
            found(CodeLang::Python, py),
            vec![
                ("/api/health".to_string(), Role::Read),
                ("/api/orders/{}".to_string(), Role::Read),
            ]
        );
        let java = "mockMvc.perform(get(\"/api/orders/1\"));\nrestTemplate.getForObject(\"/api/parts\", String.class);\n";
        assert_eq!(
            found(CodeLang::Jvm, java),
            vec![
                ("/api/orders/{}".to_string(), Role::Read),
                ("/api/parts".to_string(), Role::Read)
            ]
        );
    }
}
