pub struct DockerWeights {
    pub weight: f64,
    pub copy_weight: f64,
    pub compose_weight: f64,
    pub reverse_factor: f64,
    pub compose_context_modifier: f64,
    pub compose_volume_modifier: f64,
}

pub struct KubernetesWeights {
    pub weight: f64,
    pub configmap_secret_weight: f64,
    pub service_weight: f64,
    pub selector_weight: f64,
    pub reverse_factor: f64,
}

pub struct HelmWeights {
    pub weight: f64,
    pub reverse_factor: f64,
    pub value_modifier: f64,
    pub definition_modifier: f64,
    pub configmap_modifier: f64,
}

pub struct BuildSystemWeights {
    pub file_ref_weight: f64,
    pub reverse_factor: f64,
}

pub struct CicdWeights {
    pub weight: f64,
    pub script_weight: f64,
    pub reverse_factor: f64,
    pub script_modifier: f64,
}

pub struct PythonSemanticWeights {
    pub import_weight: f64,
    pub import_confirmed_boost: f64,
    pub import_unconfirmed_penalty: f64,
    pub reverse_factor: f64,
}

pub struct JavascriptSemanticWeights {
    pub import_weight: f64,
    pub reverse_factor: f64,
}

pub struct GoSemanticWeights {
    pub init_same_package_weight: f64,
}

pub struct OpenapiSemanticWeights {
    pub marker_scan_lines: usize,
}

pub struct AnsibleSemanticWeights {
    pub sibling_modifier: f64,
}

pub struct CFamilySemanticWeights {
    pub base_weight: f64,
    /// A name (basename, function, type) defined in more than this many
    /// distinct files carries no discriminative signal: linking every user to
    /// every definition is where envoy's 520 same-stem `config` files turned
    /// one builder pass into tens of millions of `add_edge` calls (50% of
    /// dcbench hung on it). Ambiguous names are skipped outright rather than
    /// truncated — a deterministic prefix of the wrong 8 definitions would
    /// just be smaller noise.
    pub max_files_per_name: usize,
}

pub struct TerraformSemanticWeights {
    pub weight: f64,
    pub reverse_factor: f64,
    pub module_source_modifier: f64,
}

pub struct TagsSemanticWeights {
    pub weight: f64,
    pub reverse_factor: f64,
    pub max_fragments_per_ident: usize,
    pub min_ident_len: usize,
}

pub struct SemanticDiscoveryConfig {
    pub max_depth: usize,
    pub min_identifier_length: usize,
    pub min_ref_length_for_path_match: usize,
}

pub const DOCKER: DockerWeights = DockerWeights {
    weight: 0.55,
    copy_weight: 0.65,
    compose_weight: 0.50,
    reverse_factor: 0.40,
    compose_context_modifier: 0.7,
    compose_volume_modifier: 0.6,
};
pub const KUBERNETES: KubernetesWeights = KubernetesWeights {
    weight: 0.65,
    configmap_secret_weight: 0.70,
    service_weight: 0.60,
    selector_weight: 0.55,
    reverse_factor: 0.45,
};
pub const HELM: HelmWeights = HelmWeights {
    weight: 0.70,
    reverse_factor: 0.45,
    value_modifier: 0.8,
    definition_modifier: 0.9,
    configmap_modifier: 0.5,
};
pub const BUILD_SYSTEM: BuildSystemWeights = BuildSystemWeights {
    file_ref_weight: 0.60,
    reverse_factor: 0.35,
};
pub const CICD: CicdWeights = CicdWeights {
    weight: 0.55,
    script_weight: 0.60,
    reverse_factor: 0.35,
    script_modifier: 0.8,
};
pub const PYTHON_SEMANTIC: PythonSemanticWeights = PythonSemanticWeights {
    import_weight: 0.75,
    import_confirmed_boost: 1.5,
    import_unconfirmed_penalty: 0.2,
    reverse_factor: 0.5,
};
pub const JAVASCRIPT_SEMANTIC: JavascriptSemanticWeights = JavascriptSemanticWeights {
    import_weight: 0.55,
    reverse_factor: 0.5,
};
pub const TERRAFORM_SEMANTIC: TerraformSemanticWeights = TerraformSemanticWeights {
    weight: 0.60,
    reverse_factor: 0.40,
    module_source_modifier: 0.8,
};
pub const GO_SEMANTIC: GoSemanticWeights = GoSemanticWeights {
    init_same_package_weight: 0.15,
};
pub const ANSIBLE_SEMANTIC: AnsibleSemanticWeights = AnsibleSemanticWeights {
    sibling_modifier: 0.6,
};
pub const OPENAPI_SEMANTIC: OpenapiSemanticWeights = OpenapiSemanticWeights {
    marker_scan_lines: 5,
};
pub const C_FAMILY_SEMANTIC: CFamilySemanticWeights = CFamilySemanticWeights {
    base_weight: 0.70,
    max_files_per_name: 8,
};
pub const TAGS_SEMANTIC: TagsSemanticWeights = TagsSemanticWeights {
    weight: 0.30,
    reverse_factor: 0.70,
    max_fragments_per_ident: 5,
    min_ident_len: 3,
};
pub const SEMANTIC_DISCOVERY: SemanticDiscoveryConfig = SemanticDiscoveryConfig {
    max_depth: 2,
    min_identifier_length: 2,
    min_ref_length_for_path_match: 3,
};

