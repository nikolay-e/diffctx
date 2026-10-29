//! What each edge category links, scored against the cases' own oracles.
//!
//! Report only: no threshold. For every case under the directories
//! `DIFFCTX_EDGE_PRECISION_CASES` names (comma-separated, relative to
//! `tests/cases/diff`; default `infrastructure/keys`) it builds the graph over
//! the tree after the change and counts, per category, the file pairs with
//! exactly one changed endpoint by what sits at the other end — a required
//! file, a forbidden one, or neither. Run with `-- --nocapture`.

use std::collections::{BTreeMap, BTreeSet};

use _diffctx::in_memory_harness::final_state_edges;

#[allow(dead_code)]
mod common;
mod key_cases;
use key_cases::{Label, Tally, far_end};

#[test]
fn per_category_edge_precision() {
    let dirs = key_cases::case_dirs("DIFFCTX_EDGE_PRECISION_CASES", &["infrastructure/keys"]);
    let cases = key_cases::load_cases(&dirs);
    assert!(!cases.is_empty(), "no cases under {dirs:?}");

    let mut by_category: BTreeMap<&'static str, Tally> = BTreeMap::new();
    let mut required_total = 0usize;
    let mut reached_by: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut reached_any = 0usize;

    for case in &cases {
        let mut pairs: BTreeMap<(String, String), (&'static str, f64)> = BTreeMap::new();
        for e in final_state_edges(&case.repo) {
            let key = if e.src <= e.dst {
                (e.src.clone(), e.dst.clone())
            } else {
                (e.dst.clone(), e.src.clone())
            };
            let slot = pairs.entry(key).or_insert((e.category, e.weight));
            if e.weight > slot.1 {
                *slot = (e.category, e.weight);
            }
        }

        let required = case.required_files();
        let mut reached: BTreeMap<&'static str, BTreeSet<&str>> = BTreeMap::new();
        let mut per_case: BTreeMap<&'static str, Tally> = BTreeMap::new();
        for ((a, b), (category, _)) in &pairs {
            let Some(far) = far_end(case, a, b) else {
                continue;
            };
            let label = case.label(far);
            by_category.entry(category).or_default().add(label);
            per_case.entry(category).or_default().add(label);
            if label == Label::Right {
                reached.entry(category).or_default().insert(far);
            }
        }

        let hit_any: BTreeSet<&str> = reached.values().flatten().copied().collect();
        required_total += required.len();
        reached_any += hit_any.len();
        for (category, files) in &reached {
            *reached_by.entry(category).or_default() += files.len();
        }
        let summary: Vec<String> = per_case
            .iter()
            .map(|(c, t)| format!("{c} {}/{}/{}", t.right, t.wrong, t.unlabelled))
            .collect();
        println!(
            "case {}: required {} reached {}  [{}]",
            case.name,
            required.len(),
            hit_any.len(),
            summary.join(", ")
        );
    }

    println!("\ncases: {}  (right/wrong/unlabelled)", cases.len());
    for (category, tally) in &by_category {
        println!(
            "  {category:<15} {}  recall {}/{}",
            tally.describe(),
            reached_by.get(category).copied().unwrap_or(0),
            required_total
        );
    }
    println!(
        "  {:<15} required files reached by any category: {reached_any}/{required_total}",
        "all"
    );
    println!(
        "  note: with n of 20-40 links, 0.85 tolerates 3-6 wrong ones; the interval is the power."
    );
}
