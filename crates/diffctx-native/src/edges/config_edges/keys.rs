//! The typed key table.
//!
//! Files that name the same thing in different formats — a variable read in
//! code and set in a `.env`, a compose file and a manifest; an image a pipeline
//! builds and a manifest runs — share no import and no path. Each file is
//! scanned once for records `{kind, key, strength, role, mechanism, anchor}`
//! (`keys_extract.rs`); one join links every record to the same-key records of
//! a compatible role, under one ambiguity rule:
//!
//! - a key spelled in more than `MAX_FILES_PER_KEY` files is vocabulary and
//!   links nothing, and an image nothing in the tree builds is vocabulary too;
//! - units (the nearest directory holding a Dockerfile, a Chart.yaml or a
//!   package manifest) do not restrict the search. They break a tie only: when
//!   a record's candidates lie in more than one unit, the nearest to the
//!   record's anchor stays, and a tie keeps none;
//! - two definitions of a variable link only inside one unit: across units
//!   only a reader meets a definition.
//!
//! A key the join resolved typed owns its name for the change: an untyped
//! name match (`key_match`) between the changed record's file and a file that
//! merely spells that name adds nothing to the typed answer (`untyped_spellings`).
//!
//! The same join feeds the graph (`config` edges), discovery (a changed
//! file's counterparts enter the universe) and the typed-link report the
//! `key_links` gate reads.

use std::path::{Path, PathBuf};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::canon::{KeyCache, for_each_name_key};
use crate::changed_lines::ChangedLines;
use crate::config::weights::EDGE_WEIGHTS;
use crate::types::{Fragment, FragmentId};

use super::super::EdgeDict;
use super::super::base::{self, EdgeBuilder, add_edge, index_key_of};
use super::generic::MAX_FILES_PER_KEY;
use super::keys_extract::{
    FileClass, Kind, Mount, RawRecord, Role, Strength, classify, compatible, extract, route_key,
    worth_reading,
};
use super::routes::{join as join_path, module_stem};

/// A record placed in the tree: which file, and where its unit and anchor lie.
#[derive(Clone)]
pub(crate) struct Rec {
    file: usize,
    kind: Kind,
    key: String,
    strength: Strength,
    role: Role,
    #[allow(dead_code, reason = "read by the typed-link report")]
    mechanism: &'static str,
    /// 1-based; 0 stands for the file as a whole.
    line: usize,
    dir: String,
    unit: String,
    /// The change touched this record's file.
    file_changed: bool,
    /// The change added or rewrote a line the record spans (a whole changed
    /// Dockerfile for the build that names it).
    on_change: bool,
    router: Option<String>,
    mount: Option<Mount>,
}

/// Whether a link counts for the change. A changed file's untouched key is
/// not part of the change: a link counts when its changed side sits on a
/// changed line, and a link between two untouched files is the graph's
/// ordinary business. With no knowledge of the change everything counts.
fn live(a: &Rec, b: &Rec) -> bool {
    let drives = |r: &Rec| r.file_changed && r.on_change;
    drives(a) || drives(b) || (!a.file_changed && !b.file_changed)
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(d, _)| d)
}

/// Files whose directory is the root of a deployable or buildable unit.
fn is_unit_marker(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    name == "dockerfile"
        || name.starts_with("dockerfile.")
        || name.ends_with(".dockerfile")
        || matches!(
            name.as_str(),
            "chart.yaml"
                | "package.json"
                | "pyproject.toml"
                | "setup.py"
                | "go.mod"
                | "cargo.toml"
                | "pom.xml"
                | "build.gradle"
                | "build.gradle.kts"
                | "gemfile"
                | "composer.json"
                | "mix.exs"
        )
}