/// The groups of languages whose ranking edges can come from the cells' bindings
/// (`edges/cells.rs`); a group not drawn from them keeps its text builders.
pub const CELL_EDGE_GROUPS: [&str; 12] = [
    "python",
    "typescript",
    "rust",
    "jvm",
    "go",
    "php",
    "c",
    "terraform",
    "shell",
    "swift",
    "sql",
    "config",
];

/// The one table of what a cell-drawn edge is worth. A resolved binding's edge
/// carries the weight the language's weight table names for its relation, times
/// `resolved`; the rest are switches the measurement flips, each env-overridable
/// so one binary measures every variant.
///
/// - `groups`: the groups drawn from cells (`DIFFCTX_CELL_EDGES`: `all`, `none`
///   or a comma list of `CELL_EDGE_GROUPS`). `none` is the old builders' edges.
/// - `candidates` (`DIFFCTX_CELL_EDGE_CANDIDATES`): a binding the cell narrowed to
///   several definitions draws an edge to each, worth the weight times
///   `candidate_factor` divided by their number, up to `max_candidates`.
/// - `unresolved` (`DIFFCTX_CELL_EDGE_UNRESOLVED`): a binding of a name nothing
///   binds draws a weak edge to each definition of that name in the family, worth
///   the weight times `unresolved_factor`, divided by their number, up to
///   `max_unresolved_targets`. At the shipped factor it stays below
///   `NAMING_WEIGHT_FLOOR`, so it ranks and admits through the declared channel
///   only.
#[derive(Debug, Clone)]
pub struct CellEdgeWeights {
    groups: Vec<String>,
    pub resolved: f64,
    pub candidates: bool,
    pub candidate_factor: f64,
    pub max_candidates: usize,
    pub unresolved: bool,
    pub unresolved_factor: f64,
    pub max_unresolved_targets: usize,
}

impl CellEdgeWeights {
    /// Whether the ranking edges of `group` come from the cells.
    pub fn draws(&self, group: &str) -> bool {
        self.groups.iter().any(|g| g == group)
    }

    fn from_env(read: impl Fn(&str) -> Option<String>) -> Self {
        use super::env_overrides::{parse_f64_or_default, parse_usize_or_default};
        let flag = |name: &str, default: bool| match read(name).as_deref().map(str::trim) {
            Some("1" | "true" | "on") => true,
            Some("0" | "false" | "off") => false,
            _ => default,
        };
        Self {
            groups: parse_groups(read("DIFFCTX_CELL_EDGES").as_deref()),
            resolved: parse_f64_or_default(read("DIFFCTX_CELL_EDGE_RESOLVED"), 1.0),
            candidates: flag("DIFFCTX_CELL_EDGE_CANDIDATES", false),
            candidate_factor: parse_f64_or_default(read("DIFFCTX_CELL_EDGE_CANDIDATE_FACTOR"), 1.0),
            max_candidates: parse_usize_or_default(read("DIFFCTX_CELL_EDGE_MAX_CANDIDATES"), 8),
            unresolved: flag("DIFFCTX_CELL_EDGE_UNRESOLVED", false),
            unresolved_factor: parse_f64_or_default(
                read("DIFFCTX_CELL_EDGE_UNRESOLVED_FACTOR"),
                0.3,
            ),
            max_unresolved_targets: parse_usize_or_default(
                read("DIFFCTX_CELL_EDGE_MAX_UNRESOLVED_TARGETS"),
                8,
            ),
        }
    }
}

fn parse_groups(raw: Option<&str>) -> Vec<String> {
    let all = || CELL_EDGE_GROUPS.iter().map(|g| (*g).to_string()).collect();
    match raw.map(|r| r.trim().to_ascii_lowercase()).as_deref() {
        None | Some("" | "all") => all(),
        Some("none" | "old") => Vec::new(),
        Some(list) => list
            .split(',')
            .map(str::trim)
            .filter(|g| CELL_EDGE_GROUPS.contains(g))
            .map(str::to_string)
            .collect(),
    }
}

pub static CELL_EDGES: once_cell::sync::Lazy<CellEdgeWeights> =
    once_cell::sync::Lazy::new(|| CellEdgeWeights::from_env(|name| std::env::var(name).ok()));

#[cfg(test)]
mod cell_edge_tests {
    use super::*;

    fn weights(vars: &[(&str, &str)]) -> CellEdgeWeights {
        CellEdgeWeights::from_env(|name| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_string())
        })
    }

    #[test]
    fn every_group_draws_from_cells_and_the_inexact_edges_are_off_by_default() {
        let w = weights(&[]);
        assert!(CELL_EDGE_GROUPS.iter().all(|g| w.draws(g)));
        assert!(!w.candidates && !w.unresolved);
        assert_eq!(w.resolved, 1.0);
    }

    #[test]
    fn a_list_names_the_groups_and_none_gives_the_old_builders_back() {
        let w = weights(&[("DIFFCTX_CELL_EDGES", "python, go, nonsense")]);
        assert!(w.draws("python") && w.draws("go") && !w.draws("rust"));
        let old = weights(&[("DIFFCTX_CELL_EDGES", "none")]);
        assert!(CELL_EDGE_GROUPS.iter().all(|g| !old.draws(g)));
    }

    #[test]
    fn the_switches_read_their_variables() {
        let w = weights(&[
            ("DIFFCTX_CELL_EDGE_CANDIDATES", "1"),
            ("DIFFCTX_CELL_EDGE_UNRESOLVED", "on"),
            ("DIFFCTX_CELL_EDGE_UNRESOLVED_FACTOR", "0.2"),
        ]);
        assert!(w.candidates && w.unresolved);
        assert_eq!(w.unresolved_factor, 0.2);
    }
}
