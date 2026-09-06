use std::path::Path;
use std::sync::Arc;

use pyo3::create_exception;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::config::limits::{
    DEFAULT_PIPELINE_TIMEOUT_SECONDS, DEFAULT_PPR_ALPHA, DEFAULT_SCORING,
    DEFAULT_STOPPING_THRESHOLD,
};
use crate::git::GitError as RustGitError;
use crate::mode::ScoringMode;
use crate::pipeline::{self, ScoredState};
use crate::render::DiffContextOutput;

#[pyclass(unsendable)]
pub struct PyScoredState {
    inner: Arc<ScoredState>,
}

create_exception!(_diffctx, GitError, pyo3::exceptions::PyException);

// Raised when the per-run compute deadline expires. It subclasses the builtin
// `TimeoutError`, so `except TimeoutError` catches it without importing
// anything from this module.
create_exception!(
    _diffctx,
    ComputeTimeoutError,
    pyo3::exceptions::PyTimeoutError
);

/// Runs a compute phase off the GIL and converts an expired deadline back into
/// an ordinary Python error.
///
/// The deadline fires as a panic (see `deadline::Deadline`: the phases it
/// guards run deep inside call chains that do not return `Result`), which
/// pyo3 would otherwise surface as `pyo3_runtime.PanicException` — a
/// `BaseException` no caller catches by accident, and under the old
/// `panic = "abort"` release profile not an exception at all but SIGABRT for
/// the whole interpreter. Any other panic is re-raised unchanged: this
/// converts the one outcome that is routine, not every bug.
fn detach_guarded<T: Send>(
    py: Python<'_>,
    work: impl FnOnce() -> anyhow::Result<T> + Send,
) -> PyResult<T> {
    let outcome =
        py.detach(
            move || match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
                Ok(result) => Ok(result),
                Err(payload) => match crate::deadline::deadline_panic_message(payload.as_ref()) {
                    Some(message) => Err(message),
                    None => std::panic::resume_unwind(payload),
                },
            },
        );
    match outcome {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(map_pipeline_err(e)),
        Err(message) => Err(ComputeTimeoutError::new_err(message)),
    }
}

/// `--mode locate` (#126): same pipeline and selection as pack mode, rendered
/// as the compact `diffctx.locate.v1` JSON string (ranked items + provenance
/// reasons, no source bodies).
#[pyfunction]
#[pyo3(signature = (
    root_dir,
    diff_range,
    budget_tokens = None,
    alpha = DEFAULT_PPR_ALPHA,
    tau = DEFAULT_STOPPING_THRESHOLD,
    scoring_mode = DEFAULT_SCORING,
    timeout = DEFAULT_PIPELINE_TIMEOUT_SECONDS,
))]
fn build_locate(
    py: Python<'_>,
    root_dir: &str,
    diff_range: &str,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: f64,
    scoring_mode: &str,
    timeout: u64,
) -> PyResult<String> {
    let mode =
        ScoringMode::from_str(scoring_mode).map_err(pyo3::exceptions::PyValueError::new_err)?;
    let path = Path::new(root_dir).to_path_buf();
    // Empty means "the working tree", exactly as in `build_diff_context`.
    // Forwarding `Some("")` instead reached `validate_diff_range`, which
    // rejects it — so the two entry points into the same pipeline disagreed
    // about what an unspecified range means.
    let range = if diff_range.is_empty() {
        None
    } else {
        Some(diff_range.to_string())
    };
    let output = detach_guarded(py, move || {
        crate::pipeline::build_diff_context_locate(
            &path,
            range.as_deref(),
            budget_tokens,
            alpha,
            tau,
            mode,
            timeout,
        )
    })?;
    serde_json::to_string(&output)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

fn map_pipeline_err(e: anyhow::Error) -> PyErr {
    if let Some(git_err) = e.downcast_ref::<RustGitError>() {
        return GitError::new_err(git_err.to_string());
    }
    pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
}

#[pyfunction]
#[pyo3(signature = (
    root_dir,
    diff_range,
    budget_tokens = None,
    alpha = DEFAULT_PPR_ALPHA,
    tau = DEFAULT_STOPPING_THRESHOLD,
    no_content = false,
    full = false,
    scoring_mode = DEFAULT_SCORING,
    timeout = DEFAULT_PIPELINE_TIMEOUT_SECONDS,
))]
fn build_diff_context<'py>(
    py: Python<'py>,
    root_dir: &str,
    diff_range: &str,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: f64,
    no_content: bool,
    full: bool,
    scoring_mode: &str,
    timeout: u64,
) -> PyResult<Bound<'py, PyDict>> {
    let mode =
        ScoringMode::from_str(scoring_mode).map_err(pyo3::exceptions::PyValueError::new_err)?;
    let path = Path::new(root_dir);
    let range = if diff_range.is_empty() {
        None
    } else {
        Some(diff_range)
    };

    let start = std::time::Instant::now();
    let output = detach_guarded(py, || {
        pipeline::build_diff_context(
            path,
            range,
            budget_tokens,
            alpha,
            tau,
            no_content,
            full,
            mode,
            timeout,
        )
    })?;
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;

    diff_context_output_to_dict(py, &output, Some(total_ms))
}

