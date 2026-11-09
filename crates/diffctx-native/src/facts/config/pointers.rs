//! The second table: which field of which tool's file names a place. Kustomize
//! `resources`, `bases`, `components` and `patches`, Argo CD `source.path`, Flux
//! `spec.path`, Helmfile `releases[].chart` and `values`, a chart's
//! `dependencies[].repository`. A tool joins by a row, with a fixture that reads
//! it.
//!
//! Rows are by kind. A Kustomize file, a Helmfile and a chart carry no `kind`
//! (the tools default it), so [`document_kind`] names them by the shape their
//! tool reads.

use serde::{Deserialize, Serialize};

use crate::facts::Span;

use super::pointer::{PointerKind, SourcePointer};
use super::tree::{FieldPath, Node};

struct Row {
    kind: &'static str,
    path: &'static str,
    pointer: PointerKind,
    /// `key=path`: the place is what follows the `=` (a generator's `files`).
    after_equals: bool,
}

const fn row(kind: &'static str, path: &'static str, pointer: PointerKind) -> Row {
    Row {
        kind,
        path,
        pointer,
        after_equals: false,
    }
}

const fn assigned(kind: &'static str, path: &'static str) -> Row {
    Row {
        kind,
        path,
        pointer: PointerKind::Path,
        after_equals: true,
    }
}

static ROWS: &[Row] = &[
    // Kustomize: `resources` takes a file or a directory with a kustomization;
    // generators, transformers and validators take a path or a URL.
    row("Kustomization", "resources[]", PointerKind::Path),
    row("Kustomization", "bases[]", PointerKind::Path),
    row("Kustomization", "components[]", PointerKind::Path),
    row("Kustomization", "configurations[]", PointerKind::Path),
    row("Kustomization", "crds[]", PointerKind::Path),
    row("Kustomization", "generators[]", PointerKind::Path),
    row("Kustomization", "transformers[]", PointerKind::Path),
    row("Kustomization", "validators[]", PointerKind::Path),
    row("Kustomization", "patches[].path", PointerKind::Path),
    row("Kustomization", "patchesJson6902[].path", PointerKind::Path),
    row(
        "Kustomization",
        "patchesStrategicMerge[]",
        PointerKind::Path,
    ),
    row(
        "Kustomization",
        "helmCharts[].valuesFile",
        PointerKind::Path,
    ),
    assigned("Kustomization", "configMapGenerator[].files[]"),
    row(
        "Kustomization",
        "configMapGenerator[].envs[]",
        PointerKind::Path,
    ),
    assigned("Kustomization", "secretGenerator[].files[]"),
    row(
        "Kustomization",
        "secretGenerator[].envs[]",
        PointerKind::Path,
    ),
    // Flux's Kustomization has the same kind: its `spec.path` is a directory of
    // the source.
    row("Kustomization", "spec.path", PointerKind::Path),
    // Argo CD.
    row("Application", "spec.source.path", PointerKind::Path),
    row("Application", "spec.sources[].path", PointerKind::Path),
    row(
        "ApplicationSet",
        "spec.template.spec.source.path",
        PointerKind::Path,
    ),
    // Helmfile.
    row("Helmfile", "releases[].chart", PointerKind::Chart),
    row("Helmfile", "releases[].values[]", PointerKind::Path),
    row("Helmfile", "bases[]", PointerKind::Path),
    row("Helmfile", "helmfiles[]", PointerKind::Path),
    row("Helmfile", "helmfiles[].path", PointerKind::Path),
    // A chart.
    row("Chart", "dependencies[].repository", PointerKind::Chart),
];

/// Keys that make a document without a `kind` a Kustomization, as kustomize
/// itself defaults it.
const KUSTOMIZE_KEYS: &[&str] = &[
    "resources",
    "bases",
    "components",
    "patches",
    "patchesStrategicMerge",
    "patchesJson6902",
    "configMapGenerator",
    "secretGenerator",
    "generators",
    "transformers",
    "helmCharts",
];

/// What tool's file a document is: its `kind`, or for a file that has none the
/// shape its tool reads (`Kustomization`, `Helmfile`, `Chart`).
pub fn document_kind(doc: &Node) -> Option<&str> {
    if let Some(kind) = doc.get("kind").and_then(Node::as_str) {
        return Some(kind);
    }
    if KUSTOMIZE_KEYS.iter().any(|key| doc.get(key).is_some()) {
        return Some("Kustomization");
    }
    if doc.get("releases").and_then(Node::as_seq).is_some() {
        return Some("Helmfile");
    }
    let chart_version = doc
        .get("apiVersion")
        .and_then(Node::as_str)
        .is_some_and(|version| matches!(version, "v1" | "v2"));
    (chart_version && doc.get("name").is_some() && doc.get("version").is_some()).then_some("Chart")
}

