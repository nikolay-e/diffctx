use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;

use crate::config::render::RENDER;
use crate::types::{Fragment, FragmentKind};

#[derive(Serialize)]
pub struct DiffContextOutput {
    pub name: String,
    #[serde(rename = "type")]
    pub output_type: String,
    pub fragment_count: usize,
    pub fragments: Vec<FragmentEntry>,
    #[serde(skip)]
    pub latency: Option<LatencyBreakdown>,
}

pub struct LatencyBreakdown {
    pub parse_changed_ms: f64,
    pub universe_walk_ms: f64,
    pub discovery_ms: f64,
    pub parse_discovered_ms: f64,
    pub tokenization_ms: f64,
    /// Combined scoring + selection time. Kept for backward
    /// compatibility with the existing checkpoint schema; the split
    /// values below are the new diagnostic signal.
    pub scoring_selection_ms: f64,
    pub total_ms: f64,
    /// Heavy-phase scoring only (PPR/EGO/BM25 + edge construction +
    /// graph build), excludes the selection stage.
    pub scoring_ms: f64,
    /// Selection stage only (lazy greedy / Boltzmann + post-passes).
    pub selection_ms: f64,
    /// Size of the candidate fragment universe handed to the scoring
    /// strategy (after fragment generation + signature variants but
    /// before per-strategy filtering). Surfaces blowup on large repos —
    /// pathological scoring time is correlated with this number, not
    /// with `fragment_count` (which is the *output* size after
    /// selection).
    pub candidate_count: usize,
    /// Edge count of the typed dependency graph used by PPR/EGO. Zero
    /// for BM25 mode (no graph built).
    pub edge_count: usize,
    /// Number of greedy iterations actually executed (selected non-core
    /// fragments). Bounded by `selected.len() - core.len()`. Pairs with
    /// `selection_ms` to spot lazy-heap blowup vs. genuine large output.
    pub greedy_iters: usize,
    /// Edge count after merge + hub suppression, before per-source cap.
    pub edges_before_cap: usize,
    /// Edges discarded by the per-source top-K cap.
    pub edges_dropped_by_cap: usize,
    /// Source nodes whose outgoing edge list was truncated by the cap.
    pub nodes_capped: usize,
    /// The K value applied for the per-source cap.
    pub max_out_edges_per_node: usize,
    /// PPR push iteration was truncated by `max_pushes_cap` before
    /// convergence. When true, `rel_scores` are biased toward seeds
    /// and absolute file_recall on this instance should be flagged
    /// in post-analysis. Always false for non-PPR scoring modes.
    pub ppr_truncated: bool,
    pub ppr_forward_pushes: usize,
    pub ppr_backward_pushes: usize,
}

#[derive(Serialize, Clone)]
pub struct FragmentEntry {
    pub path: String,
    pub lines: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Arc<str>>,
}

struct SymbolPatterns {
    function: Vec<Regex>,
    class: Vec<Regex>,
    r#struct: Vec<Regex>,
    interface: Vec<Regex>,
    r#enum: Vec<Regex>,
    r#impl: Vec<Regex>,
    r#type: Vec<Regex>,
    module: Vec<Regex>,
    section: Vec<Regex>,
}

static SYMBOL_PATTERNS: Lazy<SymbolPatterns> = Lazy::new(|| {
    SymbolPatterns {
    function: vec![
        Regex::new(r"(?m)^\s*(?:async\s+)?def\s+(\w+)\s*\(").unwrap(),
        Regex::new(r"(?m)^\s*(?:export\s+)?(?:async\s+)?function\s+(\w+)\s*[\(<]").unwrap(),
        Regex::new(r"(?m)^\s*(?:export\s+)?(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?(?:\([^)]*\)|\w)\s*=>").unwrap(),
        Regex::new(r"(?m)^func\s+(?:\([^)]+\)\s+)?(\w+)\s*[\(\[]").unwrap(),
        Regex::new(r"(?m)^\s*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*[\(<]").unwrap(),
        Regex::new(r"(?m)^\s*(?:(?:public|private|protected|static)\s+)*\w[\w<>\[\],]*\s+(\w+)\s*\(").unwrap(),
    ],
    class: vec![
        Regex::new(r"(?m)^\s*class\s+(\w+)\s*[:\({\s]").unwrap(),
        Regex::new(r"(?m)^\s*(?:export\s+)?(?:abstract\s+)?class\s+(\w+)").unwrap(),
    ],
    r#struct: vec![
        Regex::new(r"(?m)^\s*(?:pub\s+)?struct\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*type\s+(\w+)\s+struct\s*\{").unwrap(),
    ],
    interface: vec![
        Regex::new(r"(?m)^\s*(?:export\s+)?interface\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*type\s+(\w+)\s+interface\s*\{").unwrap(),
        Regex::new(r"(?m)^\s*(?:pub\s+)?trait\s+(\w+)").unwrap(),
    ],
    r#enum: vec![
        Regex::new(r"(?m)^\s*(?:pub\s+)?enum\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*class\s+(\w+)\s*\(.*Enum\)").unwrap(),
    ],
    r#impl: vec![
        Regex::new(r"(?m)^\s*impl(?:<[^>]+>)?\s+(\w+)").unwrap(),
    ],
    r#type: vec![
        Regex::new(r"(?m)^\s*(?:export\s+)?type\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*type\s+(\w+)\s").unwrap(),
    ],
    module: vec![
        Regex::new(r"(?m)^\s*(?:pub\s+)?mod\s+(\w+)").unwrap(),
        Regex::new(r"(?m)^\s*package\s+(\w+)").unwrap(),
    ],
    section: vec![
        Regex::new(r"(?m)^#{1,6}\s+(\S[^\n]*)$").unwrap(),
    ],
}
});