/// Records of the analysed files, placed among `rels` (every path of the
/// tree, which decides units and which Dockerfiles exist). `analysed` pairs
/// an index into `rels` with that file's records.
fn place(
    rels: &[String],
    analysed: &[(usize, Vec<RawRecord>)],
    changed: Option<&ChangedLines>,
) -> Vec<Rec> {
    let is_changed = |file: usize| changed.is_some_and(|c| c.is_changed_file(&rels[file]));
    let by_rel: FxHashMap<&str, usize> = rels
        .iter()
        .enumerate()
        .map(|(i, r)| (r.as_str(), i))
        .collect();
    let marker_dirs: FxHashSet<&str> = rels
        .iter()
        .filter(|r| is_unit_marker(r))
        .map(|r| dir_of(r))
        .collect();
    let unit_of = |file: usize| -> String {
        let mut dir = dir_of(&rels[file]);
        loop {
            if marker_dirs.contains(dir) {
                return dir.to_string();
            }
            if dir.is_empty() {
                return String::new();
            }
            dir = dir_of(dir);
        }
    };

    let mut seen: FxHashSet<(usize, Kind, String, Strength, Role, &'static str, usize)> =
        FxHashSet::default();
    let mut recs = Vec::new();
    for (file, raws) in analysed {
        let unit = unit_of(*file);
        let own_dir = dir_of(&rels[*file]);
        for raw in raws {
            if !seen.insert((
                *file,
                raw.kind,
                raw.key.clone(),
                raw.strength,
                raw.role,
                raw.mechanism,
                raw.line,
            )) {
                continue;
            }
            recs.push(Rec {
                file: *file,
                kind: raw.kind,
                key: raw.key.clone(),
                strength: raw.strength,
                role: raw.role,
                mechanism: raw.mechanism,
                line: raw.line,
                dir: raw.anchor.clone().unwrap_or_else(|| own_dir.to_string()),
                unit: unit.clone(),
                file_changed: is_changed(*file),
                on_change: changed
                    .is_some_and(|c| c.touches(&rels[*file], raw.line, raw.end.max(raw.line))),
                router: raw.router.clone(),
                mount: raw.mount.clone(),
            });
            let Some(&target) = raw.artifact.as_deref().and_then(|a| by_rel.get(a)) else {
                continue;
            };
            if target != *file && raw.role == Role::Build {
                recs.push(Rec {
                    file: target,
                    kind: raw.kind,
                    key: raw.key.clone(),
                    strength: raw.strength,
                    role: Role::Artifact,
                    mechanism: "build-context",
                    line: 0,
                    dir: dir_of(&rels[target]).to_string(),
                    unit: unit_of(target),
                    file_changed: is_changed(target),
                    on_change: is_changed(target),
                    router: None,
                    mount: None,
                });
            }
        }
    }
    recs
}

/// A bare table name means the schema its own unit gives that table, when the
/// unit's migrations and models qualify it with exactly one: `assets` in a
/// service whose migration says `stems.assets` is `stems.assets`. Any other
/// bare name stays bare, and bare names meet only inside their unit.
fn bind_schemas(recs: &mut [Rec]) {
    let mut schemas: FxHashMap<(String, String), FxHashSet<String>> = FxHashMap::default();
    for r in recs
        .iter()
        .filter(|r| r.kind == Kind::Table && r.role == Role::Set)
    {
        if let Some((schema, name)) = r.key.split_once('.') {
            schemas
                .entry((r.unit.clone(), name.to_string()))
                .or_default()
                .insert(schema.to_string());
        }
    }
    for r in recs
        .iter_mut()
        .filter(|r| r.kind == Kind::Table && !r.key.contains('.'))
    {
        if let Some(only) = schemas
            .get(&(r.unit.clone(), r.key.clone()))
            .filter(|s| s.len() == 1)
            .and_then(|s| s.iter().next())
        {
            r.key = format!("{only}.{}", r.key);
        }
    }
}

/// Whether a mount puts its prefix in front of a router's routes: a mount in
/// the router's own file names the router; one elsewhere names its module.
fn mount_applies(
    mount: &Rec,
    route: &Rec,
    rels: &[String],
    routers: &FxHashSet<(usize, &str)>,
) -> bool {
    let Some(target) = &mount.mount else {
        return false;
    };
    let Some(name) = route.router.as_deref() else {
        return false;
    };
    let named = target.router.is_empty() || target.router == name;
    if target.modules.is_empty() {
        return mount.file == route.file && named;
    }
    let stem = module_stem(&rels[route.file]);
    target.modules.contains(&stem)
        && (named || !routers.contains(&(route.file, target.router.as_str())))
}

/// Routes as the framework serves them: a router's routes behind the prefixes
/// it is mounted under, and, for what matches a path by prefix (an nginx
/// location, an Ingress path), every leading part of a route as a weak key.
fn finish_routes(recs: &mut Vec<Rec>, rels: &[String]) {
    if !recs.iter().any(|r| r.kind == Kind::Route) {
        return;
    }
    let (mounts, rest): (Vec<Rec>, Vec<Rec>) = std::mem::take(recs)
        .into_iter()
        .partition(|r| r.role == Role::Mount);
    let routers: FxHashSet<(usize, &str)> = rest
        .iter()
        .filter_map(|r| Some((r.file, r.router.as_deref()?)))
        .collect();
    let mut done: Vec<Rec> = Vec::with_capacity(rest.len());
    for rec in &rest {
        if rec.kind != Kind::Route || rec.role != Role::Set || rec.router.is_none() {
            done.push(rec.clone());
            continue;
        }
        let applying: Vec<&Rec> = mounts
            .iter()
            .filter(|m| mount_applies(m, rec, rels, &routers))
            .collect();
        if applying.is_empty() {
            if route_key(&rec.key).is_some_and(|k| k == rec.key) {
                done.push(rec.clone());
            }
            continue;
        }
        for mount in applying {
            if let Some(key) = route_key(&join_path(&mount.key, &rec.key)) {
                done.push(Rec { key, ..rec.clone() });
            }
        }
    }
    let mut prefixes: Vec<Rec> = Vec::new();
    for rec in done
        .iter()
        .filter(|r| r.kind == Kind::Route && r.role == Role::Set && r.strength == Strength::Strong)
    {
        let segments: Vec<&str> = rec.key.split('/').filter(|s| !s.is_empty()).collect();
        for n in 1..=segments.len() {
            let last = segments[n - 1];
            if last == "{}" || !segments[..n].iter().any(|s| *s != "{}" && s.len() >= 2) {
                continue;
            }
            prefixes.push(Rec {
                key: format!("/{}", segments[..n].join("/")),
                strength: Strength::Weak,
                mechanism: "route-prefix",
                router: None,
                ..rec.clone()
            });
        }
    }
    done.extend(prefixes);
    *recs = done;
}

fn components(path: &str) -> usize {
    path.split('/').filter(|c| !c.is_empty()).count()
}

fn is_within(outer: &str, inner: &str) -> bool {
    outer.is_empty() || inner == outer || inner.starts_with(&format!("{outer}/"))
}

/// How close `dir` lies to `anchor`: a directory the anchor owns (inside it),
/// then one that owns the anchor (an ancestor), then any other by the depth
/// of the common ancestor. A sibling branch never outranks an ancestor, so
/// a root `.env.example` still serves `services/api` when `services/worker`
/// declares the same name.
fn proximity(anchor: &str, dir: &str) -> (u8, usize) {
    if is_within(anchor, dir) {
        (0, components(dir))
    } else if is_within(dir, anchor) {
        (1, components(dir))
    } else {
        let common = anchor
            .split('/')
            .zip(dir.split('/'))
            .take_while(|(a, b)| a == b)
            .count();
        (2, common)
    }
}

/// Which of `candidates` a record keeps. One unit among them keeps all; more
/// than one keeps the nearest class and, within a class that is not the
/// anchor's own subtree, the deepest common ancestor — and nothing when that
/// is not a single unit.
fn keep<'a>(rec: &Rec, candidates: &[&'a Rec]) -> Vec<&'a Rec> {
    let units: FxHashSet<&str> = candidates.iter().map(|c| c.unit.as_str()).collect();
    if units.len() <= 1 {
        return candidates.to_vec();
    }
    let ranked: Vec<(u8, usize)> = candidates
        .iter()
        .map(|c| proximity(&rec.dir, &c.dir))
        .collect();
    let class = ranked.iter().map(|r| r.0).min().unwrap_or(2);
    if class == 0 {
        return candidates
            .iter()
            .zip(&ranked)
            .filter(|(_, r)| r.0 == 0)
            .map(|(c, _)| *c)
            .collect();
    }
    let depth = ranked
        .iter()
        .filter(|r| r.0 == class)
        .map(|r| r.1)
        .max()
        .unwrap_or(0);
    let best: Vec<&Rec> = candidates
        .iter()
        .zip(&ranked)
        .filter(|(_, r)| r.0 == class && r.1 == depth)
        .map(|(c, _)| *c)
        .collect();
    let best_units: FxHashSet<&str> = best.iter().map(|c| c.unit.as_str()).collect();
    if best_units.len() == 1 {
        best
    } else {
        Vec::new()
    }
}

