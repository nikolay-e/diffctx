use std::path::Path;
use std::sync::Arc;

use pyo3::create_exception;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::config::limits::{
    DEFAULT_PIPELINE_TIMEOUT_SECONDS, DEFAULT_PPR_ALPHA, DEFAULT_STOPPING_THRESHOLD,
};
use crate::git::GitError as RustGitError;
use crate::mode::ScoringMode;
use crate::pipeline::{self, ScoredState};
use crate::render::{DiffContextOutput, FragmentEntry};

#[pyclass(unsendable)]
pub struct PyScoredState {
    inner: Arc<ScoredState>,
}

create_exception!(_diffctx, GitError, pyo3::exceptions::PyException);

fn map_pipeline_err(e: anyhow::Error) -> PyErr {
    if let Some(git_err) = e.downcast_ref::<RustGitError>() {
        return GitError::new_err(git_err.to_string());
    }
    pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
}

#[pyclass]
#[derive(Clone)]
pub struct PyFragment {
    #[pyo3(get)]
    pub path: String,
    #[pyo3(get)]
    pub lines: String,
    #[pyo3(get)]
    pub kind: String,
    #[pyo3(get)]
    pub symbol: Option<String>,
    pub content: Option<Arc<str>>,
}

#[pymethods]
impl PyFragment {
    #[getter]
    fn content(&self) -> Option<&str> {
        self.content.as_deref()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("path", &self.path)?;
        dict.set_item("lines", &self.lines)?;
        dict.set_item("kind", &self.kind)?;
        if let Some(ref s) = self.symbol {
            dict.set_item("symbol", s)?;
        }
        if let Some(ref c) = self.content {
            dict.set_item("content", c.as_ref())?;
        }
        Ok(dict)
    }

    fn __repr__(&self) -> String {
        match &self.symbol {
            Some(s) => format!("PyFragment({} {} {})", self.path, self.kind, s),
            None => format!("PyFragment({} {} {})", self.path, self.kind, self.lines),
        }
    }
}

impl From<&FragmentEntry> for PyFragment {
    fn from(entry: &FragmentEntry) -> Self {
        Self {
            path: entry.path.clone(),
            lines: entry.lines.clone(),
            kind: entry.kind.clone(),
            symbol: entry.symbol.clone(),
            content: entry.content.clone(),
        }
    }
}

#[pyclass]
#[derive(Clone)]
pub struct DiffContextResult {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub fragment_count: usize,
    fragments: Vec<PyFragment>,
}

#[pymethods]
impl DiffContextResult {
    #[getter]
    fn fragments(&self) -> Vec<PyFragment> {
        self.fragments.clone()
    }

    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("name", &self.name)?;
        dict.set_item("type", "diff_context")?;
        dict.set_item("fragment_count", self.fragment_count)?;

        let frag_list = PyList::empty(py);
        for frag in &self.fragments {
            frag_list.append(frag.to_dict(py)?)?;
        }
        dict.set_item("fragments", frag_list)?;

        Ok(dict)
    }

    fn to_yaml(&self) -> String {
        let output = self.to_serializable();
        serde_yaml::to_string(&output).unwrap_or_default()
    }

    fn to_json(&self) -> String {
        let output = self.to_serializable();
        serde_json::to_string_pretty(&output).unwrap_or_default()
    }

    fn __len__(&self) -> usize {
        self.fragment_count
    }

    fn __repr__(&self) -> String {
        format!(
            "DiffContextResult(name='{}', fragments={})",
            self.name, self.fragment_count
        )
    }

    fn __iter__(slf: PyRef<'_, Self>) -> FragmentIterator {
        FragmentIterator {
            fragments: slf.fragments.clone(),
            index: 0,
        }
    }
}

impl DiffContextResult {
    fn to_serializable(&self) -> DiffContextOutput {
        DiffContextOutput {
            name: self.name.clone(),
            output_type: "diff_context".to_string(),
            fragment_count: self.fragment_count,
            fragments: self
                .fragments
                .iter()
                .map(|f| FragmentEntry {
                    path: f.path.clone(),
                    lines: f.lines.clone(),
                    kind: f.kind.clone(),
                    symbol: f.symbol.clone(),
                    content: f.content.clone(),
                })
                .collect(),
            latency: None,
        }
    }
}

impl From<DiffContextOutput> for DiffContextResult {
    fn from(output: DiffContextOutput) -> Self {
        let fragments: Vec<PyFragment> = output.fragments.iter().map(PyFragment::from).collect();
        Self {
            name: output.name,
            fragment_count: output.fragment_count,
            fragments,
        }
    }
}

