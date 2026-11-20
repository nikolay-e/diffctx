//! The keys a config change touched and who reads them: operations, typed readers and collection reads.

use rustc_hash::{FxHashMap, FxHashSet};

use crate::decide::cells::Cells;
use crate::decide::{self};

use crate::run_provenance::state::ScoredState;

use diffctx_facts::parsers::code_text;

use diffctx_facts::parsers::embedded::names_word;

use super::mentions::{class_of, commented, is_config_file};

use super::MentionClass;
use super::callers::is_document;
use super::changes::{changed_rels, head_of, is_schema_file, rel};
use super::output::{CollectionRead, KeySearch, PathReference, UnanalysedFile};
use super::removed::{MAX_NAMED_BY, MAX_NAMED_BY_PER_TARGET, read_or_note};
use super::unanalysed::{MAX_UNTRACED_NAMERS, Nameless, enclosing_names};

/// The operation ids an API description's change wrote or took out: a generated
/// client calls each by it (`api.getInvoiceById(id)`), so what still spells it
/// outside the description is stale or new. The rest of a schema is a contract
/// the answer lists.
fn operation_ids(state: &ScoredState, path: &str) -> Vec<String> {
    let mut ids: Vec<String> = state
        .hunk_texts
        .iter()
        .filter(|h| h.path == path)
        .flat_map(|h| h.added.iter().chain(&h.removed))
        .filter_map(|line| {
            let (_, rest) = line.split_once("operationId")?;
            let rest = rest.trim_start_matches(['"', '\'']).trim_start();
            let value = rest.strip_prefix(':')?.trim_start_matches([' ', '"', '\'']);
            let id: String = value
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
                .collect();
            (id.chars().count() >= 4).then_some(id)
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

/// What a changed config's lines spell that can witness a reader: words
/// (`maxPoolSize`, `DATABASE_URL`, `max-connections`, `synchronization`,
/// never `replicas`) and values (`up{job="demo"}`, `ghcr.io/acme/web:1.2`).
/// Only a structured config has keys and values; a template's or a
/// stylesheet's words are prose and selectors.
fn changed_keys(state: &ScoredState, path: &str, nameless: &[Nameless]) -> Vec<String> {
    if let Some(n) = nameless.iter().find(|n| n.path == path) {
        return n.terms.clone();
    }
    if is_schema_file(path) {
        return operation_ids(state, path);
    }
    if !is_config_file(path) {
        return Vec::new();
    }
    let mut keys: Vec<String> = state
        .hunk_texts
        .iter()
        .filter(|h| h.path == path)
        .flat_map(|h| h.added.iter().chain(&h.removed))
        .filter(|l| !commented(l))
        .flat_map(|l| super::keys::terms(l))
        .collect();
    let (removed, added): (Vec<&str>, Vec<&str>) = state
        .hunk_texts
        .iter()
        .filter(|h| h.path == path)
        .fold((Vec::new(), Vec::new()), |(mut removed, mut added), h| {
            removed.extend(
                h.removed
                    .iter()
                    .map(String::as_str)
                    .filter(|l| !commented(l)),
            );
            added.extend(h.added.iter().map(String::as_str).filter(|l| !commented(l)));
            (removed, added)
        });
    let kept = state
        .all_fragments
        .iter()
        .filter(|f| rel(state, f.path()) == path)
        .flat_map(|f| f.content.lines());
    keys.extend(super::keys::dropped_words(
        removed.into_iter(),
        added.into_iter().chain(kept),
    ));
    keys.extend(super::keys::dropped_definitions(state, path));
    // A term the file also spells on a line the change left alone was read from
    // it before: a file spelling it elsewhere is no reader of the change (#309).
    let untouched = super::keys::untouched_terms(state, path);
    keys.retain(|k| !untouched.contains(k));
    // The names of the definitions the change sits in are untouched lines, and the
    // names a reader spells.
    if let Some(text) = state
        .run
        .source()
        .read_to_string(&state.root_dir.join(path))
    {
        keys.extend(
            state
                .hunk_texts
                .iter()
                .filter(|h| h.path == path)
                .flat_map(|h| enclosing_names(&text, h.new_range().0)),
        );
    }
    keys.sort();
    keys.dedup();
    keys
}

/// The readers the typed key table finds for what the change rewrote in the
/// changed configs `untraced`: (changed file, `route /v1/orders/{}`, the
/// untouched file naming the same key, its line). Where a key has a type (an
/// environment variable, an image, a route, a table), the join knows what a
/// textual search cannot: spellings differ (`{id}` and `<int:id>`), and only
/// compatible roles meet.
fn typed_readers(state: &ScoredState, untraced: &[String]) -> Vec<(String, String, String, u32)> {
    let mut rows: Vec<(String, String, String, u32)> =
        diffctx_graph::keytable::counterparts_of_change(
            &state.all_fragments,
            Some(state.root_dir.as_path()),
            state.run.changed_lines(),
        )
        .into_iter()
        .map(|c| {
            (
                rel(state, &c.changed_path),
                format!("{} {}", c.kind, c.key),
                rel(state, &c.path),
                c.line.max(1) as u32,
            )
        })
        .filter(|(target, ..)| untraced.contains(target))
        .collect();
    rows.extend(
        diffctx_graph::keytable::runs_named_after(
            &state.all_fragments,
            Some(state.root_dir.as_path()),
            untraced,
        )
        .into_iter()
        .map(|c| {
            let key = format!("{} {}", c.kind, c.key);
            (
                c.changed_path,
                key,
                rel(state, &c.path),
                c.line.max(1) as u32,
            )
        }),
    );
    rows.sort();
    rows.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1 && a.2 == b.2);
    rows
}

/// Terms searched for one changed config, and for all of them: `git grep`
/// matches every needle against every line, so a diff of hundreds of configs
/// (an epoch's corpus cases and workflows) turned a 10 s answer into 330 s
/// (Forgejo #109). The smallest changes are searched first, a file's longest
/// terms first; a config left out stays "not followed" and is counted, and a
/// config with more terms than the cap says the run was limited.
const MAX_KEY_TERMS_PER_FILE: usize = 16;

const MAX_KEY_NEEDLES: usize = 96;

/// The lines outside the change that spell a key or a value the change
/// touched in a file outside code (#686, #609, Forgejo #109), and the files
/// the typed key table ties to it. The textual match is a search: a term too
/// common to witness a reader is not searched, a search past its caps leaves
/// the rest of the configs unsearched (the last count returned), and a file
/// none of whose terms could be searched stays `content_untraced`.
pub(super) fn key_references(
    state: &ScoredState,
    cells: &Cells,
    untraced: &[String],
    nameless: &[Nameless],
) -> (
    Vec<PathReference>,
    usize,
    Vec<KeySearch>,
    usize,
    Vec<UnanalysedFile>,
) {
    if !state.run.check() {
        return (Vec::new(), 0, Vec::new(), 0, Vec::new());
    }
    let typed = typed_readers(state, untraced);
    // A record a change adds to a directory of records is read by what reads
    // its siblings' keys; only code reads them, a sibling is a peer.
    let added: FxHashSet<&str> = state
        .hunk_texts
        .iter()
        .filter(|h| h.old_start == 0 && h.old_len == 0)
        .map(|h| h.path.as_str())
        .collect();
    let tree = std::cell::LazyCell::new(|| {
        read_or_note(state, state.run.source().list_paths(&state.root_dir))
    });
    let mut schema: FxHashSet<(String, String)> = FxHashSet::default();
    let mut candidates: Vec<(usize, &String, Vec<String>, Vec<String>)> = untraced
        .iter()
        .enumerate()
        .map(|(index, path)| {
            let readers: Vec<String> = typed
                .iter()
                .filter(|(target, ..)| target == path)
                .map(|(_, key, ..)| key.clone())
                .collect();
            let mut words = changed_keys(state, path, nameless);
            if added.contains(path.as_str()) {
                for key in super::keys::schema_keys(state, path, &tree) {
                    if !words.contains(&key) {
                        schema.insert((path.clone(), key.clone()));
                        words.push(key);
                    }
                }
            }
            (index, path, words, readers)
        })
        .filter(|(_, _, words, readers)| !words.is_empty() || !readers.is_empty())
        .collect();
    candidates.sort_by(|a, b| (a.2.len(), a.1).cmp(&(b.2.len(), b.1)));
    let mut searched: FxHashSet<String> = FxHashSet::default();
    let mut unsearched = 0;
    let mut chosen: Vec<(usize, String, Vec<String>)> = Vec::new();
    for (index, path, mut words, mut readers) in candidates {
        words.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        if words.len() > MAX_KEY_TERMS_PER_FILE {
            state
                .run
                .note(diffctx_snapshot::resource::LimitReason::CandidateLimit);
            words.truncate(MAX_KEY_TERMS_PER_FILE);
        }
        let fresh = words.iter().filter(|w| !searched.contains(*w)).count();
        if searched.len() + fresh > MAX_KEY_NEEDLES {
            if readers.is_empty() {
                unsearched += 1;
                continue;
            }
            words.clear();
        }
        searched.extend(words.iter().cloned());
        words.append(&mut readers);
        words.sort();
        words.dedup();
        chosen.push((index, path.clone(), words));
    }
    chosen.sort_by_key(|(index, ..)| *index);
    let keys: Vec<(String, Vec<String>)> = chosen
        .into_iter()
        .map(|(_, path, terms)| (path, terms))
        .collect();
    let mut needles: Vec<String> = searched.into_iter().collect();
    needles.sort();
    if keys.is_empty() {
        return (Vec::new(), 0, Vec::new(), unsearched, Vec::new());
    }
    let spelled = cells.spelled_lines(state, &needles).unwrap_or_else(|e| {
        tracing::warn!(
            "impact: the head's files could not be searched ({e}); the answer is unknown"
        );
        state
            .run
            .note(diffctx_snapshot::resource::LimitReason::ReadFailed);
        decide::cells::Spelled::default()
    });
    let changed: FxHashSet<String> = changed_rels(state).collect();
    let source = state.run.source();
    // A file that could not be read or tokenised may spell a key and nothing says
    // it does not: the answer names it as unanalysed instead of staying silent.
    let unread: Vec<UnanalysedFile> = spelled
        .unread
        .iter()
        .filter(|path| {
            !changed.contains(*path) && class_of(&state.root_dir, &source, path, "", None).reads()
        })
        .map(|path| UnanalysedFile::new(path.clone(), "too_large", Vec::new()))
        .collect();
    let wanted: FxHashSet<&str> = needles.iter().map(String::as_str).collect();
    let phrases: Vec<&String> = needles
        .iter()
        .filter(|n| super::keys::is_phrase(n))
        .collect();
    let mut by_key: FxHashMap<String, Vec<(String, u32)>> = FxHashMap::default();
    for (path, line, text) in state.run.bounded(&spelled.rows) {
        let class = class_of(&state.root_dir, &source, path, text, None);
        let unknown = class == MentionClass::Data
            && super::mentions::unclassified(&state.root_dir, &source, path)
            && !super::mentions::commented_in(path, text);
        if changed.contains(path) || !(class.reads() || unknown) {
            continue;
        }
        let spelled = super::keys::words(text)
            .filter(|w| wanted.contains(w.as_str()))
            .chain(
                phrases
                    .iter()
                    .filter(|p| names_word(text, p.as_str()))
                    .map(|p| p.to_string()),
            );
        let mut seen: FxHashSet<String> = FxHashSet::default();
        for word in spelled {
            if seen.insert(word.clone()) {
                let rows = by_key.entry(word).or_default();
                if !rows.iter().any(|(p, _)| p == path) {
                    rows.push((path.clone(), *line));
                }
            }
        }
    }
    for (_, key, path, line) in &typed {
        let rows = by_key.entry(key.clone()).or_default();
        if !rows.iter().any(|(p, _)| p == path) {
            rows.push((path.clone(), *line));
        }
    }
    let mut refs: Vec<PathReference> = Vec::new();
    let mut searches: Vec<KeySearch> = Vec::new();
    let mut omitted = 0;
    let mut claimed: FxHashSet<&str> = FxHashSet::default();
    // A sibling record spells its collection's keys as a peer, not a reader.
    let rows_of = |target: &String, key: &String| -> Vec<(String, u32)> {
        let peers_left_out = schema.contains(&(target.clone(), key.clone()));
        let mut rows: Vec<(String, u32)> = by_key
            .get(key)
            .into_iter()
            .flatten()
            .filter(|(path, _)| {
                !peers_left_out
                    || class_of(&state.root_dir, &source, path, "", None) == MentionClass::Code
            })
            .cloned()
            .collect();
        rows.sort();
        rows
    };
    for (target, target_keys) in &keys {
        let (searchable, too_common): (Vec<&String>, Vec<&String>) = target_keys
            .iter()
            .partition(|k| rows_of(target, k).len() <= MAX_UNTRACED_NAMERS);
        // A term too many files spell witnesses no reader: the answer says the
        // search left it out, it never reads as nothing spelling it.
        if !too_common.is_empty() {
            unsearched += 1;
        }
        if searchable.is_empty() {
            continue;
        }
        let mut named = 0;
        let mut listed: FxHashSet<(String, u32)> = FxHashSet::default();
        for key in &searchable {
            if !claimed.insert(key.as_str()) {
                continue;
            }
            for (path, line) in rows_of(target, *key) {
                if !listed.insert((path.clone(), line)) {
                    continue;
                }
                named += 1;
                if named > MAX_NAMED_BY_PER_TARGET || refs.len() >= MAX_NAMED_BY {
                    omitted += 1;
                    continue;
                }
                let class = class_of(&state.root_dir, &source, &path, "", None);
                refs.push(PathReference {
                    path,
                    line,
                    target: target.clone(),
                    key: Some((*key).clone()),
                    class,
                });
            }
        }
        searches.push(KeySearch {
            path: target.clone(),
            keys: searchable.into_iter().cloned().collect(),
            named,
        });
    }
    (refs, omitted, searches, unsearched, unread)
}

/// Astro's content API: a page reads the entries of a collection by its
/// name. The first argument is the name, or an object or array of them
/// with a `collection` key; anything else computes it.
static COLLECTION_CALL: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
    regex::Regex::new(
        r#"\b(?:getCollection|getEntry|getEntries|getEntryBySlug|getDataEntryById)\s*\(\s*(?:\[\s*)?(?:\{[^}]*?\bcollection\s*:\s*)?(?:'([^'$]*)'|"([^"$]*)"|`([^`$]*)`|([^\s)]))?"#,
    )
    .expect("collection call regex")
});

