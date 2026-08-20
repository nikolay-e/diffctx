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
    pub image_weight: f64,
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
    image_weight: 0.40,
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
