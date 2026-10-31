//! The impact and symbol corpus (case schema v1): a case under
//! `tests/cases/{impact,symbol}/<language>/` builds a real git repository,
//! runs the shipped binary on it and compares the answer with a set of typed
//! relations. `tests/cases/impact/SCHEMA.md` is the contract.

use std::path::Path;

pub mod case;
pub mod engine;
pub mod judge;
pub mod model;
pub mod report;

use case::Mode;

/// Language directories a case may sit in.
pub const LANGUAGES: &[&str] = &[
    "python",
    "typescript",
    "rust",
    "kotlin",
    "java",
    "swift",
    "go",
    "shell",
    "config",
    "web",
    "sql",
    "other",
];

/// What one case concluded; `failures` is empty for a green case. The caller
/// sets the report's state once the baseline has been consulted, then records it.
pub struct Outcome {
    pub failures: Vec<String>,
    pub disputed: bool,
    pub report: report::CaseReport,
}

/// Runs the case at `path`. `Err` is a case or harness error (a YAML that
/// does not parse, a fixture git refuses): never absorbed by the baseline.
pub fn evaluate(name: &str, path: &Path, mode: Mode, language: &str) -> Result<Outcome, String> {
    if !LANGUAGES.contains(&language) {
        return Err(format!(
            "`{language}` is not a language directory (one of {})",
            LANGUAGES.join(", ")
        ));
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let case = case::parse(&raw, stem, mode)?;
    let compiled = case.expect.compile()?;
    if case.dispute.is_some() {
        return Ok(Outcome {
            failures: Vec::new(),
            disputed: true,
            report: report::CaseReport::new(name, &case, language),
        });
    }
    let answer = engine::run(&case, name)?;
    let (failures, report) = judge::judge(name, &case, &compiled, language, &answer);
    Ok(Outcome {
        failures,
        disputed: false,
        report,
    })
}
