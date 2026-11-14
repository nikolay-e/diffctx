//! A Helm chart's templates bound to the values they read and the named templates
//! they include.
//!
//! A chart is the directory that holds a `Chart.yaml`; its templates are the
//! files under it. What a template reads of `.Values` is set by the values the
//! release gives the chart, which the repository states only where a release is
//! written down:
//!
//! - a values file a Helmfile release or an Argo CD application names with its
//!   chart is bound to that chart, and a read of a key it sets is `Resolved`,
//!   one binding per file that sets it (each is a release's value);
//! - the chart's own `values.yaml` is read by every release and overridden by
//!   some: a read of a key it sets is a `Candidate` (`NoRelease`), the nearest
//!   key above when the key itself is not set;
//! - a values file no release names and no chart holds is counted: it may be
//!   anyone's.
//!
//! A named template is global to a release: a template includes the ones defined
//! in its chart and the subcharts and parent charts the release holds, and no
//! other chart's.

use std::collections::{HashMap, HashSet};

use super::{
    Binding, CandidateReason, Evidence, EvidenceKind, Relation, Resolution, UnresolvedReason,
};
use crate::facts::config::{Resolved, SourcePointer, UseOf, directory_of};
use crate::facts::{DefKey, DefKind, Fact, FileFacts, Language, Name};

pub(super) struct Bound {
    pub(super) bindings: Vec<Binding>,
    /// Values files no release names and no chart holds as its own.
    pub(super) unbound_values: usize,
}

fn chart_of<'a>(path: &str, chart_dirs: &HashSet<&'a str>) -> Option<&'a str> {
    let mut dir = directory_of(path);
    loop {
        if let Some(found) = chart_dirs.get(dir) {
            return Some(*found);
        }
        if dir.is_empty() {
            return None;
        }
        dir = directory_of(dir);
    }
}

/// The outermost chart a chart is a subchart of: the one whose templates share
/// the named templates.
fn release_root<'a>(chart: &'a str, chart_dirs: &HashSet<&'a str>) -> &'a str {
    let parent = directory_of(chart);
    if parent.rsplit('/').next() == Some("charts") {
        if let Some(outer) = chart_dirs.get(directory_of(parent)) {
            return release_root(*outer, chart_dirs);
        }
    }
    chart
}

/// `a.b.c`, `a.b`, `a`: the key and the keys above it.
fn ancestry(path: &str) -> impl Iterator<Item = &str> {
    std::iter::successors(Some(path), |&p| p.rsplit_once('.').map(|(head, _)| head))
}

fn values_path(key: &DefKey) -> String {
    let mut parts: Vec<&str> = key.local.container.iter().map(Name::as_str).collect();
    parts.push(key.local.name.as_str());
    parts.join(".")
}

fn resolve_to_path(pointer: &SourcePointer, base: &str) -> Option<String> {
    match pointer.resolve(base) {
        Resolved::Local(path) | Resolved::Rooted(path) => Some(path),
        Resolved::External | Resolved::Computed | Resolved::Escapes => None,
    }
}

