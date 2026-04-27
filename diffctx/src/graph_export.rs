use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustc_hash::FxHashMap;
use serde::Serialize;

use crate::graph::{EdgeCategory, Graph};
use crate::project_graph::ProjectGraph;
use crate::types::{Fragment, FragmentId};

pub struct ProjectGraphView<'a> {
    pub graph: &'a Graph,
    pub fragments: &'a FxHashMap<FragmentId, Fragment>,
    pub root_dir: Option<&'a Path>,
}

pub fn view_from_project_graph(pg: &ProjectGraph) -> (FxHashMap<FragmentId, Fragment>, &Path) {
    let map: FxHashMap<FragmentId, Fragment> = pg
        .fragments
        .iter()
        .map(|f| (f.id.clone(), f.clone()))
        .collect();
    (map, pg.root_dir.as_path())
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeRecord {
    pub id: String,
    pub label: String,
    pub path: String,
    pub lines: String,
    pub kind: String,
    pub symbol: String,
    pub token_count: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct EdgeRecord {
    pub source: String,
    pub source_symbol: String,
    pub target: String,
    pub target_symbol: String,
    pub weight: f64,
    pub category: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphDocument {
    pub name: String,
    #[serde(rename = "type")]
    pub doc_type: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub nodes: Vec<NodeRecord>,
    pub edges: Vec<EdgeRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GraphSummary {
    pub node_count: usize,
    pub edge_count: usize,
    pub file_count: usize,
    pub density: f64,
    pub edge_type_counts: FxHashMap<String, usize>,
    pub top_in_degree: Vec<TopInDegreeEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopInDegreeEntry {
    pub label: String,
    pub in_degree: usize,
}

fn relative_path(path: &str, root: Option<&Path>) -> String {
    let Some(root) = root else {
        return path.to_string();
    };
    let p = PathBuf::from(path);
    match p.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string(),
    }
}

fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
}

fn round4(value: f64) -> f64 {
    if value.is_finite() {
        (value * 10_000.0).round() / 10_000.0
    } else {
        value
    }
}

fn node_record(view: &ProjectGraphView<'_>, fid: &FragmentId, frag: &Fragment) -> NodeRecord {
    let rel_path = relative_path(&fid.path, view.root_dir);
    let loc = format!(
        "{}:{}-{}",
        file_name(&fid.path),
        fid.start_line,
        fid.end_line
    );
    let label = match frag.symbol_name.as_deref() {
        Some(name) if !name.is_empty() => format!("{name} ({loc})"),
        _ => loc.clone(),
    };
    NodeRecord {
        id: format!("{}:{}-{}", rel_path, fid.start_line, fid.end_line),
        label,
        path: rel_path,
        lines: format!("{}-{}", fid.start_line, fid.end_line),
        kind: frag.kind.as_str().to_string(),
        symbol: frag.symbol_name.clone().unwrap_or_default(),
        token_count: frag.token_count,
    }
}

fn edge_record(
    view: &ProjectGraphView<'_>,
    src: &FragmentId,
    dst: &FragmentId,
    weight: f64,
    category: EdgeCategory,
) -> EdgeRecord {
    let src_symbol = view
        .fragments
        .get(src)
        .and_then(|f| f.symbol_name.clone())
        .unwrap_or_default();
    let dst_symbol = view
        .fragments
        .get(dst)
        .and_then(|f| f.symbol_name.clone())
        .unwrap_or_default();
    EdgeRecord {
        source: format!(
            "{}:{}-{}",
            relative_path(&src.path, view.root_dir),
            src.start_line,
            src.end_line
        ),
        source_symbol: src_symbol,
        target: format!(
            "{}:{}-{}",
            relative_path(&dst.path, view.root_dir),
            dst.start_line,
            dst.end_line
        ),
        target_symbol: dst_symbol,
        weight: round4(weight),
        category: category.as_str().to_string(),
    }
}

fn collect_sorted_nodes<'a>(
    fragments: &'a FxHashMap<FragmentId, Fragment>,
) -> Vec<(&'a FragmentId, &'a Fragment)> {
    let mut entries: Vec<(&FragmentId, &Fragment)> = fragments.iter().collect();
    entries.sort_by(|a, b| {
        a.0.path
            .as_ref()
            .cmp(b.0.path.as_ref())
            .then(a.0.start_line.cmp(&b.0.start_line))
            .then(a.0.end_line.cmp(&b.0.end_line))
    });
    entries
}

fn collect_sorted_edges(graph: &Graph) -> Vec<(FragmentId, FragmentId, f64, EdgeCategory)> {
    let mut entries: Vec<(FragmentId, FragmentId, f64, EdgeCategory)> = graph
        .edge_categories
        .iter()
        .map(|((src, dst), cat)| {
            let weight = graph.forward_edge_weight(src, dst).unwrap_or(0.0);
            (src.clone(), dst.clone(), weight, *cat)
        })
        .collect();
    entries.sort_by(|a, b| {
        let key_a = format!("({}, {})", a.0, a.1);
        let key_b = format!("({}, {})", b.0, b.1);
        key_a.cmp(&key_b)
    });
    entries
}

pub fn graph_to_document(view: &ProjectGraphView<'_>) -> GraphDocument {
    let root_name = view
        .root_dir
        .and_then(|p| p.file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();

    let sorted_nodes = collect_sorted_nodes(view.fragments);
    let nodes: Vec<NodeRecord> = sorted_nodes
        .iter()
        .map(|(fid, frag)| node_record(view, fid, frag))
        .collect();

    let sorted_edges = collect_sorted_edges(view.graph);
    let edges: Vec<EdgeRecord> = sorted_edges
        .iter()
        .map(|(src, dst, weight, cat)| edge_record(view, src, dst, *weight, *cat))
        .collect();

    GraphDocument {
        name: root_name,
        doc_type: "project_graph".to_string(),
        node_count: view.graph.node_count(),
        edge_count: view.graph.edge_count(),
        nodes,
        edges,
    }
}

pub fn graph_to_json_string(view: &ProjectGraphView<'_>) -> Result<String, serde_json::Error> {
    let doc = graph_to_document(view);
    let mut out = serde_json::to_string_pretty(&doc)?;
    out.push('\n');
    Ok(out)
}

fn escape_graphml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

pub fn graph_to_graphml_string(view: &ProjectGraphView<'_>) -> String {
    let doc = graph_to_document(view);
    let mut out = String::new();

    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<graphml xmlns=\"http://graphml.graphdrawing.org/graphml\"\n");
    out.push_str("         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n");
    out.push_str(
        "         xsi:schemaLocation=\"http://graphml.graphdrawing.org/graphml \
http://graphml.graphdrawing.org/dtds/graphml.dtd\">\n",
    );

    out.push_str("  <key id=\"d_path\" for=\"node\" attr.name=\"path\" attr.type=\"string\"/>\n");
    out.push_str("  <key id=\"d_lines\" for=\"node\" attr.name=\"lines\" attr.type=\"string\"/>\n");
    out.push_str("  <key id=\"d_kind\" for=\"node\" attr.name=\"kind\" attr.type=\"string\"/>\n");
    out.push_str(
        "  <key id=\"d_symbol\" for=\"node\" attr.name=\"symbol\" attr.type=\"string\"/>\n",
    );
    out.push_str(
        "  <key id=\"d_tokens\" for=\"node\" attr.name=\"token_count\" attr.type=\"int\"/>\n",
    );
    out.push_str(
        "  <key id=\"d_weight\" for=\"edge\" attr.name=\"weight\" attr.type=\"double\"/>\n",
    );
    out.push_str(
        "  <key id=\"d_category\" for=\"edge\" attr.name=\"category\" attr.type=\"string\"/>\n",
    );
    out.push_str(&format!(
        "  <graph id=\"{}\" edgedefault=\"directed\">\n",
        escape_graphml(&doc.name)
    ));

    for node in &doc.nodes {
        let nid = escape_graphml(&node.id);
        out.push_str(&format!("    <node id=\"{nid}\">\n"));
        out.push_str(&format!(
            "      <data key=\"d_path\">{}</data>\n",
            escape_graphml(&node.path)
        ));
        out.push_str(&format!(
            "      <data key=\"d_lines\">{}</data>\n",
            escape_graphml(&node.lines)
        ));
        out.push_str(&format!(
            "      <data key=\"d_kind\">{}</data>\n",
            escape_graphml(&node.kind)
        ));
        if !node.symbol.is_empty() {
            out.push_str(&format!(
                "      <data key=\"d_symbol\">{}</data>\n",
                escape_graphml(&node.symbol)
            ));
        }
        out.push_str(&format!(
            "      <data key=\"d_tokens\">{}</data>\n",
            node.token_count
        ));
        out.push_str("    </node>\n");
    }

    for (i, edge) in doc.edges.iter().enumerate() {
        let src = escape_graphml(&edge.source);
        let tgt = escape_graphml(&edge.target);
        out.push_str(&format!(
            "    <edge id=\"e{i}\" source=\"{src}\" target=\"{tgt}\">\n"
        ));
        out.push_str(&format!(
            "      <data key=\"d_weight\">{}</data>\n",
            edge.weight
        ));
        out.push_str(&format!(
            "      <data key=\"d_category\">{}</data>\n",
            escape_graphml(&edge.category)
        ));
        out.push_str("    </edge>\n");
    }

    out.push_str("  </graph>\n");
    out.push_str("</graphml>\n");
    out
}

fn collect_files(fragments: &FxHashMap<FragmentId, Fragment>) -> usize {
    let mut paths: rustc_hash::FxHashSet<Arc<str>> = rustc_hash::FxHashSet::default();
    for fid in fragments.keys() {
        paths.insert(fid.path.clone());
    }
    paths.len()
}

fn compute_in_degree(graph: &Graph) -> FxHashMap<FragmentId, usize> {
    let mut counts: FxHashMap<FragmentId, usize> = FxHashMap::default();
    for (_src, dst) in graph.edge_categories.keys() {
        *counts.entry(dst.clone()).or_insert(0) += 1;
    }
    counts
}

pub fn graph_summary(view: &ProjectGraphView<'_>, top_n: usize) -> GraphSummary {
    let node_count = view.graph.node_count();
    let edge_count = view.graph.edge_count();
    let file_count = collect_files(view.fragments);

    let density = if node_count > 1 {
        edge_count as f64 / (node_count * (node_count - 1)) as f64
    } else {
        0.0
    };

    let mut type_counts: FxHashMap<String, usize> = FxHashMap::default();
    for cat in view.graph.edge_categories.values() {
        *type_counts.entry(cat.as_str().to_string()).or_insert(0) += 1;
    }

    let in_deg = compute_in_degree(view.graph);
    let mut sorted: Vec<(FragmentId, usize)> = in_deg.into_iter().collect();
    sorted.sort_by(|a, b| {
        b.1.cmp(&a.1)
            .then_with(|| a.0.path.as_ref().cmp(b.0.path.as_ref()))
            .then_with(|| a.0.start_line.cmp(&b.0.start_line))
    });

    let top_in_degree: Vec<TopInDegreeEntry> = sorted
        .into_iter()
        .take(top_n)
        .map(|(fid, deg)| {
            let frag = view.fragments.get(&fid);
            let label = match frag.and_then(|f| f.symbol_name.as_deref()) {
                Some(name) if !name.is_empty() => name.to_string(),
                _ => format!("{}:{}", file_name(&fid.path), fid.start_line),
            };
            TopInDegreeEntry {
                label,
                in_degree: deg,
            }
        })
        .collect();

    GraphSummary {
        node_count,
        edge_count,
        file_count,
        density,
        edge_type_counts: type_counts,
        top_in_degree,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::build_graph;
    use crate::types::{Fragment, FragmentId, FragmentKind};
    use rustc_hash::{FxHashMap, FxHashSet};
    use std::sync::Arc;

    fn make_fragment(path: &str, start: u32, end: u32, symbol: Option<&str>) -> Fragment {
        Fragment {
            id: FragmentId::new(Arc::from(path), start, end),
            kind: FragmentKind::Function,
            content: Arc::from("body"),
            identifiers: FxHashSet::default(),
            token_count: 10,
            symbol_name: symbol.map(|s| s.to_string()),
        }
    }

    fn fragments_map(frags: &[Fragment]) -> FxHashMap<FragmentId, Fragment> {
        frags.iter().map(|f| (f.id.clone(), f.clone())).collect()
    }

    #[test]
    fn empty_graph_emits_valid_outputs() {
        let graph = build_graph(&[], FxHashMap::default(), FxHashMap::default());
        let frags = FxHashMap::default();
        let view = ProjectGraphView {
            graph: &graph,
            fragments: &frags,
            root_dir: None,
        };

        let json = graph_to_json_string(&view).expect("json ok");
        assert!(json.contains("\"node_count\": 0"));
        assert!(json.contains("\"edge_count\": 0"));
        assert!(json.contains("\"nodes\": []"));
        assert!(json.contains("\"edges\": []"));
        assert!(json.ends_with('\n'));

        let xml = graph_to_graphml_string(&view);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<graph id=\"unknown\" edgedefault=\"directed\">"));
        assert!(xml.contains("</graphml>"));
        assert!(!xml.contains("<node "));
        assert!(!xml.contains("<edge "));

        let summary = graph_summary(&view, 5);
        assert_eq!(summary.node_count, 0);
        assert_eq!(summary.edge_count, 0);
        assert_eq!(summary.file_count, 0);
        assert!((summary.density - 0.0).abs() < 1e-12);
        assert!(summary.top_in_degree.is_empty());
    }

    #[test]
    fn json_round_trip_preserves_schema() {
        let f1 = make_fragment("src/a.rs", 1, 10, Some("foo"));
        let f2 = make_fragment("src/b.rs", 5, 20, None);
        let f3 = make_fragment("src/c.rs", 1, 30, Some("baz<T>"));

        let frags = vec![f1.clone(), f2.clone(), f3.clone()];
        let mut edges: FxHashMap<(FragmentId, FragmentId), f64> = FxHashMap::default();
        let mut cats: FxHashMap<(FragmentId, FragmentId), EdgeCategory> = FxHashMap::default();
        edges.insert((f1.id.clone(), f2.id.clone()), 0.55);
        cats.insert((f1.id.clone(), f2.id.clone()), EdgeCategory::Semantic);
        edges.insert((f2.id.clone(), f3.id.clone()), 0.25);
        cats.insert((f2.id.clone(), f3.id.clone()), EdgeCategory::Structural);

        let graph = build_graph(&frags, edges, cats);
        let frag_map = fragments_map(&frags);
        let view = ProjectGraphView {
            graph: &graph,
            fragments: &frag_map,
            root_dir: None,
        };

        let json = graph_to_json_string(&view).expect("json ok");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");

        assert_eq!(parsed["type"], "project_graph");
        assert_eq!(parsed["node_count"], 3);
        assert_eq!(parsed["edge_count"], 2);
        let nodes = parsed["nodes"].as_array().expect("nodes array");
        assert_eq!(nodes.len(), 3);
        assert_eq!(nodes[0]["path"], "src/a.rs");
        assert_eq!(nodes[0]["symbol"], "foo");
        assert_eq!(nodes[0]["lines"], "1-10");
        assert_eq!(nodes[0]["kind"], "function");
        assert_eq!(nodes[1]["symbol"], "");

        let edges_out = parsed["edges"].as_array().expect("edges array");
        assert_eq!(edges_out.len(), 2);
        assert!(
            edges_out
                .iter()
                .any(|e| e["category"] == "semantic" && e["weight"].as_f64() == Some(0.55))
        );
    }

    #[test]
    fn summary_counts_correct_for_small_graph() {
        let f1 = make_fragment("src/a.rs", 1, 10, Some("alpha"));
        let f2 = make_fragment("src/a.rs", 20, 30, Some("beta"));
        let f3 = make_fragment("src/b.rs", 1, 5, None);

        let frags = vec![f1.clone(), f2.clone(), f3.clone()];
        let mut edges: FxHashMap<(FragmentId, FragmentId), f64> = FxHashMap::default();
        let mut cats: FxHashMap<(FragmentId, FragmentId), EdgeCategory> = FxHashMap::default();
        edges.insert((f1.id.clone(), f3.id.clone()), 0.4);
        cats.insert((f1.id.clone(), f3.id.clone()), EdgeCategory::Semantic);
        edges.insert((f2.id.clone(), f3.id.clone()), 0.3);
        cats.insert((f2.id.clone(), f3.id.clone()), EdgeCategory::Semantic);

        let graph = build_graph(&frags, edges, cats);
        let frag_map = fragments_map(&frags);
        let view = ProjectGraphView {
            graph: &graph,
            fragments: &frag_map,
            root_dir: None,
        };

        let summary = graph_summary(&view, 5);
        assert_eq!(summary.node_count, 3);
        assert_eq!(summary.edge_count, 2);
        assert_eq!(summary.file_count, 2);
        let expected_density = 2.0 / (3.0 * 2.0);
        assert!((summary.density - expected_density).abs() < 1e-9);
        assert_eq!(summary.edge_type_counts.get("semantic"), Some(&2));
        assert_eq!(summary.top_in_degree.len(), 1);
        assert_eq!(summary.top_in_degree[0].in_degree, 2);
    }

    #[test]
    fn graphml_escapes_special_characters() {
        let mut frag = make_fragment("src/<weird>.rs", 1, 10, Some("ham&eggs<\"x\"'>"));
        frag.token_count = 7;
        let frags = vec![frag.clone()];
        let frag_map = fragments_map(&frags);

        let graph = build_graph(&frags, FxHashMap::default(), FxHashMap::default());
        let view = ProjectGraphView {
            graph: &graph,
            fragments: &frag_map,
            root_dir: None,
        };

        let xml = graph_to_graphml_string(&view);
        assert!(xml.contains("ham&amp;eggs&lt;&quot;x&quot;&apos;&gt;"));
        assert!(xml.contains("src/&lt;weird&gt;.rs"));
        assert!(!xml.contains("ham&eggs"));
        assert!(xml.contains("<data key=\"d_tokens\">7</data>"));
    }
}