/// The lines outside the change that read the content collection of a
/// changed entry by name (#432). An entry under `src/content/<name>/` of a
/// directory holding an `astro.config.*` is read by `getCollection('<name>')`
/// and its kin, never imported or named by path, so its readers were a clean
/// zero. A call whose name is computed may read it and is listed so.
/// Read off the lines: an `.astro` page has no grammar here, and a file
/// that has one must hold the call in code, not in a string or comment.
pub(super) fn read_by_collection(
    state: &ScoredState,
    diff_range: Option<&str>,
) -> (Vec<CollectionRead>, usize) {
    let paths = read_or_note(state, state.run.source().list_paths(&state.root_dir));
    let projects: Vec<&str> = paths
        .iter()
        .filter_map(|p| {
            let (dir, file) = p.rsplit_once('/').unwrap_or(("", p));
            file.starts_with("astro.config.").then_some(dir)
        })
        .collect();
    let content_of = |project: &str| {
        if project.is_empty() {
            "src/content/".to_string()
        } else {
            format!("{project}/src/content/")
        }
    };
    let project_of = |path: &str| {
        projects
            .iter()
            .copied()
            .filter(|p| p.is_empty() || path.starts_with(&format!("{p}/")))
            .max_by_key(|p| p.len())
    };
    let changed: FxHashSet<(&str, String)> = changed_rels(state)
        .chain(state.deleted_files.iter().cloned())
        .filter_map(|path| {
            let project = project_of(&path)?;
            let (collection, _) = path.strip_prefix(&content_of(project))?.split_once('/')?;
            Some((project, collection.to_string()))
        })
        .collect();
    if changed.is_empty() {
        return (Vec::new(), 0);
    }
    let head = head_of(diff_range);
    let grepped = read_or_note(
        state,
        diffctx_snapshot::grep::grep_lines_matching(
            &state.root_dir,
            &state.run.source(),
            r"(getCollection|getEntry|getEntries|getEntryBySlug|getDataEntryById)[[:space:]]*\(",
            head.as_deref(),
            &[],
        ),
    );
    let written: FxHashSet<(&str, &str)> = state
        .hunk_texts
        .iter()
        .flat_map(|h| h.added.iter().map(move |l| (h.path.as_str(), l.trim())))
        .collect();
    let source = state.run.source();
    let mut texts: FxHashMap<&str, Option<String>> = FxHashMap::default();
    let (mut found, mut omitted) = (Vec::new(), 0);
    for (path, line, text) in state.run.bounded(&grepped.rows) {
        let Some(project) = project_of(path) else {
            continue;
        };
        if is_document(path) || commented(text) || written.contains(&(path.as_str(), text.trim())) {
            continue;
        }
        let Some(file) = texts
            .entry(path)
            .or_insert_with(|| source.read_to_string(&state.root_dir.join(path)))
        else {
            continue;
        };
        let lines: Vec<&str> = file.lines().collect();
        let code = code_text(path, file);
        let code_line = code.lines().nth(*line as usize - 1).unwrap_or("");
        // A call split over lines names its collection on the next one.
        let call = match lines.get(*line as usize) {
            Some(next) => format!("{text} {next}"),
            None => text.clone(),
        };
        for m in COLLECTION_CALL.captures_iter(&call) {
            // A call in a string or a comment reads nothing, and one on the
            // next line is that line's own.
            let at = m.get(0).map_or(0, |m| m.start());
            if at >= text.len() || code_line.get(at..at + 3) != text.get(at..at + 3) {
                continue;
            }
            let named = (1..=3)
                .find_map(|i| m.get(i))
                .map(|n| n.as_str().to_string());
            let reads = match &named {
                Some(c) => changed.contains(&(project, c.clone())),
                None => m.get(4).is_some() && changed.iter().any(|(p, _)| *p == project),
            };
            if !reads {
                continue;
            }
            if found.len() >= MAX_NAMED_BY {
                omitted += 1;
                continue;
            }
            found.push(CollectionRead {
                path: path.clone(),
                line: *line,
                collection: named,
            });
        }
    }
    (found, omitted)
}