#[pyfunction]
#[pyo3(signature = (
    root_dir,
    diff_range,
    alpha = DEFAULT_PPR_ALPHA,
    scoring_mode = DEFAULT_SCORING,
    timeout = DEFAULT_PIPELINE_TIMEOUT_SECONDS,
))]
fn compute_scored_state(
    py: Python<'_>,
    root_dir: &str,
    diff_range: &str,
    alpha: f64,
    scoring_mode: &str,
    timeout: u64,
) -> PyResult<PyScoredState> {
    let mode =
        ScoringMode::from_str(scoring_mode).map_err(pyo3::exceptions::PyValueError::new_err)?;
    let path = Path::new(root_dir);
    let range = if diff_range.is_empty() {
        None
    } else {
        Some(diff_range)
    };
    let state = detach_guarded(py, || {
        pipeline::compute_scored_state(path, range, alpha, mode, timeout)
    })?;
    Ok(PyScoredState {
        inner: Arc::new(state),
    })
}

#[pyfunction]
#[pyo3(signature = (
    state,
    budget_tokens = None,
    tau = DEFAULT_STOPPING_THRESHOLD,
    no_content = false,
))]
fn select_with_params<'py>(
    py: Python<'py>,
    state: &PyScoredState,
    budget_tokens: Option<u32>,
    tau: f64,
    no_content: bool,
) -> PyResult<Bound<'py, PyDict>> {
    let inner = state.inner.clone();
    let output = detach_guarded(py, move || {
        if inner.all_fragments.is_empty() {
            // Same deletion/rename honesty as the CLI path: a deletion-only
            // diff still reports its file lists instead of a bare skeleton.
            return Ok(pipeline::empty_output_from_state(&inner));
        }
        Ok(pipeline::select_with_params(
            &inner,
            budget_tokens,
            tau,
            no_content,
        ))
    })?;
    diff_context_output_to_dict(py, &output, None)
}

