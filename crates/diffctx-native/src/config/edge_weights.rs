use once_cell::sync::Lazy;

pub struct DockerWeights {
    pub weight: f64,
    pub copy_weight: f64,
    pub compose_weight: f64,
    pub reverse_factor: f64,
    pub compose_context_modifier: f64,
    pub compose_volume_modifier: f64,
}

impl Default for DockerWeights {
    fn default() -> Self {
        Self {
            weight: 0.55,
            copy_weight: 0.65,
            compose_weight: 0.50,
            reverse_factor: 0.40,
            compose_context_modifier: 0.7,
            compose_volume_modifier: 0.6,
        }
    }
}

pub struct KubernetesWeights {
    pub weight: f64,
    pub configmap_secret_weight: f64,
    pub service_weight: f64,
    pub selector_weight: f64,
    pub image_weight: f64,
    pub reverse_factor: f64,
}

impl Default for KubernetesWeights {
    fn default() -> Self {
        Self {
            weight: 0.65,
            configmap_secret_weight: 0.70,
            service_weight: 0.60,
            selector_weight: 0.55,
            image_weight: 0.40,
            reverse_factor: 0.45,
        }
    }
}

pub struct HelmWeights {
    pub weight: f64,
    pub reverse_factor: f64,
    pub value_modifier: f64,
    pub definition_modifier: f64,
    pub configmap_modifier: f64,
}

impl Default for HelmWeights {
    fn default() -> Self {
        Self {
            weight: 0.70,
            reverse_factor: 0.45,
            value_modifier: 0.8,
            definition_modifier: 0.9,
            configmap_modifier: 0.5,
        }
    }
}

pub struct BuildSystemWeights {
    pub file_ref_weight: f64,
    pub reverse_factor: f64,
}

impl Default for BuildSystemWeights {
    fn default() -> Self {
        Self {
            file_ref_weight: 0.60,
            reverse_factor: 0.35,
        }
    }
}

pub struct CicdWeights {
    pub weight: f64,
    pub script_weight: f64,
    pub reverse_factor: f64,
    pub script_modifier: f64,
}

impl Default for CicdWeights {
    fn default() -> Self {
        Self {
            weight: 0.55,
            script_weight: 0.60,
            reverse_factor: 0.35,
            script_modifier: 0.8,
        }
    }
}

pub struct PythonSemanticWeights {
    pub import_weight: f64,
    pub import_confirmed_boost: f64,
    pub import_unconfirmed_penalty: f64,
    pub reverse_factor: f64,
}

impl Default for PythonSemanticWeights {
    fn default() -> Self {
        Self {
            import_weight: 0.75,
            import_confirmed_boost: 1.5,
            import_unconfirmed_penalty: 0.2,
            reverse_factor: 0.5,
        }
    }
}

pub struct JavascriptSemanticWeights {
    pub import_weight: f64,
    pub reverse_factor: f64,
}

impl Default for JavascriptSemanticWeights {
    fn default() -> Self {
        Self {
            import_weight: 0.55,
            reverse_factor: 0.5,
        }
    }
}

pub struct GoSemanticWeights {
    pub init_same_package_weight: f64,
}

impl Default for GoSemanticWeights {
    fn default() -> Self {
        Self {
            init_same_package_weight: 0.15,
        }
    }
}

pub struct OpenapiSemanticWeights {
    pub marker_scan_lines: usize,
}

impl Default for OpenapiSemanticWeights {
    fn default() -> Self {
        Self {
            marker_scan_lines: 5,
        }
    }
}

pub struct AnsibleSemanticWeights {
    pub sibling_modifier: f64,
}

impl Default for AnsibleSemanticWeights {
    fn default() -> Self {
        Self {
            sibling_modifier: 0.6,
        }
    }
}

pub struct CFamilySemanticWeights {
    pub base_weight: f64,
}

impl Default for CFamilySemanticWeights {
    fn default() -> Self {
        Self { base_weight: 0.70 }
    }
}

pub struct TerraformSemanticWeights {
    pub weight: f64,
    pub reverse_factor: f64,
    pub module_source_modifier: f64,
}

impl Default for TerraformSemanticWeights {
    fn default() -> Self {
        Self {
            weight: 0.60,
            reverse_factor: 0.40,
            module_source_modifier: 0.8,
        }
    }
}

pub struct TagsSemanticWeights {
    pub weight: f64,
    pub reverse_factor: f64,
    pub max_fragments_per_ident: usize,
    pub min_ident_len: usize,
}

impl Default for TagsSemanticWeights {
    fn default() -> Self {
        Self {
            weight: 0.30,
            reverse_factor: 0.70,
            max_fragments_per_ident: 5,
            min_ident_len: 3,
        }
    }
}

pub struct SemanticDiscoveryConfig {
    pub max_depth: usize,
    pub min_identifier_length: usize,
    pub min_ref_length_for_path_match: usize,
}

impl Default for SemanticDiscoveryConfig {
    fn default() -> Self {
        Self {
            max_depth: 2,
            min_identifier_length: 2,
            min_ref_length_for_path_match: 3,
        }
    }
}

pub static DOCKER: Lazy<DockerWeights> = Lazy::new(DockerWeights::default);
pub static KUBERNETES: Lazy<KubernetesWeights> = Lazy::new(KubernetesWeights::default);
pub static HELM: Lazy<HelmWeights> = Lazy::new(HelmWeights::default);
pub static BUILD_SYSTEM: Lazy<BuildSystemWeights> = Lazy::new(BuildSystemWeights::default);
pub static CICD: Lazy<CicdWeights> = Lazy::new(CicdWeights::default);
pub static PYTHON_SEMANTIC: Lazy<PythonSemanticWeights> = Lazy::new(PythonSemanticWeights::default);
pub static JAVASCRIPT_SEMANTIC: Lazy<JavascriptSemanticWeights> =
    Lazy::new(JavascriptSemanticWeights::default);
pub static TERRAFORM_SEMANTIC: Lazy<TerraformSemanticWeights> =
    Lazy::new(TerraformSemanticWeights::default);
pub static GO_SEMANTIC: Lazy<GoSemanticWeights> = Lazy::new(GoSemanticWeights::default);
pub static ANSIBLE_SEMANTIC: Lazy<AnsibleSemanticWeights> =
    Lazy::new(AnsibleSemanticWeights::default);
pub static OPENAPI_SEMANTIC: Lazy<OpenapiSemanticWeights> =
    Lazy::new(OpenapiSemanticWeights::default);
pub static C_FAMILY_SEMANTIC: Lazy<CFamilySemanticWeights> =
    Lazy::new(CFamilySemanticWeights::default);
pub static TAGS_SEMANTIC: Lazy<TagsSemanticWeights> = Lazy::new(TagsSemanticWeights::default);
pub static SEMANTIC_DISCOVERY: Lazy<SemanticDiscoveryConfig> =
    Lazy::new(SemanticDiscoveryConfig::default);