/// A group bigger than this is not a key anyone means.
const MAX_RECORDS_PER_KEY: usize = 128;

pub(crate) struct Pair {
    pub a: usize,
    pub b: usize,
}

/// Whether two definitions of one key sit in one unit. Files under no marker
/// share the empty unit without belonging together: two services' settings
/// files named alike are a coincidence, so a variable's definitions there
/// meet only along one path of directories (a service's `.env.example`
/// above its `src/main/resources`), not across siblings. A compose
/// `env_file`'s anchor counts as the record's directory.
fn shares_unit(kind: Kind, a: &Rec, b: &Rec) -> bool {
    a.unit == b.unit
        && (kind != Kind::Env
            || !a.unit.is_empty()
            || is_within(&a.dir, &b.dir)
            || is_within(&b.dir, &a.dir))
}

/// The pairs of records that link: same key, compatible roles, in different
/// files, each keeping the other.
pub(crate) fn join(recs: &[Rec]) -> Vec<Pair> {
    let mut groups: FxHashMap<(Kind, Strength, &str), Vec<usize>> = FxHashMap::default();
    for (i, r) in recs.iter().enumerate() {
        groups
            .entry((r.kind, r.strength, r.key.as_str()))
            .or_default()
            .push(i);
    }
    let mut keys: Vec<_> = groups.keys().copied().collect();
    keys.sort();

    let mut pairs: Vec<Pair> = Vec::new();
    let mut strong_files: FxHashSet<(Kind, usize, usize)> = FxHashSet::default();
    for group_key in keys {
        let members = &groups[&group_key];
        let kind = group_key.0;
        let files: FxHashSet<usize> = members.iter().map(|i| recs[*i].file).collect();
        if files.len() > MAX_FILES_PER_KEY || files.len() < 2 || members.len() > MAX_RECORDS_PER_KEY
        {
            continue;
        }
        if kind == Kind::Image && !members.iter().any(|i| recs[*i].role == Role::Build) {
            continue;
        }
        let mut kept: FxHashSet<(usize, usize)> = FxHashSet::default();
        for &i in members {
            let rec = &recs[i];
            // Each role the record can meet is its own relation: the Dockerfile
            // a build names is not a rival of the manifest that runs the image.
            let mut wanted: Vec<&Rec> = Vec::new();
            for role in [
                Role::Read,
                Role::Set,
                Role::Build,
                Role::Consume,
                Role::Artifact,
            ] {
                if !compatible(kind, rec.role, role) {
                    continue;
                }
                let candidates: Vec<&Rec> = members
                    .iter()
                    .map(|j| &recs[*j])
                    .filter(|c| c.file != rec.file && c.role == role)
                    // Two definitions of one key link inside a unit only: across
                    // units they are a coincidence unless a reader ties them,
                    // and then each meets the reader, not the other.
                    .filter(|c| {
                        !(rec.role == Role::Set && c.role == Role::Set) || shares_unit(kind, rec, c)
                    })
                    // A table spelled without its schema is the unit's own.
                    .filter(|c| {
                        !(kind == Kind::Table && !rec.key.contains('.')) || c.unit == rec.unit
                    })
                    .collect();
                if !candidates.is_empty() {
                    wanted.extend(keep(rec, &candidates));
                }
            }
            for j in members {
                if wanted.iter().any(|w| std::ptr::eq(*w, &recs[*j])) {
                    kept.insert((i, *j));
                }
            }
        }
        let mut mutual: Vec<(usize, usize)> = kept
            .iter()
            .filter(|(i, j)| i < j && kept.contains(&(*j, *i)))
            .copied()
            .collect();
        mutual.sort_unstable();
        for (a, b) in mutual {
            let files = (
                recs[a].file.min(recs[b].file),
                recs[a].file.max(recs[b].file),
            );
            if group_key.1 == Strength::Strong {
                strong_files.insert((kind, files.0, files.1));
            } else if strong_files.contains(&(kind, files.0, files.1)) {
                continue;
            }
            pairs.push(Pair { a, b });
        }
    }
    pairs
}

/// The fragments of one file, to say which one a line sits in.
struct FileFrags<'a> {
    spans: Vec<(u32, u32, &'a FragmentId)>,
    representative: &'a FragmentId,
}

impl<'a> FileFrags<'a> {
    fn at(&self, line: usize) -> &'a FragmentId {
        let line = line as u32;
        if line == 0 {
            return self.representative;
        }
        self.spans
            .iter()
            .filter(|(s, e, _)| *s <= line && line <= *e)
            .min_by(|a, b| (a.1 - a.0, a.0, a.2).cmp(&(b.1 - b.0, b.0, b.2)))
            .map_or(self.representative, |(_, _, id)| id)
    }
}

/// The file's text rebuilt from its fragments by line number: tree-sitter
/// emits nested fragments for YAML, and concatenating them would repeat
/// lines and push every offset off.
fn file_text(frags: &[&Fragment]) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for f in frags {
        for (i, line) in f.content.lines().enumerate() {
            let idx = (f.id.start_line as usize).saturating_sub(1) + i;
            if lines.len() <= idx {
                lines.resize(idx + 1, "");
            }
            if lines[idx].is_empty() {
                lines[idx] = line;
            }
        }
    }
    let mut text = String::new();
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The records the change removed from a file, read where they stood: the
/// file before the change is rebuilt from its text after and the runs, so a
/// removed YAML list item still has its `env:` parent. Each record sits on
/// the new-side line its removal anchors on, so it can name a fragment. When
/// the runs do not fit the text, the removed lines alone are read.
fn removed_records(
    rel: &str,
    class: FileClass,
    new_text: &str,
    lines: &ChangedLines,
) -> Vec<RawRecord> {
    let Some((old, removed)) = lines.base_side(rel, new_text) else {
        let (Some(text), Some(anchor)) = (lines.removed_text(rel), lines.first_removal_anchor(rel))
        else {
            return Vec::new();
        };
        return extract(rel, class, text)
            .into_iter()
            .map(|r| RawRecord {
                line: anchor,
                end: anchor,
                ..r
            })
            .collect();
    };
    extract(rel, class, &old)
        .into_iter()
        .filter_map(|r| {
            let &(_, _, anchor) = removed
                .iter()
                .find(|&&(from, to, _)| r.line <= to && from <= r.end.max(r.line))?;
            Some(RawRecord {
                line: anchor.max(1),
                end: anchor.max(1),
                ..r
            })
        })
        .collect()
}