/// The places a document names, in the order of the table. A text that cannot be
/// a path (an inline patch with a line break, an empty one) is no pointer.
pub fn pointers_of(doc: &Node) -> Vec<SourcePointer> {
    let Some(kind) = document_kind(doc) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for row in ROWS.iter().filter(|row| row.kind == kind) {
        let path = FieldPath::parse(row.path).expect("a row's path is a field path");
        for hit in doc.select(&path) {
            let Some(text) = hit.node.as_str() else {
                continue;
            };
            let text = if row.after_equals {
                text.split_once('=').map_or(text, |(_, place)| place)
            } else {
                text
            };
            if text.trim().is_empty() || text.contains('\n') {
                continue;
            }
            found.push(SourcePointer {
                site: hit.node.span.clone(),
                kind: row.pointer,
                text: text.to_string(),
                templated: hit.node.templated,
            });
        }
    }
    found
}

/// A release of a chart: where the chart is, and the values files the release
/// gives it. A values file means something only with the chart it is given to.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Release {
    pub site: Span,
    pub name: Option<String>,
    /// The chart's directory (or its name in a repository, which no file holds).
    pub chart: Option<SourcePointer>,
    pub values: Vec<SourcePointer>,
    /// The values files are written relative to the chart (Argo CD's
    /// `valueFiles`), not to the file that holds the release (Helmfile's).
    pub values_from_chart: bool,
}

fn string_pointers(node: &Node, kind: PointerKind) -> Vec<SourcePointer> {
    node.as_seq()
        .unwrap_or(&[])
        .iter()
        .filter_map(|item| {
            let text = item.as_str()?;
            (!text.trim().is_empty() && !text.contains('\n')).then(|| SourcePointer {
                site: item.span.clone(),
                kind,
                text: text.to_string(),
                templated: item.templated,
            })
        })
        .collect()
}

fn pointer_of(node: &Node, kind: PointerKind) -> Option<SourcePointer> {
    let text = node.as_str()?;
    (!text.trim().is_empty()).then(|| SourcePointer {
        site: node.span.clone(),
        kind,
        text: text.to_string(),
        templated: node.templated,
    })
}

/// The releases a document states: a Helmfile's `releases`, an Argo CD
/// Application's `source` and `sources` (and an ApplicationSet's template).
pub fn releases_of(doc: &Node) -> Vec<Release> {
    let mut releases = Vec::new();
    match document_kind(doc) {
        Some("Helmfile") => {
            for release in doc.get("releases").and_then(Node::as_seq).unwrap_or(&[]) {
                releases.push(Release {
                    site: release.span.clone(),
                    name: release
                        .get("name")
                        .and_then(Node::as_str)
                        .map(str::to_string),
                    chart: release
                        .get("chart")
                        .and_then(|chart| pointer_of(chart, PointerKind::Chart)),
                    values: release.get("values").map_or_else(Vec::new, |values| {
                        string_pointers(values, PointerKind::Path)
                    }),
                    values_from_chart: false,
                });
            }
        }
        Some(kind @ ("Application" | "ApplicationSet")) => {
            let spec = if kind == "Application" {
                doc.get("spec")
            } else {
                doc.at("spec.template.spec")
            };
            let Some(spec) = spec else {
                return releases;
            };
            let mut sources: Vec<&Node> = Vec::new();
            sources.extend(spec.get("source"));
            sources.extend(spec.get("sources").and_then(Node::as_seq).unwrap_or(&[]));
            for source in sources {
                let chart = source.get("path").and_then(|path| {
                    // Relative to the root of the source repository.
                    pointer_of(path, PointerKind::Path).map(|mut pointer| {
                        pointer.text = format!("/{}", pointer.text.trim_start_matches('/'));
                        pointer
                    })
                });
                releases.push(Release {
                    site: source.span.clone(),
                    name: None,
                    chart,
                    values: source
                        .at("helm.valueFiles")
                        .map_or_else(Vec::new, |files| string_pointers(files, PointerKind::Path)),
                    values_from_chart: true,
                });
            }
        }
        _ => {}
    }
    releases
}
