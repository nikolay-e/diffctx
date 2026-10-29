//! What each file spells as a typed key: the environment variables it reads or
//! sets and the container images it builds, consumes or is built from.
//!
//! One scan per file class, on the file's whole text. A record is a fact about
//! a line; which fragment the line sits in is the caller's business.

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashSet;

use super::super::base::normalize_lexical;
use super::tech::{kafka_property, property_keys};
use crate::canon::canon_key;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub(crate) enum Kind {
    Env,
    Image,
    Route,
    Table,
    Proto,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Image => "image",
            Self::Route => "route",
            Self::Table => "table",
            Self::Proto => "proto",
        }
    }
}

/// A strong image key is the reference as spelled without tag or digest; a
/// weak one drops the registry too, so a manifest that says `acme/web` meets a
/// pipeline that pushes `registry.example.com/acme/web`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub(crate) enum Strength {
    Strong,
    Weak,
}

impl Strength {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Strong => "strong",
            Self::Weak => "weak",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Role {
    /// Code or a template that takes the variable from its environment.
    Read,
    /// A file that gives the variable a value.
    Set,
    /// A build declaration that produces the image.
    Build,
    /// A file that runs or derives from the image.
    Consume,
    /// The Dockerfile a build declaration names by path.
    Artifact,
    /// A router mounted under a prefix: composes route keys, joins nothing.
    Mount,
}

pub(crate) fn compatible(kind: Kind, a: Role, b: Role) -> bool {
    use Role::{Artifact, Build, Consume, Read, Set};
    match kind {
        Kind::Env => matches!((a, b), (Read, Set) | (Set, Read) | (Set, Set)),
        // A path is defined by the code that serves it and named by whatever
        // calls, proxies or documents it.
        Kind::Route => matches!((a, b), (Read, Set) | (Set, Read)),
        // A table is defined by migrations and models, queried by code; the
        // migration and the model that map it are definitions of one thing.
        Kind::Table => matches!((a, b), (Read, Set) | (Set, Read) | (Set, Set)),
        // A message or service is declared in a .proto and used by the code
        // generated from it.
        Kind::Proto => matches!((a, b), (Read, Set) | (Set, Read)),
        Kind::Image => matches!(
            (a, b),
            (Build, Consume)
                | (Consume, Build)
                | (Consume, Consume)
                | (Build, Artifact)
                | (Artifact, Build)
        ),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RawRecord {
    pub kind: Kind,
    pub key: String,
    pub strength: Strength,
    pub role: Role,
    pub mechanism: &'static str,
    /// 1-based line of the file's text.
    pub line: usize,
    /// The last line the record spans: a Helm `image:` block, a kustomize
    /// image item. A change anywhere in the span touches the record.
    pub end: usize,
    /// The directory the record speaks for when it is not its file's own: a
    /// declared build context, the directory of an `env_file`.
    pub anchor: Option<String>,
    /// The repo-relative path of the Dockerfile a build declaration names.
    pub artifact: Option<String>,
    /// The router object a route is registered on (`router`, `bp`), the name
    /// a mount refers to it by.
    pub router: Option<String>,
    /// For a [`Role::Mount`]: where the mounted router lives.
    pub mount: Option<Mount>,
}

/// A router mounted under a prefix (`app.include_router(items.router,
/// prefix="/api")`, `app.use('/api', users)`): the record's key is the prefix.
#[derive(Clone, Debug)]
pub(crate) struct Mount {
    /// Names the mounted router's module may go by; empty for a router of
    /// the mounting file itself.
    pub modules: Vec<String>,
    /// The mounted router's own name; empty when the mount names a module.
    pub router: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CodeLang {
    Python,
    Js,
    Go,
    Rust,
    Jvm,
    Ruby,
    Shell,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FileClass {
    Code(CodeLang),
    Dotenv,
    Dockerfile,
    Yaml {
        compose: bool,
    },
    Terraform,
    /// nginx configuration.
    Conf,
    /// SQL files and Prisma schemas.
    Sql,
    /// Protocol buffer definitions.
    Proto,
    /// Java-style .properties files that carry settings (Spring, Kafka).
    Properties,
    /// Any other .template file: envsubst reads the environment.
    Template,
    /// JSON that may be an OpenAPI document.
    Json,
    /// Makefile, Jenkinsfile and the like: only their `docker` commands.
    Script,
}

pub(crate) fn classify(rel: &str) -> Option<FileClass> {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    if name == "dockerfile" || name.starts_with("dockerfile.") || name.ends_with(".dockerfile") {
        return Some(FileClass::Dockerfile);
    }
    if name.starts_with(".env") || name.ends_with(".env") {
        return Some(FileClass::Dotenv);
    }
    if matches!(name.as_str(), "makefile" | "justfile") || name.starts_with("jenkinsfile") {
        return Some(FileClass::Script);
    }
    if name.ends_with(".conf.template") {
        return Some(FileClass::Conf);
    }
    if name.ends_with(".template") {
        return Some(FileClass::Template);
    }
    if name.ends_with(".properties")
        && (name.starts_with("application")
            || name.starts_with("bootstrap")
            || name.contains("kafka")
            || matches!(
                name.as_str(),
                "server.properties" | "consumer.properties" | "producer.properties"
            ))
    {
        return Some(FileClass::Properties);
    }
    let ext = name.rsplit_once('.').map(|(_, e)| e)?;
    Some(match ext {
        "yml" | "yaml" => FileClass::Yaml {
            compose: name.starts_with("docker-compose") || name.starts_with("compose."),
        },
        "tf" => FileClass::Terraform,
        "conf" => FileClass::Conf,
        "sql" | "prisma" => FileClass::Sql,
        "proto" => FileClass::Proto,
        "json" => FileClass::Json,
        "mk" => FileClass::Script,
        "py" => FileClass::Code(CodeLang::Python),
        "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "mts" | "cts" | "vue" | "svelte" => {
            FileClass::Code(CodeLang::Js)
        }
        "go" => FileClass::Code(CodeLang::Go),
        "rs" => FileClass::Code(CodeLang::Rust),
        "java" | "kt" | "kts" => FileClass::Code(CodeLang::Jvm),
        "rb" | "rake" => FileClass::Code(CodeLang::Ruby),
        "sh" | "bash" | "zsh" => FileClass::Code(CodeLang::Shell),
        _ => return None,
    })
}

/// Whether a file of this class is worth reading whole. Code carries routes,
/// queries and table names as well as environment reads, so it always is;
/// the other classes keep the cheap tests that spare them.
pub(crate) fn worth_reading<'a>(
    class: FileClass,
    mut texts: impl Iterator<Item = &'a str>,
) -> bool {
    match class {
        FileClass::Code(CodeLang::Shell) => texts.any(|t| t.contains('$') || t.contains("docker")),
        FileClass::Code(_) => true,
        FileClass::Json => texts.any(|t| t.contains("openapi") || t.contains("swagger")),
        FileClass::Script => texts.any(|t| t.contains("docker")),
        _ => true,
    }
}

/// Names the platform sets for every process: not the project's settings.
const PLATFORM_ENV: &[&str] = &[
    "path",
    "home",
    "user",
    "pwd",
    "oldpwd",
    "shell",
    "term",
    "lang",
    "lc_all",
    "tz",
    "hostname",
    "tmpdir",
    "temp",
    "tmp",
    "ci",
    "logname",
    "editor",
    "pager",
    "display",
    "debian_frontend",
];

fn env_record(name: &str, role: Role, mechanism: &'static str, line: usize) -> Option<RawRecord> {
    let key = canon_key(name);
    if key.len() < 2 || PLATFORM_ENV.contains(&key.as_str()) {
        return None;
    }
    Some(RawRecord {
        kind: Kind::Env,
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
    })
}

/// A setting that answers to several keys (a Spring property and the variable
/// that binds to it): one record per key.
fn env_records(
    keys: Vec<String>,
    role: Role,
    mechanism: &'static str,
    line: usize,
) -> Vec<RawRecord> {
    keys.into_iter()
        .filter(|k| !PLATFORM_ENV.contains(&k.as_str()))
        .map(|key| RawRecord {
            kind: Kind::Env,
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
        })
        .collect()
}

pub(super) struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    pub(super) fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Self { starts }
    }

    pub(super) fn of(&self, offset: usize) -> usize {
        self.starts.partition_point(|s| *s <= offset).max(1)
    }
}

pub(super) fn pattern(source: &str) -> Regex {
    Regex::new(source).unwrap()
}

const NAME: &str = r"([A-Za-z_][A-Za-z0-9_]*)";

static PY_ENV: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"\b(?:os\.)?environ(?:\.get\(|\.setdefault\(|\.pop\(|\[)\s*['"]{NAME}['"]|\bgetenv\(\s*['"]{NAME}['"]"#
    ))
});
static JS_ENV: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"\b(?:process|Bun)\.env\.{NAME}|\b(?:process|Bun)\.env\[\s*['"`]{NAME}['"`]\s*\]|\bimport\.meta\.env\.{NAME}|\bDeno\.env\.get\(\s*['"`]{NAME}['"`]"#
    ))
});
static GO_ENV: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"\bos\.(?:Getenv|LookupEnv)\(\s*"{NAME}"|\bviper\.BindEnv\(\s*"{NAME}""#
    ))
});
static RUST_ENV: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"\benv::var(?:_os)?\(\s*"{NAME}"|\b(?:option_)?env!\(\s*"{NAME}""#
    ))
});
static JVM_ENV: Lazy<Regex> = Lazy::new(|| pattern(&format!(r#"\bSystem\.getenv\(\s*"{NAME}""#)));
static RUBY_ENV: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"\bENV\s*\[\s*['"]{NAME}['"]\s*\]|\bENV\.(?:fetch|key\?)\(\s*['"]{NAME}['"]"#
    ))
});
static SH_READ: Lazy<Regex> = Lazy::new(|| pattern(r"\$\{?([A-Z][A-Z0-9_]{2,})"));
static SH_ASSIGN: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r"(?m)^\s*(?:export\s+|declare\s+(?:-x\s+)?|local\s+|readonly\s+)?([A-Za-z_][A-Za-z0-9_]*)=",
    )
});
static SH_EXPORT: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^\s*export\s+([A-Za-z_][A-Za-z0-9_]*)="));
static SH_LOOP: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^\s*(?:for|read(?:\s+-\w+)*)\s+([A-Za-z_][A-Za-z0-9_]*)\b"));
static DOTENV: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^\s*(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*="));

fn first_group<'a>(cap: &regex::Captures<'a>) -> Option<(&'a str, usize)> {
    (1..cap.len())
        .find_map(|i| cap.get(i))
        .map(|m| (m.as_str(), m.start()))
}