/// The ONE place a `DiffContextOutput` becomes a Python dict.
///
/// `build_diff_context` used to inline a second, character-for-character copy
/// of this. That is how `pre_phase_ms` came to be missing from both call sites
/// at once (#183): a field added to the struct has to be repeated by hand, and
/// nothing makes the two copies agree. The only behavioural difference between
/// them was the fallback below, so it became a parameter rather than a reason
/// to keep the fork.
///
/// `fallback_total_ms` is used only when the pipeline reported no latency block
/// at all — the caller's own wall-clock reading, so the dict still carries a
/// `total_ms` rather than an empty `latency`.
fn diff_context_output_to_dict<'py>(
    py: Python<'py>,
    output: &DiffContextOutput,
    fallback_total_ms: Option<f64>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("name", &output.name)?;
    dict.set_item("type", "diff_context")?;
    if let Some(ref msg) = output.commit_message {
        dict.set_item("commit_message", msg)?;
    }
    if !output.changed_files.is_empty() {
        dict.set_item("changed_files", &output.changed_files)?;
    }
    if !output.deleted_files.is_empty() {
        dict.set_item("deleted_files", &output.deleted_files)?;
    }
    if !output.lockfile_changes.is_empty() {
        dict.set_item("lockfile_changes", &output.lockfile_changes)?;
    }
    if !output.ignored_changes.is_empty() {
        dict.set_item("ignored_changes", &output.ignored_changes)?;
    }
    if output.policy_excluded_count > 0 {
        dict.set_item("policy_excluded_count", output.policy_excluded_count)?;
    }
    if !output.renamed_files.is_empty() {
        let renames = PyList::empty(py);
        for (from, to) in &output.renamed_files {
            let pair = PyDict::new(py);
            pair.set_item("from", from)?;
            pair.set_item("to", to)?;
            renames.append(pair)?;
        }
        dict.set_item("renamed_files", renames)?;
    }
    dict.set_item("fragment_count", output.fragment_count)?;

    let frag_list = PyList::empty(py);
    for entry in &output.fragments {
        let frag_dict = PyDict::new(py);
        frag_dict.set_item("path", &entry.path)?;
        frag_dict.set_item("lines", &entry.lines)?;
        if let Some(ref role) = entry.role {
            frag_dict.set_item("role", role)?;
        }
        frag_dict.set_item("kind", &entry.kind)?;
        if let Some(ref s) = entry.symbol {
            frag_dict.set_item("symbol", s)?;
        }
        if let Some(ref c) = entry.content {
            frag_dict.set_item("content", c.as_ref())?;
        }
        frag_list.append(frag_dict)?;
    }
    dict.set_item("fragments", frag_list)?;

    let latency = PyDict::new(py);
    if let Some(ref lb) = output.latency {
        let r = |v: f64| (v * 10.0).round() / 10.0;
        latency.set_item("pre_phase_ms", r(lb.pre_phase_ms))?;
        latency.set_item("parse_changed_ms", r(lb.parse_changed_ms))?;
        latency.set_item("universe_walk_ms", r(lb.universe_walk_ms))?;
        latency.set_item("discovery_ms", r(lb.discovery_ms))?;
        latency.set_item("parse_discovered_ms", r(lb.parse_discovered_ms))?;
        latency.set_item("tokenization_ms", r(lb.tokenization_ms))?;
        latency.set_item("graph_build_ms", r(lb.graph_build_ms))?;
        latency.set_item("scoring_selection_ms", r(lb.scoring_selection_ms))?;
        latency.set_item("total_ms", r(lb.total_ms))?;
        latency.set_item("scoring_ms", r(lb.scoring_ms))?;
        latency.set_item("selection_ms", r(lb.selection_ms))?;
        latency.set_item("candidate_count", lb.candidate_count)?;
        latency.set_item("edge_count", lb.edge_count)?;
        latency.set_item("greedy_iters", lb.greedy_iters)?;
        latency.set_item("edges_before_cap", lb.edges_before_cap)?;
        latency.set_item("edges_dropped_by_cap", lb.edges_dropped_by_cap)?;
        latency.set_item("nodes_capped", lb.nodes_capped)?;
        latency.set_item("max_out_edges_per_node", lb.max_out_edges_per_node)?;
        latency.set_item("ppr_truncated", lb.ppr_truncated)?;
        latency.set_item("ppr_forward_pushes", lb.ppr_forward_pushes)?;
        latency.set_item("ppr_backward_pushes", lb.ppr_backward_pushes)?;
        latency.set_item("stopping_certificate", lb.stopping_certificate)?;
        latency.set_item("peak_rss_bytes", lb.peak_rss_bytes)?;
        let emissions = PyDict::new(py);
        for &(category, raw, deduped) in &lb.edge_emissions_by_category {
            let counts = PyDict::new(py);
            counts.set_item("raw", raw)?;
            counts.set_item("deduped", deduped)?;
            emissions.set_item(category, counts)?;
        }
        latency.set_item("edge_emissions_by_category", emissions)?;
    } else if let Some(total) = fallback_total_ms {
        latency.set_item("total_ms", (total * 10.0).round() / 10.0)?;
    }
    dict.set_item("latency", latency)?;
    Ok(dict)
}

#[pyfunction]
#[pyo3(signature = (root_dir, diff_range, timeout = DEFAULT_PIPELINE_TIMEOUT_SECONDS))]
fn get_raw_diff_text(
    py: Python<'_>,
    root_dir: &str,
    diff_range: &str,
    timeout: u64,
) -> PyResult<String> {
    let range = if diff_range.is_empty() {
        None
    } else {
        Some(diff_range)
    };
    detach_guarded(py, || {
        pipeline::raw_diff_text(Path::new(root_dir), range, timeout)
    })
}