#[pyclass]
pub struct FragmentIterator {
    fragments: Vec<PyFragment>,
    index: usize,
}

#[pymethods]
impl FragmentIterator {
    fn __iter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    fn __next__(&mut self) -> Option<PyFragment> {
        if self.index < self.fragments.len() {
            let frag = self.fragments[self.index].clone();
            self.index += 1;
            Some(frag)
        } else {
            None
        }
    }
}

#[pyfunction]
#[pyo3(signature = (
    root_dir,
    diff_range,
    budget_tokens = None,
    alpha = DEFAULT_PPR_ALPHA,
    tau = DEFAULT_STOPPING_THRESHOLD,
    no_content = false,
    ignore_file = None,
    no_default_ignores = false,
    full = false,
    whitelist_file = None,
    scoring_mode = "ego",
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
    ignore_file: Option<&str>,
    no_default_ignores: bool,
    full: bool,
    whitelist_file: Option<&str>,
    scoring_mode: &str,
    timeout: u64,
) -> PyResult<Bound<'py, PyDict>> {
    if ignore_file.is_some() {
        tracing::warn!("ignore_file is not yet implemented in Rust backend, ignored");
    }
    if no_default_ignores {
        tracing::warn!("no_default_ignores is not yet implemented in Rust backend, ignored");
    }
    if whitelist_file.is_some() {
        tracing::warn!("whitelist_file is not yet implemented in Rust backend, ignored");
    }

    let mode =
        ScoringMode::from_str(scoring_mode).map_err(pyo3::exceptions::PyValueError::new_err)?;
    let path = Path::new(root_dir);
    let range = if diff_range.is_empty() {
        None
    } else {
        Some(diff_range)
    };

    let start = std::time::Instant::now();
    let output = py
        .allow_threads(|| {
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
        })
        .map_err(map_pipeline_err)?;
    let total_ms = start.elapsed().as_secs_f64() * 1000.0;

    let dict = PyDict::new(py);
    dict.set_item("name", &output.name)?;
    dict.set_item("type", "diff_context")?;
    dict.set_item("fragment_count", output.fragment_count)?;

    let frag_list = PyList::empty(py);
    for entry in &output.fragments {
        let frag_dict = PyDict::new(py);
        frag_dict.set_item("path", &entry.path)?;
        frag_dict.set_item("lines", &entry.lines)?;
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
        latency.set_item("parse_changed_ms", r(lb.parse_changed_ms))?;
        latency.set_item("universe_walk_ms", r(lb.universe_walk_ms))?;
        latency.set_item("discovery_ms", r(lb.discovery_ms))?;
        latency.set_item("parse_discovered_ms", r(lb.parse_discovered_ms))?;
        latency.set_item("tokenization_ms", r(lb.tokenization_ms))?;
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
    } else {
        latency.set_item("total_ms", (total_ms * 10.0).round() / 10.0)?;
    }
    dict.set_item("latency", latency)?;

    Ok(dict)
}

#[pyfunction]
#[pyo3(signature = (
    root_dir,
    diff_range,
    alpha = DEFAULT_PPR_ALPHA,
    scoring_mode = "ego",
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
    let state = py
        .allow_threads(|| pipeline::compute_scored_state(path, range, alpha, mode, timeout))
        .map_err(map_pipeline_err)?;
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
    let output = py.allow_threads(move || {
        if inner.all_fragments.is_empty() {
            return DiffContextOutput {
                name: inner
                    .root_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| inner.root_dir.to_string_lossy().to_string()),
                output_type: "diff_context".to_string(),
                fragment_count: 0,
                fragments: Vec::new(),
                latency: None,
            };
        }
        pipeline::select_with_params(&inner, budget_tokens, tau, no_content)
    });
    diff_context_output_to_dict(py, &output)
}

fn diff_context_output_to_dict<'py>(
    py: Python<'py>,
    output: &DiffContextOutput,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("name", &output.name)?;
    dict.set_item("type", "diff_context")?;
    dict.set_item("fragment_count", output.fragment_count)?;

    let frag_list = PyList::empty(py);
    for entry in &output.fragments {
        let frag_dict = PyDict::new(py);
        frag_dict.set_item("path", &entry.path)?;
        frag_dict.set_item("lines", &entry.lines)?;
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
        latency.set_item("parse_changed_ms", r(lb.parse_changed_ms))?;
        latency.set_item("universe_walk_ms", r(lb.universe_walk_ms))?;
        latency.set_item("discovery_ms", r(lb.discovery_ms))?;
        latency.set_item("parse_discovered_ms", r(lb.parse_discovered_ms))?;
        latency.set_item("tokenization_ms", r(lb.tokenization_ms))?;
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
    }
    dict.set_item("latency", latency)?;
    Ok(dict)
}