fn code_reads(lang: CodeLang, text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let re: &Regex = match lang {
        CodeLang::Python => &PY_ENV,
        CodeLang::Js => &JS_ENV,
        CodeLang::Go => &GO_ENV,
        CodeLang::Rust => &RUST_ENV,
        CodeLang::Jvm => &JVM_ENV,
        CodeLang::Ruby => &RUBY_ENV,
        CodeLang::Shell => return shell_reads(text, lines, out),
    };
    for cap in re.captures_iter(text) {
        if let Some((name, at)) = first_group(&cap) {
            out.extend(env_record(name, Role::Read, "env-read", lines.of(at)));
        }
    }
}

fn shell_reads(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let assigned: FxHashSet<&str> = SH_ASSIGN
        .captures_iter(text)
        .chain(SH_LOOP.captures_iter(text))
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str())
        .collect();
    for cap in SH_EXPORT.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            out.extend(env_record(
                m.as_str(),
                Role::Set,
                "shell-export",
                lines.of(m.start()),
            ));
        }
    }
    for cap in SH_READ.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            if !assigned.contains(m.as_str()) {
                out.extend(env_record(
                    m.as_str(),
                    Role::Read,
                    "shell-read",
                    lines.of(m.start()),
                ));
            }
        }
    }
}

fn dotenv(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    for cap in DOTENV.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            out.extend(env_record(
                m.as_str(),
                Role::Set,
                "dotenv",
                lines.of(m.start()),
            ));
        }
    }
}

// ---------------------------------------------------------------- images

/// `ghcr.io/Acme/Web:1.2@sha256:...` -> (`ghcr.io/acme/web`, `acme/web`). The
/// tag may be a template (`:${{ github.sha }}`), the repository may not: a
/// leading variable stands for an unknown registry and is dropped, any other
/// variable makes the reference no key.
pub(crate) fn image_keys(raw: &str) -> Option<(String, String)> {
    let lowered = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'')
        .to_ascii_lowercase();
    let repository = match lowered.rsplit_once('/') {
        Some((head, last)) => format!("{head}/{}", last.split([':', '@']).next()?),
        None => lowered.split([':', '@']).next()?.to_string(),
    };
    let mut parts: Vec<&str> = repository.split('/').collect();
    if parts.len() > 2 && parts[0].contains(['$', '{']) {
        parts.remove(0);
    }
    let strong = parts.join("/");
    let valid = !strong.is_empty()
        && strong.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-' | '/' | ':')
        })
        && strong.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !strong.ends_with('/')
        && strong != "scratch";
    if !valid {
        return None;
    }
    if parts.len() > 1 && (parts[0].contains(['.', ':']) || parts[0] == "localhost") {
        parts.remove(0);
    }
    if parts.len() > 1 && parts[0] == "library" {
        parts.remove(0);
    }
    Some((strong, parts.join("/")))
}

fn image_records(
    raw: &str,
    role: Role,
    mechanism: &'static str,
    line: usize,
    anchor: Option<&str>,
    artifact: Option<&str>,
    out: &mut Vec<RawRecord>,
) {
    let Some((strong, weak)) = image_keys(raw) else {
        return;
    };
    for (key, strength) in [(strong, Strength::Strong), (weak, Strength::Weak)] {
        out.push(RawRecord {
            kind: Kind::Image,
            key,
            strength,
            role,
            mechanism,
            line,
            end: line,
            anchor: anchor.map(str::to_string),
            artifact: artifact.map(str::to_string),
            router: None,
            mount: None,
        });
    }
}

/// Extends the records pushed since `from` to cover through `end`.
fn widen(out: &mut [RawRecord], from: usize, end: usize) {
    for record in &mut out[from..] {
        record.end = record.end.max(end);
    }
}

/// Physical lines joined across trailing backslashes: `(first line, text)`.
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut current: Option<(usize, String)> = None;
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_end();
        let (body, continues) = match trimmed.strip_suffix('\\') {
            Some(b) => (b, true),
            None => (trimmed, false),
        };
        match &mut current {
            Some((_, acc)) => {
                acc.push(' ');
                acc.push_str(body.trim());
            }
            None => current = Some((i + 1, body.to_string())),
        }
        if !continues {
            out.extend(current.take());
        }
    }
    out.extend(current);
    out
}

static DOCKER_BUILD: Lazy<Regex> =
    Lazy::new(|| pattern(r"\b(?:docker|podman)\s+(?:buildx\s+)?build\b(.*)"));
static DOCKER_RUN: Lazy<Regex> =
    Lazy::new(|| pattern(r"\b(?:docker|podman)\s+(?:compose\s+)?run\b(.*)"));

/// Options of `docker build` that take a value in the next token.
const BUILD_VALUE_FLAGS: &[&str] = &[
    "-t",
    "--tag",
    "-f",
    "--file",
    "--build-arg",
    "--target",
    "--platform",
    "--label",
    "--cache-from",
    "--cache-to",
    "--secret",
    "--ssh",
    "--output",
    "-o",
    "--network",
    "--progress",
    "--provenance",
    "--sbom",
    "--builder",
    "--iidfile",
    "--add-host",
    "--build-context",
    "--ulimit",
    "-m",
];

fn join_dir(base: &str, rel: &str) -> Option<String> {
    if rel.starts_with('/')
        || rel.contains(['$', '{'])
        || rel.contains("://")
        || rel.starts_with("git@")
    {
        return None;
    }
    let joined = if base.is_empty() {
        rel.to_string()
    } else {
        format!("{base}/{rel}")
    };
    normalize_lexical(&joined)
}

fn dockerfile_in(dir: &str, name: &str) -> String {
    let joined = if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    };
    normalize_lexical(&joined).unwrap_or(joined)
}