pub(super) fn bind(files: &[FileFacts], paths: &HashSet<&str>) -> Bound {
    let chart_dirs: HashSet<&str> = paths
        .iter()
        .copied()
        .filter(|path| path.rsplit('/').next() == Some("Chart.yaml"))
        .map(directory_of)
        .collect();

    // The releases: which values files a chart is given.
    let mut bound: HashMap<String, HashSet<String>> = HashMap::new();
    for file in files {
        let base = directory_of(file.path.as_str());
        for fact in &file.blob.facts {
            let Fact::ConfigRelease(release) = fact else {
                continue;
            };
            let chart = release
                .chart
                .as_ref()
                .and_then(|pointer| resolve_to_path(pointer, base))
                .filter(|dir| chart_dirs.contains(dir.as_str()));
            let Some(chart) = chart else {
                continue;
            };
            let values_base = if release.values_from_chart {
                chart.as_str()
            } else {
                base
            };
            for pointer in &release.values {
                if let Some(path) =
                    resolve_to_path(pointer, values_base).filter(|p| paths.contains(p.as_str()))
                {
                    bound.entry(path).or_default().insert(chart.clone());
                }
            }
        }
    }
    let mut released: HashMap<&str, Vec<&str>> = HashMap::new();
    for (file, charts) in &bound {
        for chart in charts {
            released.entry(chart).or_default().push(file);
        }
    }

    // The values each file sets, and the named templates each defines.
    let mut by_file_path: HashMap<(&str, String), &DefKey> = HashMap::new();
    let mut defaults: HashMap<(&str, String), &DefKey> = HashMap::new();
    let mut defines: HashMap<(&str, &str), Vec<&DefKey>> = HashMap::new();
    let mut unbound_values = 0;
    let keys: Vec<(usize, DefKey, DefKind)> = files
        .iter()
        .enumerate()
        .flat_map(|(at, file)| {
            file.defs()
                .map(move |def| (at, file.key(&def.local), def.kind))
        })
        .collect();
    let mut values_files: HashSet<usize> = HashSet::new();
    for (at, key, kind) in &keys {
        let file = &files[*at];
        let path = file.path.as_str();
        match file.language() {
            Language::HelmValues => {
                values_files.insert(*at);
                let own = chart_of(path, &chart_dirs)
                    .filter(|chart| *chart == directory_of(path) && is_default_values(path));
                if let Some(chart) = own {
                    defaults.insert((chart, values_path(key)), key);
                }
                by_file_path.insert((path, values_path(key)), key);
            }
            Language::HelmTemplate => {
                if *kind == DefKind::Function && key.local.container.is_empty() {
                    let root = chart_of(path, &chart_dirs)
                        .map_or_else(|| directory_of(path), |c| release_root(c, &chart_dirs));
                    defines
                        .entry((root, key.local.name.as_str()))
                        .or_default()
                        .push(key);
                }
            }
            _ => {}
        }
    }
    for at in &values_files {
        let path = files[*at].path.as_str();
        let own = chart_of(path, &chart_dirs)
            .is_some_and(|chart| chart == directory_of(path) && is_default_values(path));
        if !own && !bound.contains_key(path) {
            unbound_values += 1;
        }
    }

    let mut bindings = Vec::new();
    for file in files {
        let path = file.path.as_str();
        for fact in &file.blob.facts {
            let Fact::ConfigUse(used) = fact else {
                continue;
            };
            let mut push = |relation: Relation, resolution: Resolution| {
                bindings.push(Binding {
                    file: file.path.clone(),
                    site: used.site.clone(),
                    from: used.enclosing.clone(),
                    relation,
                    name: Name::new(used.name.clone()),
                    resolution,
                    entry_guard: false,
                });
            };
            match used.of {
                UseOf::Values => {
                    let Some(chart) = chart_of(path, &chart_dirs) else {
                        continue;
                    };
                    let mut landed = false;
                    for source in released.get(chart).into_iter().flatten() {
                        if let Some(key) = by_file_path.get(&(*source, used.name.clone())) {
                            landed = true;
                            push(
                                Relation::Reads,
                                Resolution::Resolved {
                                    target: (*key).clone(),
                                    evidence: Evidence::new(EvidenceKind::SameScope),
                                },
                            );
                        }
                    }
                    let nearest = ancestry(&used.name)
                        .find_map(|prefix| defaults.get(&(chart, prefix.to_string())));
                    if let Some(key) = nearest {
                        landed = true;
                        push(
                            Relation::Reads,
                            Resolution::Candidate {
                                targets: vec![(*key).clone()],
                                reason: CandidateReason::NoRelease,
                            },
                        );
                    }
                    if !landed {
                        push(
                            Relation::Reads,
                            Resolution::Unresolved {
                                reason: UnresolvedReason::NoMatch,
                            },
                        );
                    }
                }
                UseOf::Include => {
                    let root = chart_of(path, &chart_dirs)
                        .map_or_else(|| directory_of(path), |c| release_root(c, &chart_dirs));
                    let found = defines
                        .get(&(root, used.name.as_str()))
                        .map_or(&[][..], Vec::as_slice);
                    let resolution = match found {
                        [] => Resolution::Unresolved {
                            reason: UnresolvedReason::NoBinding,
                        },
                        [only] => Resolution::Resolved {
                            target: (*only).clone(),
                            evidence: Evidence::new(EvidenceKind::SameScope),
                        },
                        several => Resolution::Candidate {
                            targets: several.iter().map(|key| (*key).clone()).collect(),
                            reason: CandidateReason::Overloaded,
                        },
                    };
                    push(Relation::Calls, resolution);
                }
            }
        }
    }
    Bound {
        bindings,
        unbound_values,
    }
}

/// A chart's own values: `values.yaml` or `values.yml`.
fn is_default_values(path: &str) -> bool {
    matches!(path.rsplit('/').next(), Some("values.yaml" | "values.yml"))
}