fn extract_symbol(frag: &Fragment) -> Option<String> {
    let patterns = match frag.kind {
        FragmentKind::Function | FragmentKind::FunctionSignature => &SYMBOL_PATTERNS.function,
        FragmentKind::Class | FragmentKind::ClassSignature => &SYMBOL_PATTERNS.class,
        FragmentKind::Struct | FragmentKind::StructSignature => &SYMBOL_PATTERNS.r#struct,
        FragmentKind::Interface | FragmentKind::InterfaceSignature => &SYMBOL_PATTERNS.interface,
        FragmentKind::Enum | FragmentKind::EnumSignature => &SYMBOL_PATTERNS.r#enum,
        FragmentKind::Impl => &SYMBOL_PATTERNS.r#impl,
        FragmentKind::Type => &SYMBOL_PATTERNS.r#type,
        FragmentKind::Module => &SYMBOL_PATTERNS.module,
        FragmentKind::Section => &SYMBOL_PATTERNS.section,
        _ => return None,
    };

    for pattern in patterns {
        if let Some(caps) = pattern.captures(&frag.content) {
            if let Some(m) = caps.get(1) {
                let result = m.as_str().trim();
                return Some(if frag.kind == FragmentKind::Section {
                    result
                        .chars()
                        .take(RENDER.section_symbol_max_chars)
                        .collect()
                } else {
                    result.to_string()
                });
            }
        }
    }
    None
}

fn get_relative_path(frag: &Fragment, repo_root: &Path) -> String {
    let frag_path = Path::new(frag.path());
    if !frag_path.is_absolute() {
        return frag_path.to_string_lossy().replace('\\', "/");
    }
    frag_path
        .strip_prefix(repo_root)
        .unwrap_or(frag_path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn create_fragment_entry(frag: &Fragment, path_str: &str) -> FragmentEntry {
    let symbol = frag.symbol_name.clone().or_else(|| extract_symbol(frag));
    let content = if frag.content.is_empty() {
        None
    } else {
        Some(Arc::clone(&frag.content))
    };

    FragmentEntry {
        path: path_str.to_string(),
        lines: format!("{}-{}", frag.start_line(), frag.end_line()),
        kind: frag.kind.as_str().to_string(),
        symbol,
        content,
    }
}

pub fn build_diff_context_output(
    repo_root: &Path,
    selected: &[Fragment],
    no_content: bool,
) -> DiffContextOutput {
    let mut by_path: BTreeMap<String, Vec<&Fragment>> = BTreeMap::new();
    for frag in selected {
        let rel_path = get_relative_path(frag, repo_root);
        by_path.entry(rel_path).or_default().push(frag);
    }

    let mut fragments_out: Vec<FragmentEntry> = Vec::new();
    for (rel_path, frags) in &by_path {
        let mut sorted_frags: Vec<&&Fragment> = frags.iter().collect();
        sorted_frags.sort_by_key(|f| f.start_line());
        for frag in sorted_frags {
            let mut entry = create_fragment_entry(frag, rel_path);
            if no_content {
                entry.content = None;
            }
            fragments_out.push(entry);
        }
    }

    let resolved = repo_root
        .canonicalize()
        .unwrap_or_else(|_| repo_root.to_path_buf());
    let name = resolved
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| resolved.to_string_lossy().to_string());

    DiffContextOutput {
        name,
        output_type: "diff_context".to_string(),
        fragment_count: fragments_out.len(),
        fragments: fragments_out,
        latency: None,
    }
}
