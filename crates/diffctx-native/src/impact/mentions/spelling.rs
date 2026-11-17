//! How a file is written where it is used, read back to the file.
//!
//! One file has many spellings: its path from the repository root, from the
//! referrer's directory or one enclosing it (`naming`), and the ones read here —
//! behind a shell variable that holds the script's own directory
//! (`"$CI_DIR/rules-test.yaml"`), through a symlinked directory (`link/t.sh`),
//! as the directory it is the index of (`import ./lib`), as a wiki link
//! (`[[b]]`), as the route a framework serves a page file at (`/about`), as the
//! URL a `public/` directory serves a file at (`/img/a.jpg`), and as the
//! program a script is installed as (`~/.local/bin/backup`). Every
//! reader of path mentions asks `Spellings`, so a spelling one of them reads,
//! all of them read.

use std::borrow::Cow;
use std::path::Path;

use once_cell::sync::Lazy;
use regex::Regex;
use rustc_hash::FxHashMap;

use super::{Naming, PATH_TOKEN, SCRIPT_EXTENSIONS, extension, is_document, naming};

/// A file that a directory spelled alone stands for, and the extension of the
/// referrers that spell it so: Nix's `import ./lib` loads `lib/default.nix`.
/// JavaScript's `index.js` is the binding model's, which resolves the import.
const DIRECTORY_INDEX: &[(&str, &str)] = &[("default.nix", "nix")];

/// The extensions of the files a file-routed framework serves as pages.
const PAGE_EXTENSIONS: &[&str] = &[
    "astro", "md", "mdx", "vue", "svelte", "html", "js", "jsx", "ts", "tsx",
];

/// The command substitutions that print a script's own directory:
/// `$(dirname "$0")`, `$(dirname "${BASH_SOURCE[0]}")`, through `realpath` or
/// `readlink -f` too.
static SCRIPT_DIR: Lazy<Regex> = Lazy::new(|| {
    let me = r"(?:\$0|\$\{0\}|\$\{?BASH_SOURCE(?:\[0\])?\}?)";
    Regex::new(&format!(
        r#"\$\(\s*dirname\s+"?(?:{me}|\$\(\s*(?:realpath|readlink\s+-f)\s+"?{me}"?\s*\))"?\s*\)|\$\{{(?:0|BASH_SOURCE(?:\[0\])?)%/\*\}}"#
    ))
    .expect("script dir regex")
});

/// `$(cd DIR && pwd)`: the directory, absolute.
static CD_PWD: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r#"\$\(\s*cd\s+"?([^"&;|)]*)"?\s*(?:&&|;)\s*pwd(?:\s+-P)?\s*\)"#)
        .expect("cd pwd regex")
});

static VARIABLE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"\$(?:\{([A-Za-z_][A-Za-z0-9_]*)\}|([A-Za-z_][A-Za-z0-9_]*))")
        .expect("variable regex")
});

static ASSIGNMENT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^\s*(?:export\s+|local\s+|readonly\s+|declare\s+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$")
        .expect("assignment regex")
});

/// `[[b]]`, `[[notes/b|label]]`, `[[b#heading]]`: the note's path or name.
static WIKI_LINK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\[\[([^\[\]|#]+)(?:[|#][^\]]*)?\]\]").expect("wiki link regex"));

/// A playbook line that lists a role by its short name: `- web`, `- role: web`,
/// `- { role: web, tags: x }`, `name: web` of an `include_role`.
static ROLE_LINE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"^\s*(?:-\s+)?(?:\{\s*)?(?:(?:role|name):\s*)?["']?([A-Za-z0-9_][A-Za-z0-9_.-]*)["']?\s*(?:,.*|\})?\s*$"#,
    )
    .expect("role line regex")
});

/// Which reader asks: a stale reference is one a link to a removed page or
/// note also is; a reader of a changed file's content runs or loads it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Reader {
    Stale,
    Content,
}

/// The spellings a run's tree allows: its symlinks, and the directories its
/// shell scripts keep in variables.
pub(super) struct Spellings {
    root_dir: std::path::PathBuf,
    source: crate::source::Source,
    /// Each symlink of the tree and the repository path it names.
    links: Vec<(String, String)>,
    /// Per shell script, the variables that hold a directory of the
    /// repository, read once.
    script_dirs: FxHashMap<String, Vec<(String, String)>>,
}