fn docker_commands(text: &str, out: &mut Vec<RawRecord>) {
    for (line, logical) in logical_lines(text) {
        if let Some(cap) = DOCKER_BUILD.captures(&logical) {
            let tokens: Vec<&str> = cap[1].split_whitespace().collect();
            let (mut tags, mut file, mut context) = (Vec::new(), None, None);
            let mut i = 0;
            while i < tokens.len() {
                let t = tokens[i];
                let (flag, inline) = match t.split_once('=') {
                    Some((f, v)) if f.starts_with('-') => (f, Some(v)),
                    _ => (t, None),
                };
                if BUILD_VALUE_FLAGS.contains(&flag) {
                    let value = inline.or_else(|| {
                        i += 1;
                        tokens.get(i).copied()
                    });
                    match (flag, value) {
                        ("-t" | "--tag", Some(v)) => tags.push(v),
                        ("-f" | "--file", Some(v)) => file = Some(v),
                        ("--build-arg", Some(v)) => out.extend(env_record(
                            v.split('=').next().unwrap_or(v),
                            Role::Set,
                            "docker-build-arg",
                            line,
                        )),
                        _ => {}
                    }
                } else if !t.starts_with('-') && context.is_none() {
                    context = Some(t);
                }
                i += 1;
            }
            let context_dir = context.and_then(|c| join_dir("", c));
            let artifact = match file {
                Some(f) => join_dir("", f),
                None => context_dir
                    .as_deref()
                    .map(|d| dockerfile_in(d, "Dockerfile")),
            };
            for tag in tags {
                image_records(
                    tag,
                    Role::Build,
                    "docker-build",
                    line,
                    context_dir.as_deref(),
                    artifact.as_deref(),
                    out,
                );
            }
        }
        if DOCKER_RUN.is_match(&logical) {
            let tokens: Vec<&str> = logical.split_whitespace().collect();
            for (i, t) in tokens.iter().enumerate() {
                let value = match *t {
                    "-e" | "--env" => tokens.get(i + 1).copied(),
                    _ => t.strip_prefix("--env="),
                };
                if let Some(v) = value {
                    out.extend(env_record(
                        v.split('=').next().unwrap_or(v),
                        Role::Set,
                        "docker-run-env",
                        line,
                    ));
                }
            }
        }
    }
}

static FROM: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?i)^FROM\s+(?:--\S+\s+)*(\S+)(?:\s+AS\s+(\S+))?"));
static ENV_PAIR: Lazy<Regex> = Lazy::new(|| pattern(r"(?:^|\s)([A-Za-z_][A-Za-z0-9_]*)="));
static ENV_LEGACY: Lazy<Regex> = Lazy::new(|| pattern(r"^([A-Za-z_][A-Za-z0-9_]*)\s+\S"));
static ARG: Lazy<Regex> = Lazy::new(|| pattern(r"(?i)^ARG\s+([A-Za-z_][A-Za-z0-9_]*)"));

fn dockerfile(text: &str, out: &mut Vec<RawRecord>) {
    let mut stages: FxHashSet<String> = FxHashSet::default();
    for (line, logical) in logical_lines(text) {
        let l = logical.trim();
        if let Some(cap) = FROM.captures(l) {
            let reference = cap[1].to_string();
            if !stages.contains(&reference.to_ascii_lowercase()) {
                image_records(
                    &reference,
                    Role::Consume,
                    "dockerfile-from",
                    line,
                    None,
                    None,
                    out,
                );
            }
            if let Some(stage) = cap.get(2) {
                stages.insert(stage.as_str().to_ascii_lowercase());
            }
        } else if let Some(rest) = l.strip_prefix("ENV ").or_else(|| l.strip_prefix("env ")) {
            let rest = rest.trim();
            if rest.contains('=') {
                for cap in ENV_PAIR.captures_iter(rest) {
                    out.extend(env_record(&cap[1], Role::Set, "dockerfile-env", line));
                }
            } else if let Some(cap) = ENV_LEGACY.captures(rest) {
                out.extend(env_record(&cap[1], Role::Set, "dockerfile-env", line));
            }
        } else if let Some(cap) = ARG.captures(l) {
            out.extend(env_record(&cap[1], Role::Set, "dockerfile-arg", line));
        }
    }
}

// ---------------------------------------------------------------- YAML

struct Entry {
    line: usize,
    key: String,
    value: String,
    parent: Option<usize>,
    /// First entry of a list item.
    item: bool,
    /// A list item that is a bare scalar, or a line of a block scalar.
    plain: bool,
}

fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '"' || c == '\'')
}

/// Text before a comment: a `#` that starts the line or follows a space.
fn strip_comment(line: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for (i, c) in line.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '#') if prev.is_whitespace() => return &line[..i],
            _ => {}
        }
        prev = c;
    }
    line
}

/// The key and value of `key: value`, when the line is a mapping entry.
fn split_mapping(rest: &str) -> Option<(String, String)> {
    let mut quote: Option<char> = None;
    let bytes = rest.as_bytes();
    for (i, c) in rest.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') if i == 0 => quote = Some(c),
            (None, ':') if i + 1 == bytes.len() || bytes[i + 1] == b' ' => {
                let key = unquote(&rest[..i]);
                if key.is_empty() || key.contains("{{") {
                    return None;
                }
                return Some((key.to_string(), rest[i + 1..].trim().to_string()));
            }
            _ => {}
        }
    }
    None
}

fn parse_yaml(text: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    let mut stack: Vec<(usize, usize)> = Vec::new();
    let mut block: Option<(usize, usize)> = None;
    for (n, raw) in text.lines().enumerate() {
        let line = n + 1;
        let indent = raw.len() - raw.trim_start().len();
        if let Some((owner, col)) = block {
            if raw.trim().is_empty() || indent > col {
                if !raw.trim().is_empty() {
                    entries.push(Entry {
                        line,
                        key: String::new(),
                        value: raw.trim().to_string(),
                        parent: Some(owner),
                        item: false,
                        plain: true,
                    });
                }
                continue;
            }
            block = None;
        }
        let body = strip_comment(raw).trim_end();
        let trimmed = body.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "---" || trimmed.starts_with("--- ") {
            stack.clear();
            continue;
        }
        let mut col = indent;
        let mut rest = trimmed;
        let mut item = false;
        while rest == "-" || rest.starts_with("- ") {
            item = true;
            let after = rest[1..].trim_start();
            col += rest.len() - after.len();
            rest = after;
        }
        if rest.is_empty() {
            continue;
        }
        let (key, value, plain) = match split_mapping(rest) {
            Some((k, v)) => (k, v, false),
            None => (String::new(), rest.to_string(), true),
        };
        while stack.last().is_some_and(|(c, _)| *c >= col) {
            stack.pop();
        }
        let parent = stack.last().map(|(_, i)| *i);
        let index = entries.len();
        let opens_block = !plain && (value.starts_with('|') || value.starts_with('>'));
        entries.push(Entry {
            line,
            key,
            value,
            parent,
            item,
            plain,
        });
        if !plain {
            stack.push((col, index));
        }
        if opens_block {
            block = Some((index, col));
        }
    }
    entries
}

fn children(entries: &[Entry]) -> Vec<Vec<usize>> {
    let mut kids = vec![Vec::new(); entries.len()];
    for (i, e) in entries.iter().enumerate() {
        if let Some(p) = e.parent {
            kids[p].push(i);
        }
    }
    kids
}

fn norm(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect::<String>()
        .to_ascii_lowercase()
}

const ENV_PARENTS: &[&str] = &[
    "env",
    "environment",
    "extraenv",
    "envvars",
    "envvariables",
    "extraenvvars",
    "variables",
];

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn scalar_items<'a>(entries: &'a [Entry], kids: &[usize]) -> Vec<(&'a str, usize)> {
    kids.iter()
        .filter(|k| entries[**k].plain)
        .map(|k| (entries[*k].value.as_str(), entries[*k].line))
        .collect()
}

/// Environment records the YAML entries hold, each with the entry it came
/// from, so a compose pass can attach the service's anchor.
fn yaml_env(entries: &[Entry], kids: &[Vec<usize>]) -> Vec<(RawRecord, usize)> {
    let mut out = Vec::new();
    let config_kind = entries.iter().any(|e| {
        e.parent.is_none() && e.key == "kind" && matches!(unquote(&e.value), "ConfigMap" | "Secret")
    });
    for (p, parent) in entries.iter().enumerate() {
        if parent.plain {
            continue;
        }
        let key = norm(&parent.key);
        let grand = parent.parent.map(|g| norm(&entries[g].key));
        let env_like = ENV_PARENTS.contains(&key.as_str())
            || (key == "args" && grand.as_deref() == Some("build"));
        let data_like = config_kind
            && parent.parent.is_none()
            && matches!(parent.key.as_str(), "data" | "stringData");
        if !env_like && !data_like {
            continue;
        }
        let mechanism = if data_like { "k8s-data" } else { "yaml-env" };
        let list_mode = kids[p].iter().any(|k| entries[*k].item);
        for &k in &kids[p] {
            let e = &entries[k];
            let name = if e.plain {
                Some(e.value.split('=').next().unwrap_or(&e.value).trim())
            } else if list_mode {
                (e.key == "name").then(|| unquote(&e.value))
            } else {
                Some(e.key.as_str())
            };
            let Some(name) = name.filter(|n| valid_name(n)) else {
                continue;
            };
            if data_like
                && !name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                continue;
            }
            out.extend(env_record(name, Role::Set, mechanism, e.line).map(|r| (r, k)));
        }
    }
    out
}

