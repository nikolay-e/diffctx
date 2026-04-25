use std::path::Path;

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::mode::ScoringMode;
use crate::pipeline;
use crate::render::{DiffContextOutput, FragmentEntry};

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
    #[pyo3(get)]
    pub content: Option<String>,
}

#[pymethods]
impl PyFragment {
    fn to_dict<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        dict.set_item("path", &self.path)?;
        dict.set_item("lines", &self.lines)?;
        dict.set_item("kind", &self.kind)?;
        if let Some(ref s) = self.symbol {
            dict.set_item("symbol", s)?;
        }
        if let Some(ref c) = self.content {
            dict.set_item("content", c)?;
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
    diff_range = None,
    budget_tokens = None,
    alpha = 0.60,
    tau = 0.08,
    no_content = false,
    full = false,
    scoring_mode = "hybrid",
    timeout = 300,
))]
fn build_diff_context_native(
    root_dir: &str,
    diff_range: Option<&str>,
    budget_tokens: Option<u32>,
    alpha: f64,
    tau: f64,
    no_content: bool,
    full: bool,
    scoring_mode: &str,
    timeout: u64,
) -> PyResult<DiffContextResult> {
    let mode = ScoringMode::from_str(scoring_mode);
    let path = Path::new(root_dir);

    pipeline::build_diff_context(
        path,
        diff_range,
        budget_tokens,
        alpha,
        tau,
        no_content,
        full,
        mode,
        timeout,
    )
    .map(DiffContextResult::from)
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
}

#[pyfunction]
#[pyo3(signature = (
    root_dir,
    diff_range,
    budget_tokens = None,
    alpha = 0.60,
    tau = 0.08,
    no_content = false,
    ignore_file = None,
    no_default_ignores = false,
    full = false,
    whitelist_file = None,
    scoring_mode = "hybrid",
    timeout = 300,
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

    let mode = ScoringMode::from_str(scoring_mode);
    let path = Path::new(root_dir);
    let range = if diff_range.is_empty() {
        None
    } else {
        Some(diff_range)
    };

    let start = std::time::Instant::now();
    let output = pipeline::build_diff_context(
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
    .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
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
            frag_dict.set_item("content", c)?;
        }
        frag_list.append(frag_dict)?;
    }
    dict.set_item("fragments", frag_list)?;

    let latency = PyDict::new(py);
    if let Some(ref lb) = output.latency {
        latency.set_item(
            "fragmentation_ms",
            (lb.fragmentation_ms * 10.0).round() / 10.0,
        )?;
        latency.set_item("discovery_ms", (lb.discovery_ms * 10.0).round() / 10.0)?;
        latency.set_item(
            "tokenization_ms",
            (lb.tokenization_ms * 10.0).round() / 10.0,
        )?;
        latency.set_item(
            "scoring_selection_ms",
            (lb.scoring_selection_ms * 10.0).round() / 10.0,
        )?;
        latency.set_item("total_ms", (lb.total_ms * 10.0).round() / 10.0)?;
    } else {
        latency.set_item("total_ms", (total_ms * 10.0).round() / 10.0)?;
    }
    dict.set_item("latency", latency)?;

    Ok(dict)
}

#[pymodule]
pub fn _diffctx(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(build_diff_context, m)?)?;
    m.add_function(wrap_pyfunction!(build_diff_context_native, m)?)?;
    m.add_class::<DiffContextResult>()?;
    m.add_class::<PyFragment>()?;
    Ok(())
}
