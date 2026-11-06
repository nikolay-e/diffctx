//! `DIFFCTX_ORACLE_REPORT=<path>`: one JSON record per case, for the
//! Tier x language x relation tables.

use std::sync::Mutex;

use serde::Serialize;

use super::case::{Case, IssueState};

#[derive(Debug, Clone, Serialize)]
pub struct TargetReport {
    pub target: String,
    pub relation: String,
    pub tp: usize,
    pub fp: usize,
    #[serde(rename = "fn")]
    pub fn_: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CaseReport {
    pub case: String,
    pub mode: &'static str,
    pub language: String,
    pub source: String,
    pub issue_state: Option<&'static str>,
    pub tags: Vec<String>,
    /// `pass`, `fail`, `baselined_red`, `stale_baseline` or `disputed`.
    pub state: &'static str,
    pub targets: Vec<TargetReport>,
    /// The class of each unknown the answer named, or its sentence when no
    /// class matches.
    pub unknowns: Vec<String>,
    pub empty: bool,
    pub disclosed: bool,
    pub absence_claims: Vec<String>,
    pub kind_mismatches: Vec<String>,
    pub failures: Vec<String>,
}

impl CaseReport {
    pub fn new(name: &str, case: &Case, language: &str) -> Self {
        Self {
            case: name.to_string(),
            mode: case.mode.name(),
            language: language.to_string(),
            source: case.source.clone(),
            issue_state: case.issue_state.map(|s| match s {
                IssueState::Open => "open",
                IssueState::Closed => "closed",
            }),
            tags: case.tags.clone(),
            state: "pass",
            targets: Vec::new(),
            unknowns: Vec::new(),
            empty: false,
            disclosed: false,
            absence_claims: Vec::new(),
            kind_mismatches: Vec::new(),
            failures: Vec::new(),
        }
    }
}

static RECORDS: Mutex<Vec<CaseReport>> = Mutex::new(Vec::new());

pub fn record(report: CaseReport) {
    if let Ok(mut records) = RECORDS.lock() {
        records.push(report);
    }
}

/// Writes the report when `DIFFCTX_ORACLE_REPORT` names a path.
pub fn write_if_requested() -> Result<(), String> {
    let Some(path) = std::env::var_os("DIFFCTX_ORACLE_REPORT") else {
        return Ok(());
    };
    let mut cases = RECORDS.lock().map_err(|e| e.to_string())?.clone();
    cases.sort_by(|a, b| a.case.cmp(&b.case));
    let document = serde_json::json!({
        "schema": "diffctx.oracle-report.v1",
        "cases": cases,
    });
    let text = serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("write {path:?}: {e}"))
}