/// The commit a duration spec (`24h`, `8d`, `1h30m`) resolves to, or the range
/// verbatim when it is an ordinary revision. Exported so the Python CLI phrases
/// its messages in terms of the same window the pipeline actually diffed,
/// instead of restating the duration grammar on its own.
#[pyfunction]
fn resolve_diff_range(py: Python<'_>, root_dir: &str, diff_range: &str) -> PyResult<String> {
    let root = Path::new(root_dir).to_path_buf();
    let range = (!diff_range.is_empty()).then(|| diff_range.to_string());
    // Spawns `git`: holding the GIL across a subprocess blocks every other
    // thread in the interpreter for its whole duration, which is exactly the
    // wall-clock the latency columns are supposed to attribute to diffctx.
    let resolved = py
        .detach(move || crate::git::resolve_duration_range(&root, range.as_deref()))
        .map_err(|e| GitError::new_err(e.to_string()))?;
    Ok(resolved.range.unwrap_or_default())
}

#[pyfunction]
fn get_language_for_file(path: &str) -> Option<String> {
    crate::languages::get_language_for_file(path).map(|s| s.to_string())
}

/// The engine's secret-path policy, for tree mode and the MCP tools (#227).
#[pyfunction]
fn is_secret_path(path: &str) -> bool {
    crate::pipeline::is_secret_path(Path::new(path))
}

/// Which of `rel_paths` the engine withholds — secret by name or ignored as
/// git resolves it — so the MCP fetch refuses exactly what selection refuses
/// (#228). Off the GIL: it spawns `git check-ignore`.
#[pyfunction]
fn withheld_paths(py: Python<'_>, root_dir: &str, rel_paths: Vec<String>) -> Vec<String> {
    let root = Path::new(root_dir).to_path_buf();
    py.detach(move || crate::pipeline::withheld_paths(&root, &rel_paths))
}

#[pyfunction]
fn count_tokens(py: Python<'_>, text: &str) -> PyResult<u32> {
    let owned = text.to_string();
    py.detach(move || crate::tokenizer::try_count_tokens(&owned))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

// -- Project graph + analytics + export (Python-facing wrappers) -------------

use rustc_hash::{FxHashMap as RsFxHashMap, FxHashSet as RsFxHashSet};

use crate::analytics;
use crate::graph::EdgeCategory;
use crate::graph_export;
use crate::project_graph;

#[pyclass]
pub struct PyProjectGraph {
    inner: project_graph::ProjectGraph,
    fragment_map: RsFxHashMap<crate::types::FragmentId, crate::types::Fragment>,
}

impl PyProjectGraph {
    fn view(&self) -> graph_export::ProjectGraphView<'_> {
        graph_export::ProjectGraphView {
            graph: &self.inner.graph,
            fragments: &self.fragment_map,
            root_dir: Some(self.inner.root_dir.as_path()),
        }
    }
}

#[pymethods]
impl PyProjectGraph {
    #[getter]
    fn fragment_count(&self) -> usize {
        self.inner.fragments.len()
    }

    #[getter]
    fn node_count(&self) -> usize {
        self.inner.graph.node_count()
    }

    #[getter]
    fn edge_count(&self) -> usize {
        self.inner.graph.edge_count()
    }

    #[getter]
    fn root_dir(&self) -> String {
        self.inner.root_dir.to_string_lossy().into_owned()
    }

    fn __repr__(&self) -> String {
        format!(
            "ProjectGraph(fragments={}, nodes={}, edges={})",
            self.inner.fragments.len(),
            self.inner.graph.node_count(),
            self.inner.graph.edge_count(),
        )
    }
}

#[pyclass]
pub struct PyQuotientGraph {
    inner: analytics::QuotientGraph,
}

#[pymethods]
impl PyQuotientGraph {
    #[getter]
    fn node_count(&self) -> usize {
        self.inner.nodes.len()
    }

    #[getter]
    fn edge_count(&self) -> usize {
        self.inner.edges.len()
    }
}