impl Spellings {
    pub(super) fn of(root_dir: &Path, source: crate::source::Source) -> Self {
        let links = source.list().map_or_else(
            |_| Vec::new(),
            |listing| {
                listing
                    .links
                    .iter()
                    .filter_map(|abs| {
                        let link = crate::paths::display_rel(root_dir, abs)?;
                        let text = source.read_to_string(abs)?;
                        let text = text.trim();
                        if text.starts_with('/') {
                            return None;
                        }
                        let to = match link.rsplit_once('/') {
                            Some((dir, _)) => inside(&format!("{dir}/{text}")),
                            None => inside(text),
                        }?;
                        Some((link, to))
                    })
                    .collect()
            },
        );
        Self {
            root_dir: root_dir.to_path_buf(),
            source,
            links,
            script_dirs: FxHashMap::default(),
        }
    }

    /// What the grep for `targets` looks for beyond their paths and names.
    pub(super) fn needles(&self, targets: &[String], reader: Reader) -> Vec<String> {
        let mut needles = Vec::new();
        for target in targets {
            for alias in self.through_links(target) {
                needles.push(super::basename_of(&alias));
                needles.push(alias);
            }
            if let Some((dir, _)) = index_of(target) {
                needles.push(format!("/{}", super::basename_of(dir)));
            }
            if is_runnable(target) && super::basename_of(target).contains('.') {
                needles.push(format!("bin/{}", stem(target)));
            }
            if reader == Reader::Stale {
                needles.extend(role_of(target).map(str::to_string));
                if is_document(target) {
                    needles.push(format!("[[{}", stem(target)));
                }
                needles.extend(route_of(target));
            }
        }
        needles
    }

    /// `text`, a line of `referrer`, with the directories its variables hold
    /// written out: `"$CI_DIR/rules-test.yaml"` in `chart/ci/rules-test.sh`
    /// reads `"chart/ci/rules-test.yaml"`.
    pub(super) fn line<'t>(&mut self, referrer: &str, text: &'t str) -> Cow<'t, str> {
        if !text.contains('$') || !is_shell(referrer) {
            return Cow::Borrowed(text);
        }
        let (root_dir, source) = (&self.root_dir, &self.source);
        let dirs = self
            .script_dirs
            .entry(referrer.to_string())
            .or_insert_with(|| match source.read_to_string(&root_dir.join(referrer)) {
                Some(content) if is_shell_script(referrer, &content) => {
                    script_dirs(referrer, &content)
                }
                // The line being read came from this file: one that is no
                // shell script holds no directory, and one that cannot be
                // read again names none.
                _ => Vec::new(),
            });
        let written = with_dirs(text, script_dir(referrer), dirs);
        if written == text {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(written)
        }
    }

    /// The path spellings a line holds: each run of path characters, a full
    /// stop that ends a sentence dropped, and the note each wiki link of a
    /// document names.
    pub(super) fn tokens(referrer: &str, line: &str) -> Vec<String> {
        let mut tokens: Vec<String> = PATH_TOKEN
            .find_iter(line)
            .map(|m| m.as_str().trim_end_matches('.').to_string())
            .collect();
        if matches!(extension(referrer).as_str(), "yml" | "yaml") {
            tokens.extend(ROLE_LINE.captures(line).map(|c| format!("roles/{}", &c[1])));
        }
        if is_document(referrer) {
            tokens.extend(WIKI_LINK.captures_iter(line).map(|c| {
                let name = c[1].trim();
                if super::basename_of(name).contains('.') {
                    name.to_string()
                } else {
                    format!("{name}.md")
                }
            }));
        }
        tokens
    }

    /// How `token`, found in `referrer`, names `target` by any of its
    /// spellings: the strongest one. A reader of the content spells the file
    /// whole: `./scripts/run` is the module, not the script `run.py`.
    pub(super) fn naming(
        &self,
        token: &str,
        referrer: &str,
        target: &str,
        unique_basename: bool,
        root: &str,
        reader: Reader,
    ) -> Option<Naming> {
        let spelled = |path: &str, unique: bool| {
            (reader == Reader::Stale || token.ends_with(super::basename_of(path).as_str()))
                .then(|| naming(token, referrer, path, unique, root))
                .flatten()
        };
        let direct = spelled(target, unique_basename);
        let linked = self
            .through_links(target)
            .filter_map(|alias| spelled(&alias, unique_basename))
            .min();
        // A directory spelled alone is a path literal (`./lib`), never a word.
        let indexed = index_of(target)
            .filter(|(_, ext)| token.contains('/') && extension(referrer) == *ext)
            .and_then(|(dir, _)| spelled(dir, false))
            .filter(|n| matches!(n, Naming::FullPath | Naming::RelativePath));
        let installed =
            (unique_basename && runs_installed(token, target)).then_some(Naming::Installed);
        let routed = (reader == Reader::Stale
            && route_of(target).is_some_and(|route| token.trim_end_matches('/') == route))
        .then_some(Naming::FullPath);
        let served = public_url(target)
            .is_some_and(|(project, url)| token == url && referrer.starts_with(&project))
            .then_some(Naming::FullPath);
        [direct, linked, indexed, installed, routed, served]
            .into_iter()
            .flatten()
            .min()
    }

    /// `target` spelled through each symlink of the tree that names it or a
    /// directory holding it.
    fn through_links<'a>(&'a self, target: &'a str) -> impl Iterator<Item = String> + 'a {
        self.links.iter().filter_map(move |(link, to)| {
            if target == to.as_str() {
                Some(link.clone())
            } else {
                target
                    .strip_prefix(to.as_str())
                    .and_then(|rest| rest.strip_prefix('/'))
                    .map(|rest| format!("{link}/{rest}"))
            }
        })
    }
}