#[pyfunction]
fn get_language_for_file(path: &str) -> Option<String> {
    crate::languages::get_language_for_file(path).map(|s| s.to_string())
}

#[pyfunction]
fn count_tokens(text: &str) -> u32 {
    crate::tokenizer::count_tokens(text)
}

// -- Project graph + analytics + export (Python-facing wrappers) -------------

use rustc_hash::{FxHashMap as RsFxHashMap, FxHashSet as RsFxHashSet};
use std::path::PathBuf;

use crate::analytics;
use crate::graph::EdgeCategory;
use crate::graph_export;
use crate::project_graph;

#[pyclass]
pub struct PyProjectGraph {
    inner: project_graph::ProjectGraph,
    fragment_map: RsFxHashMap<crate::types::FragmentId, crate::types::Fragment>,
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

#[pyclass]
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
fn build_project_graph(root_dir: &str) -> PyResult<PyProjectGraph> {
    let pg = project_graph::build_project_graph(std::path::Path::new(root_dir))
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
#[pyo3(signature = (pg, level="directory", edge_types=None))]
fn detect_cycles(
    pg: &PyProjectGraph,
    level: &str,
    edge_types: Option<Vec<String>>,
) -> Vec<Vec<String>> {
    let level = parse_quotient_level(level);
    let cats = parse_edge_categories(edge_types);
    let root = pg.inner.root_dir.to_str();
    analytics::detect_cycles(
        &pg.inner.graph,
        &pg.inner.fragments,
        level,
        root,
        cats.as_ref(),
    )
    .into_iter()
    .map(|cycle| cycle.into_iter().map(|s| s.to_string()).collect())
    .collect()
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
        None,
    );
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let details = PyDict::new(py);
        details.set_item("out_degree", entry.out_degree)?;
        details.set_item("churn", entry.churn)?;
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
    let view = graph_export::ProjectGraphView {
        graph: &pg.inner.graph,
        fragments: &pg.fragment_map,
        root_dir: Some(pg.inner.root_dir.as_path()),
    };
    graph_export::graph_to_json_string(&view)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction]
fn graph_to_graphml_string(pg: &PyProjectGraph) -> String {
    let view = graph_export::ProjectGraphView {
        graph: &pg.inner.graph,
        fragments: &pg.fragment_map,
        root_dir: Some(pg.inner.root_dir.as_path()),
    };
    graph_export::graph_to_graphml_string(&view)
}

#[pyfunction]
#[pyo3(signature = (pg, top_n=10))]
fn graph_summary<'py>(
    py: Python<'py>,
    pg: &PyProjectGraph,
    top_n: usize,
) -> PyResult<Bound<'py, PyDict>> {
    let view = graph_export::ProjectGraphView {
        graph: &pg.inner.graph,
        fragments: &pg.fragment_map,
        root_dir: Some(pg.inner.root_dir.as_path()),
    };
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
    m.add_function(wrap_pyfunction!(compute_scored_state, m)?)?;
    m.add_function(wrap_pyfunction!(select_with_params, m)?)?;
    m.add_class::<PyScoredState>()?;
    m.add_function(wrap_pyfunction!(get_language_for_file, m)?)?;
    m.add_function(wrap_pyfunction!(count_tokens, m)?)?;
    m.add_function(wrap_pyfunction!(build_project_graph, m)?)?;
    m.add_function(wrap_pyfunction!(detect_cycles, m)?)?;
    m.add_function(wrap_pyfunction!(hotspots, m)?)?;
    m.add_function(wrap_pyfunction!(coupling_metrics, m)?)?;
    m.add_function(wrap_pyfunction!(quotient_graph, m)?)?;
    m.add_function(wrap_pyfunction!(to_mermaid, m)?)?;
    m.add_function(wrap_pyfunction!(graph_to_json_string, m)?)?;
    m.add_function(wrap_pyfunction!(graph_to_graphml_string, m)?)?;
    m.add_function(wrap_pyfunction!(graph_summary, m)?)?;
    m.add_class::<DiffContextResult>()?;
    m.add_class::<PyFragment>()?;
    m.add_class::<PyProjectGraph>()?;
    m.add_class::<PyQuotientGraph>()?;
    m.add_class::<PyModuleMetrics>()?;
    m.add("GitError", m.py().get_type::<GitError>())?;
    Ok(())
}

// silence "unused" warnings for fields used only via Python pyclass getters
#[allow(dead_code)]
fn _used(_: PathBuf) {}