#[pyclass(skip_from_py_object)]
#[derive(Clone)]
pub struct PyModuleMetrics {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub cohesion: f64,
    #[pyo3(get)]
    pub coupling: f64,
    #[pyo3(get)]
    pub instability: f64,
    #[pyo3(get)]
    pub fan_in: u32,
    #[pyo3(get)]
    pub fan_out: u32,
}

fn parse_edge_categories(types: Option<Vec<String>>) -> Option<RsFxHashSet<EdgeCategory>> {
    types.map(|v| v.iter().map(|s| EdgeCategory::from_str(s)).collect())
}

fn parse_quotient_level(level: &str) -> analytics::QuotientLevel {
    analytics::QuotientLevel::from_str(level)
}

#[pyfunction]
#[pyo3(signature = (root_dir))]
fn build_project_graph(py: Python<'_>, root_dir: &str) -> PyResult<PyProjectGraph> {
    let root = std::path::Path::new(root_dir).to_path_buf();
    // Walks the repository and parses every file with tree-sitter. Held under
    // the GIL this was the heaviest single-threaded stall the extension could
    // impose on its host, and it made harness wall-clock unusable as a
    // product number (#245).
    let pg = py
        .detach(move || project_graph::build_project_graph(&root))
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
    let fragment_map: RsFxHashMap<_, _> = pg
        .fragments
        .iter()
        .map(|f| (f.id.clone(), f.clone()))
        .collect();
    Ok(PyProjectGraph {
        inner: pg,
        fragment_map,
    })
}

#[pyfunction]
#[pyo3(signature = (pg, top=10, edge_types=None))]
fn hotspots<'py>(
    py: Python<'py>,
    pg: &PyProjectGraph,
    top: usize,
    edge_types: Option<Vec<String>>,
) -> PyResult<Vec<(String, f64, Bound<'py, PyDict>)>> {
    let cats = parse_edge_categories(edge_types);
    let root = pg.inner.root_dir.to_str();
    let entries = analytics::hotspots(
        &pg.inner.graph,
        &pg.inner.fragments,
        top,
        root,
        cats.as_ref(),
    );
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let details = PyDict::new(py);
        details.set_item("out_degree", entry.out_degree)?;
        out.push((entry.path.to_string(), entry.score, details));
    }
    Ok(out)
}

#[pyfunction]
#[pyo3(signature = (pg, level="directory", edge_types=None))]
fn coupling_metrics(
    pg: &PyProjectGraph,
    level: &str,
    edge_types: Option<Vec<String>>,
) -> Vec<PyModuleMetrics> {
    let level = parse_quotient_level(level);
    let cats = parse_edge_categories(edge_types);
    let root = pg.inner.root_dir.to_str();
    analytics::coupling_metrics(
        &pg.inner.graph,
        &pg.inner.fragments,
        level,
        root,
        cats.as_ref(),
    )
    .into_iter()
    .map(|m| PyModuleMetrics {
        name: m.name.to_string(),
        cohesion: m.cohesion,
        coupling: m.coupling,
        instability: m.instability,
        fan_in: m.fan_in,
        fan_out: m.fan_out,
    })
    .collect()
}

#[pyfunction]
#[pyo3(signature = (pg, level="directory"))]
fn quotient_graph(pg: &PyProjectGraph, level: &str) -> PyQuotientGraph {
    let level = parse_quotient_level(level);
    let root = pg.inner.root_dir.to_str();
    let qg = analytics::quotient_graph(&pg.inner.graph, &pg.inner.fragments, level, root);
    PyQuotientGraph { inner: qg }
}

#[pyfunction]
#[pyo3(signature = (qg, top_n=50))]
fn to_mermaid(qg: &PyQuotientGraph, top_n: usize) -> String {
    analytics::to_mermaid(&qg.inner, top_n)
}

#[pyfunction]
fn graph_to_json_string(pg: &PyProjectGraph) -> PyResult<String> {
    let view = pg.view();
    graph_export::graph_to_json_string(&view)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction]
fn graph_to_graphml_string(pg: &PyProjectGraph) -> String {
    let view = pg.view();
    graph_export::graph_to_graphml_string(&view)
}