/// The role an Ansible role directory is: a playbook lists it by that name, not by
/// the path `roles/<name>`.
fn role_of(path: &str) -> Option<&str> {
    let (parent, name) = path.rsplit_once('/')?;
    (parent == "roles" || parent.ends_with("/roles")).then_some(name)
}

/// A relative `path` with `.` and `..` resolved, or `None` when it leaves the
/// repository.
fn inside(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// The directory `target` is the index of, and the extension of the files
/// that spell it so.
fn index_of(target: &str) -> Option<(&str, &'static str)> {
    let (dir, name) = target.rsplit_once('/')?;
    DIRECTORY_INDEX
        .iter()
        .find(|(index, _)| name == *index)
        .map(|(_, ext)| (dir, *ext))
}

/// A file a command line starts: a script by its extension, or a file with
/// none (a shebang script, which `is_data` tells from a data file).
fn is_runnable(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .is_none_or(|(_, ext)| SCRIPT_EXTENSIONS.contains(&ext))
}

/// A script is run from where it is installed, by its name or its name
/// without the extension: `~/.local/bin/backup`, `/usr/local/bin/deploy` for
/// `tools/deploy.sh`. The path is outside the repository: absolute, a home's
/// or a variable's, into a `bin` directory.
fn runs_installed(token: &str, target: &str) -> bool {
    let Some((dir, name)) = token.rsplit_once('/') else {
        return false;
    };
    let file = target.rsplit('/').next().unwrap_or(target);
    token.starts_with(['/', '~', '$'])
        && matches!(dir.rsplit('/').next(), Some("bin" | "sbin"))
        && !name.is_empty()
        && (name == file || name == stem(target))
        && !is_document(target)
        && is_runnable(target)
}

/// The route a file-routed framework serves a page file at: Astro, Next and
/// Nuxt route `pages/`, Next's app router `app/**/page.*`, `SvelteKit`
/// `routes/**/+page.svelte`. A dynamic segment (`[slug]`) is no one route,
/// a `_` file no page, and the root route names every page.
fn route_of(path: &str) -> Option<String> {
    let segments: Vec<&str> = path.split('/').collect();
    let (file, dirs) = segments.split_last()?;
    let (stem, ext) = file.rsplit_once('.')?;
    if !PAGE_EXTENSIONS.contains(&ext) {
        return None;
    }
    let routed: Vec<&str> = if let Some(at) = dirs.iter().rposition(|d| *d == "pages") {
        if stem.starts_with('_') {
            return None;
        }
        dirs[at + 1..]
            .iter()
            .copied()
            .chain((stem != "index").then_some(stem))
            .collect()
    } else {
        let marker = match stem {
            "page" => "app",
            "+page" => "routes",
            _ => return None,
        };
        let at = dirs.iter().rposition(|d| *d == marker)?;
        dirs[at + 1..]
            .iter()
            .copied()
            .filter(|d| !(d.starts_with('(') && d.ends_with(')')))
            .collect()
    };
    if routed.is_empty() || routed.iter().any(|s| s.starts_with(['[', '_'])) {
        return None;
    }
    Some(format!("/{}", routed.join("/")))
}

/// The URL the `public/` directory of a web project serves a file at, and the
/// directory the project sits in, which the pages naming it lie under:
/// `site/public/img/a.jpg` is `/img/a.jpg` for `site/`.
fn public_url(path: &str) -> Option<(String, String)> {
    let segments: Vec<&str> = path.split('/').collect();
    let (_, dirs) = segments.split_last()?;
    let at = dirs.iter().position(|d| *d == "public")?;
    let project: String = segments[..at].iter().flat_map(|d| [*d, "/"]).collect();
    Some((project, format!("/{}", segments[at + 1..].join("/"))))
}

fn is_shell(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    matches!(extension(path).as_str(), "sh" | "bash" | "zsh" | "ksh") || !name.contains('.')
}

fn is_shell_script(path: &str, content: &str) -> bool {
    matches!(extension(path).as_str(), "sh" | "bash" | "zsh" | "ksh")
        || content
            .lines()
            .next()
            .is_some_and(|l| l.starts_with("#!") && l.contains("sh"))
}

/// The directory a script sits in, as a path spelled from the root reads it.
fn script_dir(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(dir, _)| dir)
}