fn image_ref(value: &str) -> Option<&str> {
    let v = unquote(value);
    (!v.is_empty() && !v.contains("{{") && !v.contains(['{', '}', ' '])).then_some(v)
}

/// Images spelled by `image:` entries (manifests, Helm values, CI services),
/// `container:` scalars, kustomize `images:` and bare `services:` items.
fn yaml_image_consumers(
    entries: &[Entry],
    kids: &[Vec<usize>],
    skip_image_key: bool,
    out: &mut Vec<RawRecord>,
) {
    for (i, e) in entries.iter().enumerate() {
        if e.plain {
            if let Some(p) = e.parent {
                if norm(&entries[p].key) == "services" {
                    if let Some(r) = image_ref(&e.value) {
                        image_records(r, Role::Consume, "ci-services", e.line, None, None, out);
                    }
                }
            }
            continue;
        }
        match e.key.as_str() {
            "image" if !skip_image_key => {
                if e.value.is_empty() {
                    let get = |name: &str| {
                        kids[i]
                            .iter()
                            .find(|k| entries[**k].key == name)
                            .map(|k| unquote(&entries[*k].value))
                            .filter(|v| !v.is_empty() && !v.contains("{{"))
                    };
                    let repository = get("repository").or_else(|| get("name"));
                    if let Some(repo) = repository {
                        let full = match get("registry") {
                            Some(reg) if !repo.starts_with(reg) => format!("{reg}/{repo}"),
                            _ => repo.to_string(),
                        };
                        let from = out.len();
                        image_records(
                            &full,
                            Role::Consume,
                            "values-image",
                            e.line,
                            None,
                            None,
                            out,
                        );
                        let last = kids[i].iter().map(|k| entries[*k].line).max();
                        widen(out, from, last.unwrap_or(e.line));
                    }
                } else if let Some(r) = image_ref(&e.value) {
                    image_records(r, Role::Consume, "yaml-image", e.line, None, None, out);
                }
            }
            "container" => {
                if let Some(r) = image_ref(&e.value) {
                    image_records(r, Role::Consume, "ci-container", e.line, None, None, out);
                }
            }
            "images" => {
                let mut groups: Vec<Vec<usize>> = Vec::new();
                for &k in &kids[i] {
                    if entries[k].item || groups.is_empty() {
                        groups.push(Vec::new());
                    }
                    if let Some(last) = groups.last_mut() {
                        last.push(k);
                    }
                }
                for group in groups {
                    let get = |name: &str| {
                        group
                            .iter()
                            .find(|k| entries[**k].key == name)
                            .map(|k| (unquote(&entries[*k].value), entries[*k].line))
                    };
                    if let Some((r, line)) = get("newName").or_else(|| get("name")) {
                        let from = out.len();
                        image_records(r, Role::Consume, "kustomize-image", line, None, None, out);
                        let first = group.iter().map(|k| entries[*k].line).min().unwrap_or(line);
                        let last = group.iter().map(|k| entries[*k].line).max().unwrap_or(line);
                        for record in &mut out[from..] {
                            record.line = first;
                        }
                        widen(out, from, last);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `docker/build-push-action`'s `with:` block: `tags`, `context`, `file`.
fn build_push_action(entries: &[Entry], kids: &[Vec<usize>], out: &mut Vec<RawRecord>) {
    for (i, e) in entries.iter().enumerate() {
        if e.plain || e.key != "tags" {
            continue;
        }
        let Some(p) = e.parent else { continue };
        if entries[p].key != "with" {
            continue;
        }
        let sibling = |name: &str| {
            kids[p]
                .iter()
                .find(|k| entries[**k].key == name)
                .map(|k| unquote(&entries[*k].value))
        };
        let context = sibling("context")
            .and_then(|c| join_dir("", c))
            .or_else(|| Some(String::new()));
        let artifact = match sibling("file") {
            Some(f) => join_dir("", f),
            None => context.as_deref().map(|d| dockerfile_in(d, "Dockerfile")),
        };
        let mut tags: Vec<(String, usize)> = e
            .value
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty() && !t.starts_with(['|', '>']))
            .map(|t| (t.to_string(), e.line))
            .collect();
        for (v, line) in scalar_items(entries, &kids[i]) {
            tags.extend(v.split(',').map(|t| (t.trim().to_string(), line)));
        }
        for (tag, line) in tags {
            image_records(
                &tag,
                Role::Build,
                "build-push-action",
                line,
                context.as_deref(),
                artifact.as_deref(),
                out,
            );
        }
    }
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Compose services: `image:` beside `build:` declares the build, `image:`
/// alone consumes; a service's variables speak for its build context or
/// `env_file` directory.
fn compose(
    rel: &str,
    entries: &[Entry],
    kids: &[Vec<usize>],
    env: &mut [(RawRecord, usize)],
    out: &mut Vec<RawRecord>,
) {
    let base = dir_of(rel);
    let Some(services) = entries
        .iter()
        .position(|e| e.parent.is_none() && e.key == "services")
    else {
        return;
    };
    for &s in &kids[services] {
        let find = |name: &str| kids[s].iter().copied().find(|k| entries[*k].key == name);
        let build = find("build");
        let context = build.and_then(|b| {
            if entries[b].value.is_empty() {
                let ctx = kids[b].iter().find(|k| entries[**k].key == "context")?;
                join_dir(base, unquote(&entries[*ctx].value))
            } else {
                join_dir(base, unquote(&entries[b].value))
            }
        });
        let dockerfile = build.and_then(|b| {
            kids[b]
                .iter()
                .find(|k| entries[**k].key == "dockerfile")
                .map(|k| unquote(&entries[*k].value).to_string())
        });
        let env_file_dir = find("env_file").and_then(|f| {
            let first = if entries[f].value.is_empty() {
                scalar_items(entries, &kids[f])
                    .first()
                    .map(|(v, _)| unquote(v).to_string())
            } else {
                Some(unquote(&entries[f].value).to_string())
            }?;
            join_dir(base, &first).map(|p| dir_of(&p).to_string())
        });
        let anchor = context.clone().or(env_file_dir);

        if let Some(image) = find("image") {
            let artifact = context
                .as_deref()
                .map(|c| dockerfile_in(c, dockerfile.as_deref().unwrap_or("Dockerfile")));
            let role = if build.is_some() {
                Role::Build
            } else {
                Role::Consume
            };
            let artifact = if build.is_some() {
                artifact.as_deref()
            } else {
                None
            };
            if let Some(r) = image_ref(&entries[image].value) {
                image_records(
                    r,
                    role,
                    "compose-image",
                    entries[image].line,
                    anchor.as_deref(),
                    artifact,
                    out,
                );
            }
        }
        for (record, entry) in env.iter_mut() {
            let mut at = *entry;
            while let Some(p) = entries[at].parent {
                if p == s {
                    record.anchor.clone_from(&anchor);
                    break;
                }
                at = p;
            }
        }
    }
}

static INTERPOLATION: Lazy<Regex> = Lazy::new(|| pattern(r"\$\{([A-Za-z_][A-Za-z0-9_]*)"));

fn yaml(rel: &str, text: &str, lines: &Lines, compose_file: bool, out: &mut Vec<RawRecord>) {
    let entries = parse_yaml(text);
    let kids = children(&entries);
    let mut env = yaml_env(&entries, &kids);
    yaml_image_consumers(&entries, &kids, compose_file, out);
    build_push_action(&entries, &kids, out);
    yaml_routes(&entries, &kids, out);
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    if name.starts_with("application") || name.starts_with("bootstrap") {
        spring_yaml(&entries, out);
    }
    docker_commands(text, out);
    if compose_file {
        compose(rel, &entries, &kids, &mut env, out);
        for cap in INTERPOLATION.captures_iter(text) {
            if let Some(m) = cap.get(1) {
                out.extend(env_record(
                    m.as_str(),
                    Role::Read,
                    "compose-interpolation",
                    lines.of(m.start()),
                ));
            }
        }
    }
    out.extend(env.into_iter().map(|(r, _)| r));
}

// ---------------------------------------------------------------- Terraform

static TF_IMAGE: Lazy<Regex> = Lazy::new(|| pattern(r#"(?m)^\s*"?image"?\s*[=:]\s*"([^"\n]+)""#));
static TF_ENV_BLOCK: Lazy<Regex> =
    Lazy::new(|| pattern(r"\b(?:env|environment|env_vars?)\b\s*[=\{\[]"));
static TF_NAME: Lazy<Regex> = Lazy::new(|| pattern(r#""?name"?\s*[=:]\s*"([^"\n]+)""#));
static TF_MAP_KEY: Lazy<Regex> = Lazy::new(|| pattern(r#"(?m)^\s*"?([A-Z][A-Z0-9_]*)"?\s*[=:]"#));

fn terraform(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    for cap in TF_IMAGE.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            image_records(
                m.as_str(),
                Role::Consume,
                "terraform-image",
                lines.of(m.start()),
                None,
                None,
                out,
            );
        }
    }
    for opener in TF_ENV_BLOCK.find_iter(text) {
        let start = opener.end() - 1;
        let mut depth = 0i32;
        let mut end = text.len().min(start + 20_000);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        for (i, c) in text[start..end].char_indices() {
            match c {
                '{' | '[' => depth += 1,
                '}' | ']' => {
                    depth -= 1;
                    if depth <= 0 {
                        end = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        let region = &text[start..end];
        for cap in TF_NAME
            .captures_iter(region)
            .chain(TF_MAP_KEY.captures_iter(region))
        {
            if let Some(m) = cap.get(1) {
                if valid_name(m.as_str()) {
                    out.extend(env_record(
                        m.as_str(),
                        Role::Set,
                        "terraform-env",
                        lines.of(start + m.start()),
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------- routes

/// The key of an HTTP path: lowercase, query and fragment cut, trailing slash
/// gone, every parameter (`{id}`, `:id`, `<int:id>`, `${id}`, `[id]`, a bare
/// number) one `{}`. A base-URL variable in front (`${API}/users`) stands for the host and
/// is dropped. `None` for a string that is not a path or names no static
/// segment.
pub(crate) fn route_key(raw: &str) -> Option<String> {
    route_shape(raw).filter(|k| has_static_segment(k))
}

fn has_static_segment(key: &str) -> bool {
    key.split('/').any(|p| p != "{}" && p.len() >= 2)
}

/// A path's key before it is known to name anything: a router's own `/{id}`
/// names a route only once its mount prefix is in front of it.
pub(crate) fn route_shape(raw: &str) -> Option<String> {
    let mut s = raw
        .trim()
        .trim_matches(|c| c == '"' || c == '\'' || c == '`');
    if let Some(i) = s.find("://") {
        let after = &s[i + 3..];
        s = &after[after.find('/')?..];
    }
    let s = s.split(['?', '#']).next()?.to_ascii_lowercase();
    let leading_variable = !s.starts_with('/');
    let mut segments: Vec<String> = Vec::new();
    for segment in s.split('/').filter(|p| !p.is_empty()) {
        let parameter = (segment.starts_with('{') && segment.ends_with('}'))
            || segment.starts_with(':')
            || (segment.starts_with('<') && segment.ends_with('>'))
            || (segment.starts_with('[') && segment.ends_with(']'))
            || segment.contains("${")
            || segment == "*"
            // A literal id in a test client's path.
            || segment.chars().all(|c| c.is_ascii_digit());
        segments.push(if parameter {
            "{}".to_string()
        } else {
            segment.to_string()
        });
    }
    if leading_variable {
        if segments.first().map(String::as_str) != Some("{}") {
            return None;
        }
        segments.remove(0);
    }
    Some(format!("/{}", segments.join("/")))
}

fn route_record(raw: &str, role: Role, mechanism: &'static str, line: usize) -> Option<RawRecord> {
    Some(RawRecord {
        kind: Kind::Route,
        key: route_key(raw)?,
        strength: Strength::Strong,
        role,
        mechanism,
        line,
        end: line,
        anchor: None,
        artifact: None,
        router: None,
        mount: None,
    })
}

static NGINX_LOCATION: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^\s*location\s+([=~*^]+\s+)?(/[^\s{;]*)"));
static JSON_PATH_KEY: Lazy<Regex> = Lazy::new(|| pattern(r#"(?m)^\s*"(/[^"\n]+)"\s*:\s*\{"#));

/// A path a proxy or an Ingress routes by prefix names every route behind it:
/// the exact path as a strong key, and the same path as a weak one, which
/// meets the leading parts of the routes the code serves.
fn route_prefix_records(raw: &str, mechanism: &'static str, line: usize, out: &mut Vec<RawRecord>) {
    let Some(strong) = route_record(raw, Role::Read, mechanism, line) else {
        return;
    };
    out.push(RawRecord {
        strength: Strength::Weak,
        ..strong.clone()
    });
    out.push(strong);
}

fn nginx(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    for cap in NGINX_LOCATION.captures_iter(text) {
        let Some(m) = cap.get(2) else {
            continue;
        };
        let modifier = cap.get(1).map_or("", |m| m.as_str().trim());
        let line = lines.of(m.start());
        match modifier {
            // A regular expression names no path.
            "~" | "~*" => {}
            "=" => out.extend(route_record(m.as_str(), Role::Read, "nginx-location", line)),
            _ => route_prefix_records(m.as_str(), "nginx-location", line, out),
        }
    }
}

fn openapi_json(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    if !text.contains("\"openapi\"") && !text.contains("\"swagger\"") {
        return;
    }
    for cap in JSON_PATH_KEY.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            out.extend(route_record(
                m.as_str(),
                Role::Read,
                "openapi-path",
                lines.of(m.start()),
            ));
        }
    }
}

/// Ingress `path:` values and the keys of an OpenAPI document's `paths:`.
fn yaml_routes(entries: &[Entry], kids: &[Vec<usize>], out: &mut Vec<RawRecord>) {
    let is_openapi = entries
        .iter()
        .any(|e| e.parent.is_none() && matches!(e.key.as_str(), "openapi" | "swagger"));
    for (i, e) in entries.iter().enumerate() {
        if e.plain {
            continue;
        }
        if e.key == "path" && unquote(&e.value).starts_with('/') {
            let exact = e.parent.is_some_and(|p| {
                kids[p].iter().any(|&k| {
                    norm(&entries[k].key) == "pathtype" && unquote(&entries[k].value) == "Exact"
                })
            });
            if exact {
                out.extend(route_record(
                    unquote(&e.value),
                    Role::Read,
                    "ingress-path",
                    e.line,
                ));
            } else {
                route_prefix_records(unquote(&e.value), "ingress-path", e.line, out);
            }
        }
        if is_openapi && e.parent.is_none() && e.key == "paths" {
            for &k in &kids[i] {
                if entries[k].key.starts_with('/') {
                    out.extend(route_record(
                        &entries[k].key,
                        Role::Read,
                        "openapi-path",
                        entries[k].line,
                    ));
                }
            }
        }
    }
}

// ---------------------------------------------------------------- tables

/// Words the SQL patterns can capture that are no table.
const NOT_A_TABLE: &[&str] = &[
    "select",
    "from",
    "where",
    "set",
    "values",
    "table",
    "dual",
    "information_schema",
    "pg_catalog",
    "public",
    "the",
    "this",
    "that",
    "into",
    "update",
    "delete",
    "insert",
    "join",
    "null",
    "true",
    "false",
    "with",
    "only",
    "exists",
    "not",
    "temp",
    "temporary",
    "unlogged",
    "index",
    "view",
    "schema",
    "lateral",
    "unnest",
    "generate_series",
    "json_each",
    "each",
];

/// Schemas a database searches without being told: a table behind one is
/// the bare name.
const DEFAULT_SCHEMAS: &[&str] = &["public", "dbo", "main"];
/// Schemas the database keeps for itself.
const SYSTEM_SCHEMAS: &[&str] = &[
    "information_schema",
    "pg_catalog",
    "sys",
    "mysql",
    "performance_schema",
];

fn sql_word(w: &str) -> bool {
    w.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && w.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A table's key: its name behind its schema when it has one, so `stems.assets`
/// is not `library.assets`. `file_schema` is the one a file states for the
/// names it spells bare.
fn table_record(
    raw: &str,
    role: Role,
    mechanism: &'static str,
    line: usize,
    file_schema: Option<&str>,
) -> Option<RawRecord> {
    let cleaned = raw
        .chars()
        .filter(|c| !matches!(c, '"' | '`' | '\'' | '[' | ']'))
        .collect::<String>()
        .to_ascii_lowercase();
    let (schema, name) = match cleaned.split_once('.') {
        Some((schema, name)) => (Some(schema), name),
        None => (file_schema, cleaned.as_str()),
    };
    if name.len() < 3 || !sql_word(name) || NOT_A_TABLE.contains(&name) {
        return None;
    }
    let key = match schema {
        Some(s) if SYSTEM_SCHEMAS.contains(&s) || !sql_word(s) => return None,
        Some(s) if !DEFAULT_SCHEMAS.contains(&s) => format!("{s}.{name}"),
        _ => name.to_string(),
    };
    Some(RawRecord {
        kind: Kind::Table,
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
    })
}

const TABLE_PATH: &str = r#"([`"\[]?\w+[`"\]]?\.[`"\[]?\w+[`"\]]?|[`"\[]?\w+)"#;

static SQL_TABLE_DEF: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r"(?i)\b(?:create\s+(?:or\s+replace\s+)?(?:temp(?:orary)?\s+|unlogged\s+)?table|alter\s+table|drop\s+table|truncate\s+table|comment\s+on\s+table)\s+(?:if\s+(?:not\s+)?exists\s+)?(?:only\s+)?{TABLE_PATH}"
    ))
});
static ORM_TABLE_DEF: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r#"\b__tablename__\s*=\s*['"](\w+)['"]|\bdb_table\s*=\s*['"](\w+)['"]|\bTable\(\s*['"](\w+)['"]|\bop\.(?:create_table|drop_table|add_column|drop_column|alter_column|create_index|rename_table)\(\s*['"](\w+)['"]|@Table\(\s*name\s*=\s*"(\w+)"|@@map\(\s*"(\w+)"\s*\)|@Entity\(\s*['"](\w+)['"]|\bcreateTable\(\s*['"](\w+)['"]|\bcreate_table\s+:?['"]?(\w+)|TableName\(\)\s*string\s*\{\s*return\s*"(\w+)""#,
    )
});
static SQL_TABLE_USE: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"(?i)\bselect\b[^;]{{0,2000}}?\bfrom\s+{TABLE_PATH}|\binsert\s+into\s+{TABLE_PATH}|\bdelete\s+from\s+{TABLE_PATH}|\bupdate\s+{TABLE_PATH}[`"\]]?\s+set\b|\bjoin\s+{TABLE_PATH}"#
    ))
});

/// The one schema a file states for its models (`schema="stems"`,
/// `{"schema": "stems"}`, `@@schema("stems")`); several or none give no schema.
static SCHEMA_LITERAL: Lazy<Regex> = Lazy::new(|| {
    pattern(r#"(?:['"]schema['"]\s*:|\bschema\s*[:=]|@@schema\()\s*['"]([A-Za-z_]\w*)['"]"#)
});

fn file_schema(text: &str) -> Option<String> {
    let found: FxHashSet<String> = SCHEMA_LITERAL
        .captures_iter(text)
        .filter_map(|c| c.get(1))
        .map(|m| m.as_str().to_ascii_lowercase())
        .collect();
    match found.len() {
        1 => found.into_iter().next(),
        _ => None,
    }
}

fn tables(class: FileClass, text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let defines: &[(&Regex, &'static str)] = match class {
        FileClass::Sql => &[(&SQL_TABLE_DEF, "sql-ddl"), (&ORM_TABLE_DEF, "orm-table")],
        FileClass::Code(CodeLang::Shell) => &[],
        FileClass::Code(_) => &[(&SQL_TABLE_DEF, "sql-ddl"), (&ORM_TABLE_DEF, "orm-table")],
        _ => &[],
    };
    let schema = (!defines.is_empty()).then(|| file_schema(text)).flatten();
    for (re, mechanism) in defines {
        let bare_in = if *mechanism == "orm-table" {
            schema.as_deref()
        } else {
            None
        };
        for cap in re.captures_iter(text) {
            if let Some((name, at)) = first_group(&cap) {
                out.extend(table_record(
                    name,
                    Role::Set,
                    mechanism,
                    lines.of(at),
                    bare_in,
                ));
            }
        }
    }
    if matches!(class, FileClass::Sql | FileClass::Code(_))
        && !matches!(class, FileClass::Code(CodeLang::Shell))
    {
        for cap in SQL_TABLE_USE.captures_iter(text) {
            if let Some((name, at)) = first_group(&cap) {
                out.extend(table_record(
                    name,
                    Role::Read,
                    "sql-query",
                    lines.of(at),
                    None,
                ));
            }
        }
    }
}

// ---------------------------------------------------------------- protobuf

static PROTO_PACKAGE: Lazy<Regex> = Lazy::new(|| pattern(r"(?m)^\s*package\s+([\w.]+)\s*;"));
static PROTO_DECL: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^\s*(?:message|service|enum)\s+([A-Za-z_]\w*)"));
static PY_PB: Lazy<Regex> = Lazy::new(|| pattern(r"\b(\w+?)_pb2(?:_grpc)?\.(\w+)"));
static GO_PB: Lazy<Regex> = Lazy::new(|| {
    pattern(
        r"\bpb\.([A-Z]\w*)|\bRegister([A-Z]\w*)Server\(|\bNew([A-Z]\w*)Client\(|\bUnimplemented([A-Z]\w*)Server\b",
    )
});
static JAVA_PB: Lazy<Regex> = Lazy::new(|| pattern(r"\b([A-Z]\w*)Grpc\b"));

fn proto_record(
    key: String,
    strength: Strength,
    role: Role,
    mechanism: &'static str,
    line: usize,
) -> RawRecord {
    RawRecord {
        kind: Kind::Proto,
        key,
        strength,
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

/// `Order` of `order_pb2.Order`, `OrdersServicer` of the generated gRPC
/// class, `add_OrdersServicer_to_server`: the name the .proto declared.
fn generated_name(raw: &str) -> String {
    for (prefix, suffix) in [
        ("Register", "Server"),
        ("New", "Client"),
        ("Unimplemented", "Server"),
    ] {
        if let Some(name) = raw
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(suffix))
            .filter(|n| !n.is_empty())
        {
            return name.to_ascii_lowercase();
        }
    }
    let raw = raw.strip_prefix("add_").unwrap_or(raw);
    let raw = raw.strip_suffix("_to_server").unwrap_or(raw);
    for suffix in ["Servicer", "Stub", "ServiceClient", "Client", "Server"] {
        if let Some(base) = raw.strip_suffix(suffix) {
            if !base.is_empty() {
                return base.to_ascii_lowercase();
            }
        }
    }
    raw.to_ascii_lowercase()
}

fn proto_defs(rel: &str, text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let package = PROTO_PACKAGE
        .captures(text)
        .map(|c| c[1].to_ascii_lowercase());
    let stem = rel
        .rsplit('/')
        .next()
        .and_then(|n| n.strip_suffix(".proto"))
        .unwrap_or("")
        .to_ascii_lowercase();
    for cap in PROTO_DECL.captures_iter(text) {
        let Some(m) = cap.get(1) else { continue };
        let name = m.as_str().to_ascii_lowercase();
        let line = lines.of(m.start());
        let qualified = match &package {
            Some(p) => format!("{p}.{name}"),
            None => name.clone(),
        };
        out.push(proto_record(
            qualified,
            Strength::Strong,
            Role::Set,
            "proto-decl",
            line,
        ));
        out.push(proto_record(
            format!("@{stem}.{name}"),
            Strength::Strong,
            Role::Set,
            "proto-decl",
            line,
        ));
        out.push(proto_record(
            name,
            Strength::Weak,
            Role::Set,
            "proto-decl",
            line,
        ));
    }
}

fn proto_uses(lang: CodeLang, text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    match lang {
        // The module a generated name comes from is the .proto file's stem:
        // `orders_pb2.Order` names Order of orders.proto and no other.
        CodeLang::Python => {
            for cap in PY_PB.captures_iter(text) {
                let (Some(module), Some(name)) = (cap.get(1), cap.get(2)) else {
                    continue;
                };
                let key = format!(
                    "@{}.{}",
                    module.as_str().to_ascii_lowercase(),
                    generated_name(name.as_str())
                );
                out.push(proto_record(
                    key,
                    Strength::Strong,
                    Role::Read,
                    "pb2-use",
                    lines.of(name.start()),
                ));
            }
        }
        CodeLang::Go => {
            for cap in GO_PB.captures_iter(text) {
                if let Some((name, at)) = first_group(&cap) {
                    out.push(proto_record(
                        generated_name(name),
                        Strength::Weak,
                        Role::Read,
                        "pb-use",
                        lines.of(at),
                    ));
                }
            }
        }
        CodeLang::Jvm => {
            for cap in JAVA_PB.captures_iter(text) {
                if let Some(m) = cap.get(1) {
                    out.push(proto_record(
                        m.as_str().to_ascii_lowercase(),
                        Strength::Weak,
                        Role::Read,
                        "grpc-use",
                        lines.of(m.start()),
                    ));
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------- technologies
// The rules `tech.rs` documents, applied.

static PROPERTY_LINE: Lazy<Regex> =
    Lazy::new(|| pattern(r"(?m)^[ \t]*([A-Za-z][\w.\-]*)[ \t]*[=:][ \t]*(.*)$"));
static PLACEHOLDER: Lazy<Regex> = Lazy::new(|| pattern(r"\$\{([A-Za-z_][\w.\-]*)"));
static SPRING_VALUE: Lazy<Regex> = Lazy::new(|| pattern(r#"@Value\(\s*"\$\{([A-Za-z_][\w.\-]*)"#));
static DJANGO_ENV: Lazy<Regex> = Lazy::new(|| {
    pattern(&format!(
        r#"\benv(?:\.(?:str|int|bool|float|list|dict|json|url|path|db|db_url|cache|cache_url|email|email_url|search_url))?\(\s*['"]{NAME}['"]|\bconfig\(\s*['"]{NAME}['"]"#
    ))
});
static ENVSUBST: Lazy<Regex> =
    Lazy::new(|| pattern(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Z_][A-Z0-9_]*)"));

/// `application.properties` and the Kafka broker files: each setting is a
/// definition under every key it answers to, each `${placeholder}` in a value
/// a read.
fn properties(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    for cap in PROPERTY_LINE.captures_iter(text) {
        let (Some(key), Some(value)) = (cap.get(1), cap.get(2)) else {
            continue;
        };
        let line = lines.of(key.start());
        out.extend(env_records(
            property_keys(key.as_str()),
            Role::Set,
            "properties-key",
            line,
        ));
        placeholders(value.as_str(), line, out);
    }
}

fn placeholders(value: &str, line: usize, out: &mut Vec<RawRecord>) {
    for cap in PLACEHOLDER.captures_iter(value) {
        if let Some(m) = cap.get(1) {
            out.extend(env_records(
                property_keys(m.as_str()),
                Role::Read,
                "placeholder",
                line,
            ));
        }
    }
}

/// `spring:` / `datasource:` / `url:` as `spring.datasource.url`: the
/// settings and placeholders of a Spring application file.
fn spring_yaml(entries: &[Entry], out: &mut Vec<RawRecord>) {
    for (i, e) in entries.iter().enumerate() {
        if e.plain {
            continue;
        }
        let mut path = vec![e.key.as_str()];
        let mut at = e.parent;
        while let Some(p) = at {
            if entries[p].plain {
                break;
            }
            path.push(entries[p].key.as_str());
            at = entries[p].parent;
        }
        path.reverse();
        let value = unquote(&e.value);
        let is_leaf = !value.is_empty() && !value.starts_with(['|', '>']);
        let _ = i;
        if is_leaf {
            out.extend(env_records(
                property_keys(&path.join(".")),
                Role::Set,
                "spring-yaml-key",
                e.line,
            ));
            placeholders(value, e.line, out);
        }
    }
}

fn spring_value_reads(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    for cap in SPRING_VALUE.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            out.extend(env_records(
                property_keys(m.as_str()),
                Role::Read,
                "spring-value",
                lines.of(m.start()),
            ));
        }
    }
}

fn django_reads(rel: &str, text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    let lower = rel.to_ascii_lowercase();
    if !(lower.contains("settings") || lower.contains("config")) {
        return;
    }
    for cap in DJANGO_ENV.captures_iter(text) {
        if let Some((name, at)) = first_group(&cap) {
            out.extend(env_record(name, Role::Read, "django-environ", lines.of(at)));
        }
    }
}

fn envsubst(text: &str, lines: &Lines, out: &mut Vec<RawRecord>) {
    for cap in ENVSUBST.captures_iter(text) {
        if let Some((name, at)) = first_group(&cap) {
            out.extend(env_record(name, Role::Read, "envsubst", lines.of(at)));
        }
    }
}

/// A Kafka image variable also answers to the broker property it sets.
fn kafka_aliases(out: &mut Vec<RawRecord>) {
    let aliases: Vec<RawRecord> = out
        .iter()
        .filter(|r| r.kind == Kind::Env && r.role == Role::Set)
        .filter_map(|r| {
            let key = kafka_property(&r.key)?;
            Some(RawRecord {
                key,
                mechanism: "kafka-env",
                ..r.clone()
            })
        })
        .collect();
    out.extend(aliases);
}

/// Every record the file's text holds.
pub(crate) fn extract(rel: &str, class: FileClass, text: &str) -> Vec<RawRecord> {
    let lines = Lines::new(text);
    let mut out = Vec::new();
    match class {
        FileClass::Code(lang) => {
            code_reads(lang, text, &lines, &mut out);
            super::routes::code_routes(lang, text, &lines, &mut out);
            tables(class, text, &lines, &mut out);
            proto_uses(lang, text, &lines, &mut out);
            match lang {
                CodeLang::Jvm => spring_value_reads(text, &lines, &mut out),
                CodeLang::Python => django_reads(rel, text, &lines, &mut out),
                _ => {}
            }
            if lang == CodeLang::Shell {
                docker_commands(text, &mut out);
            }
        }
        FileClass::Dotenv => dotenv(text, &lines, &mut out),
        FileClass::Dockerfile => dockerfile(text, &mut out),
        FileClass::Yaml { compose } => yaml(rel, text, &lines, compose, &mut out),
        FileClass::Terraform => terraform(text, &lines, &mut out),
        FileClass::Conf => {
            nginx(text, &lines, &mut out);
            envsubst(text, &lines, &mut out);
        }
        FileClass::Sql => tables(class, text, &lines, &mut out),
        FileClass::Proto => proto_defs(rel, text, &lines, &mut out),
        FileClass::Properties => properties(text, &lines, &mut out),
        FileClass::Template => envsubst(text, &lines, &mut out),
        FileClass::Json => openapi_json(text, &lines, &mut out),
        FileClass::Script => docker_commands(text, &mut out),
    }
    kafka_aliases(&mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_of_a_path_has_one_key() {
        for raw in [
            "/api/users/{id}",
            "/api/users/:id",
            "/api/users/<int:id>",
            "/api/users/${id}",
            "https://svc.internal/api/users/42?x=1"
                .replace("42", "{id}")
                .as_str(),
            "${API}/api/users/${id}",
            "/API/Users/[id]/",
        ] {
            assert_eq!(route_key(raw).as_deref(), Some("/api/users/{}"), "{raw}");
        }
        assert_eq!(route_key("users/list"), None);
        assert_eq!(route_key("/{id}"), None);
        assert_eq!(route_key("/"), None);
    }

    fn keys(rel: &str, text: &str, kind: Kind) -> Vec<(String, Role, &'static str)> {
        let class = classify(rel).expect("a known class");
        extract(rel, class, text)
            .into_iter()
            .filter(|r| r.kind == kind)
            .map(|r| (r.key, r.role, r.mechanism))
            .collect()
    }

    #[test]
    fn routes_are_defined_in_code_and_named_everywhere_else() {
        let py = "@router.get(\"/api/orders/{order_id}\")\ndef f(): ...\n";
        assert_eq!(
            keys("a/main.py", py, Kind::Route),
            vec![("/api/orders/{}".into(), Role::Set, "route-decorator")]
        );
        let ts = "const r = await fetch(`/api/orders/${id}`);\n";
        assert_eq!(
            keys("web/o.ts", ts, Kind::Route),
            vec![("/api/orders/{}".into(), Role::Read, "http-call")]
        );
        let nginx = "server {\n  location /api/orders/ {\n    proxy_pass http://api;\n  }\n}\n";
        assert_eq!(
            keys("deploy/nginx.conf", nginx, Kind::Route),
            vec![
                ("/api/orders".into(), Role::Read, "nginx-location"),
                ("/api/orders".into(), Role::Read, "nginx-location")
            ]
        );
        let openapi = "openapi: 3.0.0\npaths:\n  /api/orders/{id}:\n    get: {}\n";
        assert_eq!(
            keys("docs/api.yaml", openapi, Kind::Route),
            vec![("/api/orders/{}".into(), Role::Read, "openapi-path")]
        );
        let go = "mux.HandleFunc(\"/api/orders/{id}\", h)\n";
        assert_eq!(
            keys("cmd/s.go", go, Kind::Route),
            vec![("/api/orders/{}".into(), Role::Set, "route-registration")]
        );
    }

    #[test]
    fn tables_are_defined_by_migrations_and_models_and_queried_by_code() {
        let ddl = "ALTER TABLE IF EXISTS public.orders ADD COLUMN note text;\n";
        assert_eq!(
            keys("db/0042.sql", ddl, Kind::Table),
            vec![("orders".into(), Role::Set, "sql-ddl")]
        );
        let model = "class Order(Base):\n    __tablename__ = \"orders\"\n";
        assert_eq!(
            keys("app/models.py", model, Kind::Table),
            vec![("orders".into(), Role::Set, "orm-table")]
        );
        let query = "rows = db.execute(\"SELECT id, total FROM orders o JOIN order_items i ON i.order_id = o.id\")\n";
        let mut found: Vec<String> = keys("app/reports.py", query, Kind::Table)
            .into_iter()
            .map(|(k, role, _)| {
                assert_eq!(role, Role::Read);
                k
            })
            .collect();
        found.sort();
        assert_eq!(found, vec!["order_items".to_string(), "orders".to_string()]);
        let prisma = "model Invoice {\n  id Int\n  @@map(\"invoices\")\n}\n";
        assert_eq!(
            keys("prisma/schema.prisma", prisma, Kind::Table),
            vec![("invoices".into(), Role::Set, "orm-table")]
        );
        // A variable or column spelled like the table is no query.
        assert!(
            keys(
                "app/cache.py",
                "orders = []\norders_total = 0\n",
                Kind::Table
            )
            .is_empty()
        );
        assert!(
            keys(
                "app/x.py",
                "# select the items from the list\n",
                Kind::Table
            )
            .is_empty()
        );
    }

    #[test]
    fn a_table_keeps_the_schema_it_is_spelled_behind() {
        let names = |rel: &str, text: &str| -> Vec<String> {
            let mut v: Vec<String> = keys(rel, text, Kind::Table)
                .into_iter()
                .map(|(k, _, _)| k)
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            names(
                "db/0008.sql",
                "ALTER TABLE stems.assets ADD COLUMN c text;\n"
            ),
            vec!["stems.assets"]
        );
        assert_eq!(
            names(
                "db/0008.sql",
                "ALTER TABLE \"stems\".\"assets\" ADD c text;\n"
            ),
            vec!["stems.assets"]
        );
        assert_eq!(
            names(
                "db/0009.sql",
                "ALTER TABLE public.assets ADD COLUMN c text;\n"
            ),
            vec!["assets"]
        );
        assert!(
            names(
                "q.py",
                "db.execute(\"SELECT * FROM information_schema.tables\")\n"
            )
            .is_empty()
        );
        let model = "class A(Base):\n    __tablename__ = \"assets\"\n    __table_args__ = {\"schema\": \"stems\"}\n";
        assert_eq!(names("app/models.py", model), vec!["stems.assets"]);
        let two = "class A(Base):\n    __tablename__ = \"assets\"\n    schema = \"a\"\n    other = {\"schema\": \"b\"}\n";
        assert_eq!(names("app/models.py", two), vec!["assets"]);
        let prisma =
            "model Asset {\n  id Int @id\n  @@map(\"assets\")\n  @@schema(\"library\")\n}\n";
        assert_eq!(
            names("prisma/schema.prisma", prisma),
            vec!["library.assets"]
        );
    }

    #[test]
    fn a_prisma_map_and_a_typescript_query_name_one_table() {
        let prisma = "model Invoice {\n  id Int @id\n  @@map(\"invoices\")\n}\n";
        assert_eq!(keys("prisma/schema.prisma", prisma, Kind::Table).len(), 1);
        let ts = "export const open = (db) => db.query(\"SELECT * FROM invoices WHERE paid = false\");\n";
        assert_eq!(
            keys("src/billing.ts", ts, Kind::Table),
            vec![("invoices".into(), Role::Read, "sql-query")]
        );
    }

    #[test]
    fn generated_code_names_the_service_or_message_it_came_from() {
        let py = "import orders_pb2, orders_pb2_grpc\nx = orders_pb2.OrderItem()\norders_pb2_grpc.add_OrdersServicer_to_server(s, srv)\nc = orders_pb2_grpc.OrdersStub(ch)\n";
        let mut found: Vec<String> = keys("a/s.py", py, Kind::Proto)
            .into_iter()
            .map(|(k, _, _)| k)
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec!["@orders.orderitem", "@orders.orders", "@orders.orders"]
        );
        let go = "pb.RegisterCatalogServer(s, x)\nc := pb.NewCatalogClient(conn)\n";
        assert!(
            keys("a/main.go", go, Kind::Proto)
                .iter()
                .all(|(k, role, _)| k == "catalog" && *role == Role::Read)
        );
        let decl = "syntax = \"proto3\";\npackage shop;\nmessage Order {}\nservice Orders {}\n";
        let defs = keys("proto/shop/orders.proto", decl, Kind::Proto);
        assert!(defs.iter().any(|(k, _, _)| k == "shop.order"));
        assert!(defs.iter().any(|(k, _, _)| k == "@orders.orders"));
        assert!(defs.iter().any(|(k, _, _)| k == "order"));
    }

    fn env_keys(rel: &str, text: &str) -> Vec<(String, Role)> {
        let mut v: Vec<(String, Role)> = keys(rel, text, Kind::Env)
            .into_iter()
            .map(|(k, r, _)| (k, r))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.dedup();
        v
    }

    #[test]
    fn a_spring_property_answers_to_its_relaxed_environment_name() {
        let props = "spring.main.log-startup-info=false\nurl=${DB_URL:jdbc:h2:mem}\n# note=1\n";
        let found = env_keys("svc/src/main/resources/application.properties", props);
        for expected in [
            ("spring_main_log_startup_info", Role::Set),
            ("spring_main_logstartupinfo", Role::Set),
            ("db_url", Role::Read),
        ] {
            assert!(
                found.contains(&(expected.0.to_string(), expected.1)),
                "{expected:?} in {found:?}"
            );
        }
        let yaml = "spring:\n  datasource:\n    url: ${JDBC_URL:x}\n";
        let found = env_keys("svc/application.yml", yaml);
        assert!(found.contains(&("spring_datasource_url".to_string(), Role::Set)));
        assert!(found.contains(&("jdbc_url".to_string(), Role::Read)));
        let java = "@Value(\"${demo.item-price}\") int p;\n";
        assert!(env_keys("a/B.java", java).contains(&("demo_itemprice".to_string(), Role::Read)));
    }

    #[test]
    fn kafka_image_variables_also_name_the_broker_property() {
        let compose = "services:\n  k:\n    environment:\n      - KAFKA_ADVERTISED_LISTENERS=x\n      - KAFKA_CFG_AUTO_CREATE_TOPICS_ENABLE=true\n";
        let found = env_keys("deploy/docker-compose.yml", compose);
        for key in [
            "advertised_listeners",
            "auto_create_topics_enable",
            "kafka_advertised_listeners",
        ] {
            assert!(
                found.iter().any(|(k, r)| k == key && *r == Role::Set),
                "{key} in {found:?}"
            );
        }
        let props = "advertised.listeners=PLAINTEXT://k:9092\n";
        assert!(
            env_keys("kafka/server.properties", props)
                .contains(&("advertised_listeners".to_string(), Role::Set))
        );
        // Redis has no convention: the variable stays itself.
        let redis = "services:\n  r:\n    environment:\n      - REDIS_PASSWORD=x\n";
        assert_eq!(
            env_keys("docker-compose.yml", redis),
            vec![("redis_password".to_string(), Role::Set)]
        );
    }

    #[test]
    fn settings_helpers_and_templates_read_the_environment() {
        let settings = "SECRET_KEY = env('DJANGO_SECRET_KEY')\nDEBUG = env.bool('DJANGO_DEBUG', default=False)\n";
        let found = env_keys("proj/settings.py", settings);
        assert!(found.contains(&("django_secret_key".to_string(), Role::Read)));
        assert!(found.contains(&("django_debug".to_string(), Role::Read)));
        // Outside a settings module a function called env is no settings helper.
        assert!(env_keys("proj/views.py", settings).is_empty());
        let template = "server { listen ${NGINX_PORT}; set $x $host; proxy_pass $UPSTREAM; }\n";
        let found = env_keys("nginx/default.conf.template", template);
        assert!(found.contains(&("nginx_port".to_string(), Role::Read)));
        assert!(found.contains(&("upstream".to_string(), Role::Read)));
        assert!(!found.iter().any(|(k, _)| k == "host"));
    }
}
