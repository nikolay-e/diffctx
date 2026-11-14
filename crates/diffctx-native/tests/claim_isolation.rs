//! What a claim says comes from the cells and the token index, never from an old
//! edge builder: the answer, impact and the decision read no path into
//! `crate::edges` and ask the ranking graph nothing, which stay the ranking's alone (`facts` and `resolve` are held
//! tighter by their own isolation tests).

use std::fs;
use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

mod isolation_source;
use isolation_source::without_comments;

const CLAIM_PATHS: [&str; 5] = [
    "src/impact.rs",
    "src/impact",
    "src/decide.rs",
    "src/answer.rs",
    "src/answer",
];

/// What the ranking graph answers to, which only the ranking may ask.
const GRAPH_READS: &str = r"\b(?:for_each_reverse_neighbor|for_each_categorized_edge|edge_category|forward_edge_weight|is_forward|cap_stats)\b|\bscoring_result\s*\.\s*graph\b|\bcrate\s*::\s*graph\b";

fn edge_paths(source: &str) -> Vec<String> {
    let source = without_comments(source);
    let graph = Regex::new(GRAPH_READS).expect("a pattern");
    let direct = Regex::new(r"\b(?:crate|_diffctx|diffctx|super)\s*::\s*edges\b|\bedges\s*::")
        .expect("a pattern");
    let grouped = Regex::new(r"\b(?:crate|_diffctx|diffctx)\s*::\s*\{[^}]*\bedges\b[^}]*\}")
        .expect("a pattern");
    direct
        .find_iter(&source)
        .chain(grouped.find_iter(&source))
        .chain(graph.find_iter(&source))
        .map(|hit| hit.as_str().to_string())
        .collect()
}

#[test]
fn a_claim_reads_no_edge_builder() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = 0;
    let mut reaches = Vec::new();
    for claim in CLAIM_PATHS {
        for entry in WalkDir::new(manifest.join(claim)) {
            let entry = entry.expect("a directory entry");
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            files += 1;
            let text = fs::read_to_string(path).expect("a source file");
            for hit in edge_paths(&text) {
                reaches.push(format!("{}: {hit}", path.display()));
            }
        }
    }
    assert!(files >= 8, "the walk found {files} claim files");
    assert!(
        reaches.is_empty(),
        "a claim reaches the edges: {reaches:#?}"
    );
}

#[test]
fn a_path_into_the_edges_is_a_violation() {
    assert_eq!(edge_paths("use crate::edges::base;").len(), 1);
    assert_eq!(edge_paths("use crate::{edges, git};").len(), 1);
    assert_eq!(
        edge_paths("let x = edges::collect_capped_edges();").len(),
        1
    );
    assert_eq!(edge_paths("use super::edges::EdgeDict;").len(), 1);
    assert!(edge_paths("// crate::edges::base\nlet edges = Vec::new(); edges.len();").is_empty());
    assert!(edge_paths("use crate::keytable::counterparts_of_change;").is_empty());
    assert_eq!(
        edge_paths("graph.for_each_categorized_edge(|a, b, c| {});").len(),
        1
    );
    assert_eq!(edge_paths("let g = &state.scoring_result.graph;").len(), 1);
    assert_eq!(edge_paths("use crate::graph::EdgeCategory;").len(), 1);
}