/// The variables of a shell script that hold a directory of the repository,
/// in the order the script assigns them: `CI_DIR=$(cd "$(dirname "$0")" &&
/// pwd)`, `ROOT="$CI_DIR/.."`. One assigned anything else, or two different
/// directories, holds none.
fn script_dirs(path: &str, content: &str) -> Vec<(String, String)> {
    let here = script_dir(path);
    let mut held: Vec<(String, Option<String>)> = Vec::new();
    for line in content.lines() {
        let Some(c) = ASSIGNMENT.captures(line) else {
            continue;
        };
        let name = c[1].to_string();
        let value = c[2].trim().trim_end_matches(';');
        let known: Vec<(String, String)> = held
            .iter()
            .filter_map(|(n, d)| d.clone().map(|d| (n.clone(), d)))
            .collect();
        let derived = SCRIPT_DIR.is_match(value)
            || VARIABLE
                .captures_iter(value)
                .any(|v| known.iter().any(|(n, _)| n == variable(&v)));
        let dir = derived
            .then(|| with_dirs(value, here, &known))
            .map(|v| v.trim_matches(['"', '\'']).to_string())
            .filter(|v| {
                !v.is_empty()
                    && v.chars()
                        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-'))
            })
            .and_then(|v| inside(&v))
            .map(|d| if d.is_empty() { ".".to_string() } else { d });
        match held.iter_mut().find(|(n, _)| *n == name) {
            Some((_, old)) if *old != dir => *old = None,
            Some(_) => {}
            None => held.push((name, dir)),
        }
    }
    held.into_iter()
        .filter_map(|(n, d)| d.map(|d| (n, d)))
        .collect()
}

/// `text` with the script's own directory and the variables that hold a
/// directory written out.
fn with_dirs(text: &str, here: &str, dirs: &[(String, String)]) -> String {
    let text = SCRIPT_DIR.replace_all(text, regex::NoExpand(here));
    let text = CD_PWD.replace_all(&text, "$1");
    VARIABLE
        .replace_all(&text, |c: &regex::Captures<'_>| {
            dirs.iter()
                .find(|(n, _)| n == variable(c))
                .map_or_else(|| c[0].to_string(), |(_, d)| d.clone())
        })
        .into_owned()
}

fn variable<'c>(c: &regex::Captures<'c>) -> &'c str {
    c.get(1).or_else(|| c.get(2)).map_or("", |m| m.as_str())
}

/// A path with `.` and `..` resolved where they follow a name, its leading
/// `./` or `../` kept: `./bin/../list.sh` is `./list.sh`, `chart/ci/..` is
/// `chart`.
pub(super) fn tidy(path: &str) -> String {
    let mut lead: Vec<&str> = Vec::new();
    let mut rest: Vec<&str> = Vec::new();
    for (i, part) in path.split('/').enumerate() {
        match part {
            "." if i == 0 => lead.push("."),
            "" | "." => {}
            ".." => {
                if rest.pop().is_none() {
                    lead.retain(|p| *p != ".");
                    lead.push("..");
                }
            }
            p => rest.push(p),
        }
    }
    if rest.is_empty() && lead.is_empty() {
        return ".".to_string();
    }
    lead.into_iter().chain(rest).collect::<Vec<_>>().join("/")
}