struct Table<'a> {
    paths: Vec<&'a str>,
    frags: Vec<FileFrags<'a>>,
    recs: Vec<Rec>,
}

impl<'a> Table<'a> {
    fn from_fragments(fragments: &'a [Fragment], repo_root: Option<&Path>) -> Self {
        let mut by_path: FxHashMap<&str, Vec<&Fragment>> = FxHashMap::default();
        for f in fragments {
            by_path.entry(f.path()).or_default().push(f);
        }
        let mut paths: Vec<&str> = by_path.keys().copied().collect();
        paths.sort_unstable();
        let rels: Vec<String> = paths.iter().map(|p| index_key_of(p, repo_root)).collect();

        let mut frags = Vec::with_capacity(paths.len());
        let mut analysed: Vec<(usize, Vec<RawRecord>)> = Vec::new();
        let mut ghosts: Vec<(usize, Vec<RawRecord>)> = Vec::new();
        let changed = crate::resource::current_changed_lines();
        for (file, path) in paths.iter().enumerate() {
            let in_file = &by_path[path];
            let representative = in_file
                .iter()
                .max_by(|a, b| {
                    a.token_count
                        .cmp(&b.token_count)
                        .then_with(|| b.id.cmp(&a.id))
                })
                .map(|f| &f.id)
                .expect("a path has at least one fragment");
            frags.push(FileFrags {
                spans: in_file
                    .iter()
                    .map(|f| (f.id.start_line, f.id.end_line, &f.id))
                    .collect(),
                representative,
            });
            if !crate::resource::poll_current_every(file, 32) {
                break;
            }
            let Some(class) = classify(&rels[file]) else {
                continue;
            };
            let text = changed
                .as_ref()
                .filter(|c| c.is_changed_file(&rels[file]))
                .map(|_| file_text(in_file));
            if let (Some(lines), Some(text)) = (&changed, &text) {
                let removed = removed_records(&rels[file], class, text, lines);
                if !removed.is_empty() {
                    ghosts.push((file, removed));
                }
            }
            if !worth_reading(class, in_file.iter().map(|f| f.content.as_ref())) {
                continue;
            }
            let records = extract(
                &rels[file],
                class,
                &text.unwrap_or_else(|| file_text(in_file)),
            );
            if !records.is_empty() {
                analysed.push((file, records));
            }
        }
        // `frags` is complete only if the loop was not cut short; a cut
        // loop leaves later files without spans, and their records unplaced.
        let mut recs: Vec<Rec> = place(&rels, &analysed, changed.as_deref());
        // What the change removed names its counterparts the way what it
        // added does, from the line the removal anchors on.
        let mut removed_recs = place(&rels, &ghosts, None);
        for r in &mut removed_recs {
            r.file_changed = true;
            r.on_change = true;
        }
        recs.extend(removed_recs);
        recs.retain(|r| r.file < frags.len());
        finish_routes(&mut recs, &rels);
        bind_schemas(&mut recs);
        Self { paths, frags, recs }
    }

    /// Every pair of files that spell the same key, whether the join linked
    /// them or refused to: the table has taken a position on those pairs. A
    /// key too common to name anything (the vocabulary bar) takes none.
    fn ruled_on_pairs(&self) -> Vec<(String, String)> {
        let mut by_key: FxHashMap<(Kind, Strength, &str), FxHashSet<usize>> = FxHashMap::default();
        for r in &self.recs {
            by_key
                .entry((r.kind, r.strength, r.key.as_str()))
                .or_default()
                .insert(r.file);
        }
        let mut out = Vec::new();
        for (&(_, strength, _), files) in &by_key {
            // A weak key links below the naming floor: it takes no position.
            if strength == Strength::Weak || files.len() < 2 || files.len() > MAX_FILES_PER_KEY {
                continue;
            }
            let mut files: Vec<&str> = files.iter().map(|f| self.paths[*f]).collect();
            files.sort_unstable();
            for (i, a) in files.iter().enumerate() {
                for b in &files[i + 1..] {
                    out.push(((*a).to_string(), (*b).to_string()));
                }
            }
        }
        out
    }

    /// The environment variables the change's own records name, once the join
    /// tied a reader to a provider (a link is live only for a name read or set
    /// on a changed line), with the files that hold those records. Other kinds name
    /// things code spells as ordinary words everywhere (an image, a table, a
    /// path segment): a shared word there is vocabulary, not the key.
    fn owned_names(&self) -> FxHashMap<String, FxHashSet<usize>> {
        let mut owned: FxHashMap<String, FxHashSet<usize>> = FxHashMap::default();
        for pair in self.live_links() {
            let ends = [&self.recs[pair.a], &self.recs[pair.b]];
            // A reader met its provider: two providers agreeing say nothing
            // about the code that reads the name in a language nobody parses.
            if ends[0].kind != Kind::Env || depends_on(ends[0].role, ends[1].role).is_none() {
                continue;
            }
            for rec in ends {
                if rec.file_changed && rec.on_change {
                    owned.entry(rec.key.clone()).or_default().insert(rec.file);
                }
            }
        }
        owned
    }

    /// Pairs of a changed record's file and a file that spells one of its
    /// resolved names without a record of its own: a local constant
    /// `QUEUE_URL = ...` beside the env key `QUEUE_URL` the join already tied
    /// to its readers. The table has taken a position on those pairs too, so
    /// the untyped name match does not overrule it.
    fn untyped_spellings(&self, fragments: &[Fragment]) -> Vec<(String, String)> {
        let owned = self.owned_names();
        if owned.is_empty() {
            return Vec::new();
        }
        let mut cache = KeyCache::default();
        let mut pairs: FxHashSet<(&str, &str)> = FxHashSet::default();
        for (n, f) in fragments.iter().enumerate() {
            if !crate::resource::poll_current_every(n, 64) {
                break;
            }
            let path = f.path();
            for_each_name_key(&f.content, &mut cache, |key| {
                for &file in owned.get(key).into_iter().flatten() {
                    let owner = self.paths[file];
                    if owner != path {
                        pairs.insert((owner.min(path), owner.max(path)));
                    }
                }
            });
        }
        let mut pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        pairs.sort_unstable();
        pairs
    }