#[pyfunction]
#[pyo3(signature = (pg, top_n=10))]
fn graph_summary<'py>(
    py: Python<'py>,
    pg: &PyProjectGraph,
    top_n: usize,
) -> PyResult<Bound<'py, PyDict>> {
    let view = pg.view();
    let summary = graph_export::graph_summary(&view, top_n);
    let dict = PyDict::new(py);
    dict.set_item("node_count", summary.node_count)?;
    dict.set_item("edge_count", summary.edge_count)?;
    dict.set_item("file_count", summary.file_count)?;
    dict.set_item("density", summary.density)?;
    let etc = PyDict::new(py);
    for (k, v) in &summary.edge_type_counts {
        etc.set_item(k, *v)?;
    }
    dict.set_item("edge_type_counts", etc)?;
    let top = PyList::empty(py);
    for entry in &summary.top_in_degree {
        let item = PyDict::new(py);
        item.set_item("label", &entry.label)?;
        item.set_item("in_degree", entry.in_degree)?;
        top.append(item)?;
    }
    dict.set_item("top_in_degree", top)?;
    Ok(dict)
}

#[pymodule]
pub fn _diffctx(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(build_diff_context, m)?)?;
    m.add_function(wrap_pyfunction!(build_locate, m)?)?;
    m.add_function(wrap_pyfunction!(compute_scored_state, m)?)?;
    m.add_function(wrap_pyfunction!(select_with_params, m)?)?;
    m.add_class::<PyScoredState>()?;
    m.add_function(wrap_pyfunction!(get_raw_diff_text, m)?)?;
    m.add_function(wrap_pyfunction!(resolve_diff_range, m)?)?;
    m.add_function(wrap_pyfunction!(get_language_for_file, m)?)?;
    m.add_function(wrap_pyfunction!(count_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(is_secret_path, m)?)?;
    m.add_function(wrap_pyfunction!(withheld_paths, m)?)?;
    m.add_function(wrap_pyfunction!(build_project_graph, m)?)?;
    m.add_function(wrap_pyfunction!(hotspots, m)?)?;
    m.add_function(wrap_pyfunction!(coupling_metrics, m)?)?;
    m.add_function(wrap_pyfunction!(quotient_graph, m)?)?;
    m.add_function(wrap_pyfunction!(to_mermaid, m)?)?;
    m.add_function(wrap_pyfunction!(graph_to_json_string, m)?)?;
    m.add_function(wrap_pyfunction!(graph_to_graphml_string, m)?)?;
    m.add_function(wrap_pyfunction!(graph_summary, m)?)?;
    m.add_class::<PyProjectGraph>()?;
    m.add_class::<PyQuotientGraph>()?;
    m.add_class::<PyModuleMetrics>()?;
    // The shipped defaults, exported so the Python layers read them instead of
    // restating them. `cli.py` and `mcp/server.py` each carried their own
    // `_DEFAULT_TAU = 0.12` — the layering contract forbids mcp importing cli,
    // and the answer to that was a copy. Reading them from the extension keeps
    // the layering and removes the copy, which is how 0.12/0.08/0.05 drifted
    // apart across the harnesses in the first place.
    m.add("DEFAULT_TAU", DEFAULT_STOPPING_THRESHOLD)?;
    m.add("DEFAULT_ALPHA", DEFAULT_PPR_ALPHA)?;
    m.add(
        "DEFAULT_CORE_BUDGET_FRACTION",
        crate::config::selection::DEFAULT_CORE_BUDGET_FRACTION,
    )?;
    m.add("DEFAULT_SCORING", DEFAULT_SCORING)?;
    m.add("DEFAULT_TIMEOUT", DEFAULT_PIPELINE_TIMEOUT_SECONDS)?;
    // Same reason as the constants above: the Python CLI enumerated the accepted
    // --scoring values in its own literal and fell out of step the moment a mode
    // was added, so `pit` parsed everywhere except the two CLIs.
    m.add("SCORING_MODES", crate::mode::SCORING_MODE_NAMES.to_vec())?;
    m.add("GitError", m.py().get_type::<GitError>())?;
    m.add(
        "ComputeTimeoutError",
        m.py().get_type::<ComputeTimeoutError>(),
    )?;
    Ok(())
}