    /// The links that count for the change.
    fn live_links(&self) -> Vec<Pair> {
        join(&self.recs)
            .into_iter()
            .filter(|p| live(&self.recs[p.a], &self.recs[p.b]))
            .collect()
    }

    fn fragment(&self, rec: &Rec) -> &'a FragmentId {
        self.frags[rec.file].at(rec.line)
    }
}

/// Which end of a pair depends on the other, the way every builder points its
/// forward weight: a reader on the file that sets the variable, a manifest on
/// the build that makes its image, a build on the Dockerfile it names.
/// `Some(true)`: the first depends on the second. Roles that are peers (two
/// providers, two consumers) depend on neither.
fn depends_on(a: Role, b: Role) -> Option<bool> {
    match (a, b) {
        (Role::Read, Role::Set) | (Role::Consume, Role::Build) | (Role::Build, Role::Artifact) => {
            Some(true)
        }
        (Role::Set, Role::Read) | (Role::Build, Role::Consume) | (Role::Artifact, Role::Build) => {
            Some(false)
        }
        _ => None,
    }
}

fn weight(kind: Kind, strength: Strength) -> (f64, f64) {
    let w = match (kind, strength) {
        (Kind::Env, _) => &EDGE_WEIGHTS["config_env"],
        (Kind::Image, Strength::Strong) => &EDGE_WEIGHTS["config_image"],
        (Kind::Image, Strength::Weak) => &EDGE_WEIGHTS["config_image_weak"],
        (Kind::Route, Strength::Strong) => &EDGE_WEIGHTS["config_route"],
        (Kind::Route, Strength::Weak) => &EDGE_WEIGHTS["config_route_prefix"],
        (Kind::Table, _) => &EDGE_WEIGHTS["config_table"],
        (Kind::Proto, Strength::Strong) => &EDGE_WEIGHTS["config_proto"],
        (Kind::Proto, Strength::Weak) => &EDGE_WEIGHTS["config_proto_weak"],
    };
    (w.forward, w.reverse_factor)
}

/// A link of the table, for the report the gate reads.
pub(crate) struct TypedLink {
    pub kind: &'static str,
    pub strength: &'static str,
    pub key: String,
    pub a_path: String,
    pub b_path: String,
    pub a_mechanism: &'static str,
    pub b_mechanism: &'static str,
}

pub(crate) fn typed_links(fragments: &[Fragment], repo_root: Option<&Path>) -> Vec<TypedLink> {
    let table = Table::from_fragments(fragments, repo_root);
    table
        .live_links()
        .into_iter()
        .map(|p| {
            let (a, b) = (&table.recs[p.a], &table.recs[p.b]);
            TypedLink {
                kind: a.kind.as_str(),
                strength: a.strength.as_str(),
                key: a.key.clone(),
                a_path: table.paths[a.file].to_string(),
                b_path: table.paths[b.file].to_string(),
                a_mechanism: a.mechanism,
                b_mechanism: b.mechanism,
            }
        })
        .collect()
}

/// A record the change touched in a changed file and the record of an
/// untouched file that names the same key: who reads what the change
/// rewrote, or defines what it reads.
pub(crate) struct Counterpart {
    pub kind: &'static str,
    pub key: String,
    pub changed_path: String,
    pub path: String,
    /// 1-based; 0 stands for the file as a whole.
    pub line: usize,
}

/// The strong links of the table that count for the change and end in a file
/// outside it: the impact view's answer to "who reads this key", from the
/// same join the graph and discovery use. Without the change's lines nothing
/// is the change's.
pub(crate) fn counterparts_of_change(
    fragments: &[Fragment],
    repo_root: Option<&Path>,
    lines: Option<std::sync::Arc<ChangedLines>>,
) -> Vec<Counterpart> {
    if lines.is_none() {
        return Vec::new();
    }
    crate::resource::with_changed_lines(lines, || {
        let table = Table::from_fragments(fragments, repo_root);
        let mut out = Vec::new();
        for pair in table.live_links() {
            let (a, b) = (&table.recs[pair.a], &table.recs[pair.b]);
            if a.strength != Strength::Strong {
                continue;
            }
            for (changed, other) in [(a, b), (b, a)] {
                if changed.file_changed && changed.on_change && !other.file_changed {
                    out.push(Counterpart {
                        kind: changed.kind.as_str(),
                        key: changed.key.clone(),
                        changed_path: table.paths[changed.file].to_string(),
                        path: table.paths[other.file].to_string(),
                        line: other.line,
                    });
                }
            }
        }
        out
    })
}

pub struct KeyTableEdgeBuilder;

impl EdgeBuilder for KeyTableEdgeBuilder {
    fn is_key_table(&self) -> bool {
        true
    }

    fn build(&self, fragments: &[Fragment], repo_root: Option<&Path>) -> EdgeDict {
        self.build_deciding(fragments, repo_root).0
    }

    fn build_deciding(
        &self,
        fragments: &[Fragment],
        repo_root: Option<&Path>,
    ) -> (EdgeDict, Vec<(String, String)>) {
        let table = Table::from_fragments(fragments, repo_root);
        let mut ruled_on = table.ruled_on_pairs();
        ruled_on.extend(table.untyped_spellings(fragments));
        let mut edges = EdgeDict::default();
        let mut reported = 0u64;
        for (n, pair) in table.live_links().into_iter().enumerate() {
            if !crate::resource::poll_emissions(n, 64, edges.len() as u64, &mut reported) {
                break;
            }
            let (a, b) = (&table.recs[pair.a], &table.recs[pair.b]);
            let (forward, reverse_factor) = weight(a.kind, a.strength);
            let (src, dst) = (table.fragment(a), table.fragment(b));
            match depends_on(a.role, b.role) {
                Some(true) => add_edge(&mut edges, src, dst, forward, reverse_factor),
                Some(false) => add_edge(&mut edges, dst, src, forward, reverse_factor),
                None => {
                    add_edge(&mut edges, src, dst, forward, reverse_factor);
                    add_edge(&mut edges, dst, src, forward, reverse_factor);
                }
            }
        }
        (edges, ruled_on)
    }

    /// A changed file's records bring in every file holding a same-key record
    /// of a compatible role, under the vocabulary bar the graph applies: a
    /// changed reader brings its providers, a changed provider its readers and
    /// the other providers. An edge whose endpoint never entered the universe
    /// does nothing.
    fn discover_related_files(
        &self,
        changed: &[PathBuf],
        candidates: &[PathBuf],
        repo_root: Option<&Path>,
        file_cache: Option<&FxHashMap<PathBuf, String>>,
    ) -> Vec<PathBuf> {
        let mut seen: FxHashSet<&PathBuf> = FxHashSet::default();
        let universe: Vec<&PathBuf> = changed
            .iter()
            .chain(candidates)
            .filter(|p| seen.insert(*p))
            .collect();
        let rels: Vec<String> = universe
            .iter()
            .map(|p| index_key_of(&p.to_string_lossy(), repo_root))
            .collect();
        let changed_set: FxHashSet<&PathBuf> = changed.iter().collect();
        let is_changed = |file: usize| changed_set.contains(universe[file]);

        let removes_something = |rels: &[String], is_changed: &dyn Fn(usize) -> bool| {
            crate::resource::current_changed_lines().is_some_and(|lines| {
                (0..rels.len()).any(|f| is_changed(f) && lines.removed_text(&rels[f]).is_some())
            })
        };
        let mut analysed: Vec<(usize, Vec<RawRecord>)> = Vec::new();
        let mut new_texts: FxHashMap<usize, String> = FxHashMap::default();
        // The changed files first: with no record of theirs there is nothing
        // to look up, and the candidates are never read.
        for pass_changed in [true, false] {
            for (file, path) in universe.iter().enumerate() {
                if is_changed(file) != pass_changed {
                    continue;
                }
                if !crate::resource::poll_current_every(file, 16) {
                    return Vec::new();
                }
                let Some(class) = classify(&rels[file]) else {
                    continue;
                };
                let Some(text) = base::read_file_cached(path, file_cache) else {
                    continue;
                };
                if !worth_reading(class, std::iter::once(text.as_str())) {
                    continue;
                }
                let records = extract(&rels[file], class, &text);
                if !records.is_empty() {
                    analysed.push((file, records));
                }
                if pass_changed {
                    new_texts.insert(file, text);
                }
            }
            if pass_changed && analysed.is_empty() && !removes_something(&rels, &is_changed) {
                return Vec::new();
            }
        }

        let lines = crate::resource::current_changed_lines();
        let mut recs = place(&rels, &analysed, lines.as_deref());
        // What the change removed is gone from the file but not from the
        // tree: the readers and providers of a removed key are the change's.
        if let Some(lines) = &lines {
            let ghosts: Vec<(usize, Vec<RawRecord>)> = (0..universe.len())
                .filter(|f| is_changed(*f))
                .filter_map(|f| {
                    let class = classify(&rels[f])?;
                    let text = new_texts.get(&f)?;
                    Some((f, removed_records(&rels[f], class, text, lines)))
                })
                .collect();
            let mut ghost_recs = place(&rels, &ghosts, None);
            for r in &mut ghost_recs {
                r.file_changed = true;
                r.on_change = true;
            }
            recs.extend(ghost_recs);
        }
        finish_routes(&mut recs, &rels);
        bind_schemas(&mut recs);
        let mut found: FxHashSet<usize> = FxHashSet::default();
        for pair in join(&recs) {
            let (a, b) = (&recs[pair.a], &recs[pair.b]);
            if !live(a, b) {
                continue;
            }
            match (is_changed(a.file), is_changed(b.file)) {
                (true, false) => found.insert(b.file),
                (false, true) => found.insert(a.file),
                _ => false,
            };
        }
        let mut out: Vec<PathBuf> = found.into_iter().map(|f| universe[f].clone()).collect();
        out.sort();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(files: &[(&str, &str)]) -> FxHashMap<PathBuf, String> {
        files
            .iter()
            .map(|(p, c)| (PathBuf::from(p), (*c).to_string()))
            .collect()
    }

    fn discovered(changed: &[&str], files: &[(&str, &str)]) -> Vec<String> {
        let all: Vec<PathBuf> = files.iter().map(|(p, _)| PathBuf::from(p)).collect();
        let changed: Vec<PathBuf> = changed.iter().map(PathBuf::from).collect();
        KeyTableEdgeBuilder
            .discover_related_files(&changed, &all, Some(Path::new(".")), Some(&cache(files)))
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect()
    }

    const FILES: &[(&str, &str)] = &[
        (
            "cmd/main.go",
            "package main\nfunc f() { os.Getenv(\"CACHE_TTL\") }\n",
        ),
        (
            "deploy/server.yaml",
            "kind: Deployment\nspec:\n  containers:\n    - name: s\n      env:\n        - name: CACHE_TTL\n          value: \"1\"\n",
        ),
        (
            "deploy/other.yaml",
            "kind: Deployment\nspec:\n  containers:\n    - name: o\n      env:\n        - name: CACHE_TTL_JITTER\n          value: \"1\"\n",
        ),
    ];

    #[test]
    fn a_changed_reader_brings_its_provider_and_only_it() {
        assert_eq!(
            discovered(&["cmd/main.go"], FILES),
            vec!["deploy/server.yaml"]
        );
    }

    #[test]
    fn a_changed_provider_brings_its_reader() {
        assert_eq!(
            discovered(&["deploy/server.yaml"], FILES),
            vec!["cmd/main.go"]
        );
    }

    const TWO_UNITS: &[(&str, &str)] = &[
        ("svc/api/Dockerfile", "FROM python:3.12\n"),
        ("svc/api/.env.example", "SINK_URL=a\n"),
        ("svc/worker/Dockerfile", "FROM python:3.12\n"),
        ("svc/worker/.env.example", "SINK_URL=b\n"),
    ];

    #[test]
    fn two_definitions_in_two_units_link_only_through_a_reader() {
        assert!(discovered(&["svc/api/.env.example"], TWO_UNITS).is_empty());
        let mut with_reader = TWO_UNITS.to_vec();
        with_reader.push((
            "svc/api/app.py",
            "import os\nSINK = os.environ[\"SINK_URL\"]\n",
        ));
        let found = discovered(&["svc/api/.env.example"], &with_reader);
        assert_eq!(found, vec!["svc/api/app.py"]);
    }

    fn discovered_with(diff: &str, changed: &[&str], files: &[(&str, &str)]) -> Vec<String> {
        let lines = crate::changed_lines::ChangedLines::from_diff(diff);
        crate::resource::with_changed_lines(Some(std::sync::Arc::new(lines)), || {
            discovered(changed, files)
        })
    }

    const UNTOUCHED: &[(&str, &str)] = &[
        ("svc/api/Dockerfile", "FROM python:3.12\n"),
        ("svc/api/.env.example", "DB_URL=x\nTTL=60\n"),
        ("svc/api/app.py", "import os\nT = os.environ[\"TTL\"]\n"),
        ("svc/worker/Dockerfile", "FROM python:3.12\n"),
        (
            "svc/worker/app.py",
            "import os\nD = os.environ[\"DB_URL\"]\n",
        ),
    ];

    #[test]
    fn only_the_keys_on_changed_lines_drive_discovery() {
        let diff = "diff --git a/svc/api/.env.example b/svc/api/.env.example\n--- a/svc/api/.env.example\n+++ b/svc/api/.env.example\n@@ -1 +2 @@\n+TTL=60\n";
        let changed = ["svc/api/.env.example"];
        assert_eq!(
            discovered_with(diff, &changed, UNTOUCHED),
            vec!["svc/api/app.py"],
            "the untouched DB_URL must not bring the other unit's reader"
        );
        assert_eq!(
            discovered(&changed, UNTOUCHED),
            vec!["svc/api/app.py", "svc/worker/app.py"],
            "without the change's lines every key counts"
        );
    }

    #[test]
    fn a_removed_key_still_brings_its_readers() {
        let diff = "diff --git a/svc/api/.env.example b/svc/api/.env.example\n--- a/svc/api/.env.example\n+++ b/svc/api/.env.example\n@@ -2 +1,0 @@\n-TTL=60\n";
        let after: Vec<(&str, &str)> = UNTOUCHED
            .iter()
            .map(|(p, c)| {
                (
                    *p,
                    if *p == "svc/api/.env.example" {
                        "DB_URL=x\n"
                    } else {
                        *c
                    },
                )
            })
            .collect();
        assert_eq!(
            discovered_with(diff, &["svc/api/.env.example"], &after),
            vec!["svc/api/app.py"]
        );
    }

    #[test]
    fn a_renamed_compose_variable_still_names_the_readers_of_the_old_name() {
        let diff = "diff --git a/svc/api/docker-compose.yml b/svc/api/docker-compose.yml\n--- a/svc/api/docker-compose.yml\n+++ b/svc/api/docker-compose.yml\n@@ -4 +4 @@\n-      - OLD_HOST=x\n+      - NEW_HOST=x\n";
        let files = [
            ("svc/api/Dockerfile", "FROM python:3.12\n"),
            (
                "svc/api/docker-compose.yml",
                "services:\n  app:\n    environment:\n      - NEW_HOST=x\n",
            ),
            (
                "svc/api/app.py",
                "import os\nH = os.environ[\"OLD_HOST\"]\n",
            ),
            (
                "svc/api/other.py",
                "import os\nH = os.environ[\"UNRELATED\"]\n",
            ),
        ];
        assert_eq!(
            discovered_with(diff, &["svc/api/docker-compose.yml"], &files),
            vec!["svc/api/app.py"]
        );
    }

    const SCHEMAS: &[(&str, &str)] = &[
        ("stems/Dockerfile", "FROM python:3.12\n"),
        ("stems/db/0007.sql", "CREATE TABLE stems.assets (id int);\n"),
        ("stems/app/q.py", "db.execute(\"SELECT id FROM assets\")\n"),
        ("library/Dockerfile", "FROM python:3.12\n"),
        (
            "library/db/0001.sql",
            "CREATE TABLE library.assets (id int);\n",
        ),
        (
            "library/app/q.py",
            "db.execute(\"SELECT id FROM library.assets\")\n",
        ),
        ("reports/Dockerfile", "FROM python:3.12\n"),
        ("reports/q.py", "db.execute(\"SELECT id FROM assets\")\n"),
    ];

    #[test]
    fn two_schemas_holding_one_table_name_never_link() {
        // A bare name is the unit's own table: the stems query meets only the
        // stems migration, and the other schema's files and the bare name in
        // a unit with no such table stay out.
        assert_eq!(
            discovered(&["stems/db/0007.sql"], SCHEMAS),
            vec!["stems/app/q.py"]
        );
        assert_eq!(
            discovered(&["library/db/0001.sql"], SCHEMAS),
            vec!["library/app/q.py"]
        );
        assert!(discovered(&["reports/q.py"], SCHEMAS).is_empty());
    }

    const MOUNTED: &[(&str, &str)] = &[
        (
            "api/main.py",
            "from fastapi import FastAPI\nfrom app.routers import items\n\napp = FastAPI()\napp.include_router(items.router, prefix=\"/api\")\n",
        ),
        (
            "api/routers/items.py",
            "router = APIRouter(prefix=\"/items\")\n\n@router.get(\"/{item_id}\")\ndef one(item_id): ...\n",
        ),
        (
            "web/items.ts",
            "const API = \"/api\";\nexport const one = (id) => fetch(`${API}/items/${id}`);\n",
        ),
        (
            "web/other.ts",
            "export const other = (id) => fetch(`/items/${id}`);\n",
        ),
        (
            "edge/nginx.conf",
            "server {\n  location /api/items/ {\n    proxy_pass http://api;\n  }\n}\n",
        ),
        (
            "edge/other.conf",
            "server {\n  location /api/ {\n    proxy_pass http://api;\n  }\n}\n",
        ),
    ];

    #[test]
    fn a_router_is_served_behind_the_prefixes_it_is_mounted_under() {
        // The handler's own file says `/items/{id}`; the mount in main.py puts
        // `/api` in front, so the client that spells `/api/items/..` and the
        // proxy locations whose prefix it falls under are its readers, and the
        // client that spells `/items/..` is not.
        assert_eq!(
            discovered(&["api/routers/items.py"], MOUNTED),
            vec!["edge/nginx.conf", "edge/other.conf", "web/items.ts"]
        );
        assert_eq!(
            discovered(&["web/items.ts"], MOUNTED),
            vec!["api/routers/items.py"]
        );
    }

    const SINK_DIFF: &str = "diff --git a/svc/app.env b/svc/app.env\n--- a/svc/app.env\n+++ b/svc/app.env\n@@ -1 +1 @@\n+ZZSINKHOST=http://a\n";

    const SINK: &[(&str, &str)] = &[
        ("svc/app.env", "ZZSINKHOST=http://a\n"),
        (
            "svc/app.py",
            "import os\nSINK = os.environ[\"ZZSINKHOST\"]\n",
        ),
        ("tools/loadtest.py", "ZZSINKHOST = \"http://localhost:9\"\n"),
        ("tools/other.py", "SINK_COUNT = 3\n"),
    ];

    fn untyped_pairs(diff: Option<&str>, files: &[(&str, &str)]) -> Vec<(String, String)> {
        let fragments: Vec<Fragment> = files
            .iter()
            .flat_map(|(p, c)| crate::parsers::fragment_file(std::sync::Arc::from(*p), c))
            .collect();
        let lines =
            diff.map(|d| std::sync::Arc::new(crate::changed_lines::ChangedLines::from_diff(d)));
        crate::resource::with_changed_lines(lines, || {
            KeyTableEdgeBuilder.build_deciding(&fragments, None).1
        })
    }

    fn holds(pairs: &[(String, String)], a: &str, b: &str) -> bool {
        pairs.iter().any(|(x, y)| x == a && y == b)
    }

    #[test]
    fn a_variable_the_change_names_owns_its_name_against_a_local_constant() {
        let pairs = untyped_pairs(Some(SINK_DIFF), SINK);
        assert!(
            holds(&pairs, "svc/app.env", "tools/loadtest.py"),
            "the constant spelled like the key is not what the change reaches"
        );
        assert!(
            !holds(&pairs, "svc/app.env", "tools/other.py"),
            "a file that spells another name is no business of the key"
        );
        assert!(
            !holds(&pairs, "svc/app.py", "tools/loadtest.py"),
            "the reader's line is not on the change"
        );
    }

    #[test]
    fn a_name_the_table_did_not_resolve_stays_with_the_name_match() {
        let without_reader: Vec<(&str, &str)> = SINK
            .iter()
            .filter(|(p, _)| *p != "svc/app.py")
            .copied()
            .collect();
        assert!(untyped_pairs(Some(SINK_DIFF), &without_reader).is_empty());
        let unmarked = untyped_pairs(None, SINK);
        assert!(
            holds(&unmarked, "svc/app.env", "svc/app.py"),
            "the typed pair is ruled on with or without the change"
        );
        assert!(
            !holds(&unmarked, "svc/app.env", "tools/loadtest.py"),
            "without the change's lines no record is the changed one"
        );
    }

    const NESTED_PRICE: &str = "demo:\n  currency: EUR\n  total: ${demo.item-price:10}\n";
    const FLAT_PRICE: &str = "currency: EUR\ntotal: ${demo.item-price:10}\n";
    const APPLICATION: &str = "billing/src/main/resources/application.yml";

    /// The edges the key table draws for a change that adds the last line of
    /// `application`, with the fragments the change seeds.
    fn price_edges(application: &str, line: u32) -> (EdgeDict, FxHashSet<FragmentId>) {
        let files = [
            (
                "billing/Dockerfile",
                "FROM eclipse-temurin:21\nCOPY . /srv\n",
            ),
            ("billing/.env.example", "DEMO_ITEMPRICE=10\n"),
            (APPLICATION, application),
        ];
        let fragments: Vec<Fragment> = files
            .iter()
            .flat_map(|(p, c)| crate::parsers::fragment_file(std::sync::Arc::from(*p), c))
            .collect();
        let added = application.lines().last().unwrap_or_default();
        let diff = format!(
            "diff --git a/{APPLICATION} b/{APPLICATION}\n--- a/{APPLICATION}\n+++ b/{APPLICATION}\n@@ -{},0 +{line} @@\n+{added}\n",
            line - 1
        );
        let hunks = [crate::types::DiffHunk {
            path: std::sync::Arc::from(APPLICATION),
            new_start: line,
            new_len: 1,
            old_start: line - 1,
            old_len: 0,
        }];
        let core = crate::core::identify_core_fragments(&hunks, &fragments);
        let lines = std::sync::Arc::new(crate::changed_lines::ChangedLines::from_diff(&diff));
        let edges = crate::resource::with_changed_lines(Some(lines), || {
            KeyTableEdgeBuilder.build(&fragments, None)
        });
        (edges, core)
    }

    #[test]
    fn a_typed_link_leaves_the_fragment_the_change_seeds_flat_or_nested() {
        for (application, line) in [(FLAT_PRICE, 2), (NESTED_PRICE, 3)] {
            let (edges, core) = price_edges(application, line);
            assert!(
                edges
                    .keys()
                    .any(|(src, dst)| core.contains(src)
                        && dst.path.as_ref() == "billing/.env.example"),
                "no edge from the seeded fragment {core:?} to the variable in {application:?}: {edges:?}"
            );
        }
    }

    fn setting(file: usize, dir: &str, unit: &str) -> Rec {
        Rec {
            file,
            kind: Kind::Env,
            key: "pool_size".to_string(),
            strength: Strength::Strong,
            role: Role::Set,
            mechanism: "spring-yaml-key",
            line: 1,
            dir: dir.to_string(),
            unit: unit.to_string(),
            file_changed: true,
            on_change: true,
            router: None,
            mount: None,
        }
    }

    #[test]
    fn definitions_under_no_marker_meet_only_along_one_path() {
        let linked = |a: Rec, b: Rec| !join(&[a, b]).is_empty();
        assert!(
            !linked(setting(0, "backend/res", ""), setting(1, "reports/res", "")),
            "two services without a marker are no unit"
        );
        assert!(linked(setting(0, "res", ""), setting(1, "res", "")));
        assert!(
            linked(setting(0, "backend", ""), setting(1, "backend/res", "")),
            "a service's file above its resources is on one path"
        );
        assert!(
            linked(
                setting(0, "billing/a", "billing"),
                setting(1, "billing/b", "billing")
            ),
            "a marked unit still holds its definitions together"
        );
        assert!(!linked(
            setting(0, "a", "billing"),
            setting(1, "b", "ledger")
        ));
        let table = |file, dir: &str| Rec {
            kind: Kind::Table,
            key: "shop.orders".to_string(),
            ..setting(file, dir, "")
        };
        assert!(
            linked(table(0, "db/migrations"), table(1, "app/models")),
            "a migration and its model are one table wherever they sit"
        );
    }
}
