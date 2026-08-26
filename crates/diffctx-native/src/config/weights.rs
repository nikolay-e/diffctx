use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, Copy)]
pub struct EdgeWeightConfig {
    pub forward: f64,
    pub reverse_factor: f64,
}

impl EdgeWeightConfig {
    pub const fn new(forward: f64, reverse_factor: f64) -> Self {
        Self {
            forward,
            reverse_factor,
        }
    }

    pub fn reverse(&self) -> f64 {
        self.forward * self.reverse_factor
    }
}

pub static EDGE_WEIGHTS: Lazy<FxHashMap<&'static str, EdgeWeightConfig>> = Lazy::new(|| {
    let entries: &[(&str, EdgeWeightConfig)] = &[
        ("containment", EdgeWeightConfig::new(0.50, 0.70)),
        ("import", EdgeWeightConfig::new(0.50, 0.70)),
        ("test_direct", EdgeWeightConfig::new(0.60, 0.50)),
        ("test_naming", EdgeWeightConfig::new(0.50, 0.50)),
        ("test_reverse", EdgeWeightConfig::new(0.30, 1.0)),
        ("config_code", EdgeWeightConfig::new(0.35, 0.50)),
        ("sibling", EdgeWeightConfig::new(0.05, 1.0)),
        ("cochange", EdgeWeightConfig::new(0.40, 1.0)),
        ("doc_structure", EdgeWeightConfig::new(0.30, 0.83)),
        ("anchor_link", EdgeWeightConfig::new(0.55, 0.64)),
        ("citation", EdgeWeightConfig::new(0.25, 1.0)),
        ("python_call", EdgeWeightConfig::new(0.65, 0.70)),
        ("python_symbol", EdgeWeightConfig::new(0.70, 0.70)),
        ("python_type", EdgeWeightConfig::new(0.50, 0.70)),
        ("javascript_call", EdgeWeightConfig::new(0.70, 0.50)),
        ("javascript_symbol", EdgeWeightConfig::new(0.75, 0.50)),
        ("javascript_type", EdgeWeightConfig::new(0.65, 0.50)),
        ("go_import", EdgeWeightConfig::new(0.70, 0.40)),
        ("go_type", EdgeWeightConfig::new(0.65, 0.40)),
        ("go_func", EdgeWeightConfig::new(0.60, 0.40)),
        ("go_same_package", EdgeWeightConfig::new(0.05, 0.40)),
        ("rust_mod", EdgeWeightConfig::new(0.70, 0.40)),
        ("rust_use", EdgeWeightConfig::new(0.65, 0.40)),
        ("rust_type", EdgeWeightConfig::new(0.65, 0.40)),
        ("rust_fn", EdgeWeightConfig::new(0.60, 0.40)),
        ("rust_same_crate", EdgeWeightConfig::new(0.05, 0.40)),
        ("jvm_import", EdgeWeightConfig::new(0.75, 0.40)),
        ("jvm_inheritance", EdgeWeightConfig::new(0.80, 0.40)),
        ("jvm_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("jvm_member", EdgeWeightConfig::new(0.65, 0.40)),
        ("jvm_same_package", EdgeWeightConfig::new(0.05, 0.40)),
        ("jvm_annotation", EdgeWeightConfig::new(0.50, 0.40)),
        ("c_include", EdgeWeightConfig::new(0.65, 0.40)),
        ("c_call", EdgeWeightConfig::new(0.55, 0.40)),
        ("c_type", EdgeWeightConfig::new(0.50, 0.40)),
        ("c_inheritance", EdgeWeightConfig::new(0.70, 0.40)),
        ("dotnet_using", EdgeWeightConfig::new(0.65, 0.40)),
        ("dotnet_inheritance", EdgeWeightConfig::new(0.75, 0.40)),
        ("dotnet_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("dotnet_member", EdgeWeightConfig::new(0.65, 0.40)),
        ("dotnet_same_namespace", EdgeWeightConfig::new(0.05, 0.40)),
        ("dotnet_attribute", EdgeWeightConfig::new(0.50, 0.40)),
        ("dotnet_partial", EdgeWeightConfig::new(0.80, 0.40)),
        ("ruby_require", EdgeWeightConfig::new(0.65, 0.40)),
        ("ruby_include", EdgeWeightConfig::new(0.60, 0.40)),
        ("ruby_const", EdgeWeightConfig::new(0.55, 0.40)),
        ("ruby_same_dir", EdgeWeightConfig::new(0.05, 0.40)),
        ("php_use", EdgeWeightConfig::new(0.65, 0.40)),
        ("php_require", EdgeWeightConfig::new(0.60, 0.40)),
        ("php_inheritance", EdgeWeightConfig::new(0.75, 0.40)),
        ("php_type", EdgeWeightConfig::new(0.55, 0.40)),
        ("php_same_namespace", EdgeWeightConfig::new(0.05, 0.40)),
        ("shell_source", EdgeWeightConfig::new(0.60, 0.35)),
        ("shell_script", EdgeWeightConfig::new(0.50, 0.35)),
        ("swift_import", EdgeWeightConfig::new(0.65, 0.40)),
        ("swift_conformance", EdgeWeightConfig::new(0.70, 0.40)),
        ("swift_extension", EdgeWeightConfig::new(0.65, 0.40)),
        ("swift_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("swift_func", EdgeWeightConfig::new(0.55, 0.40)),
        ("swift_same_module", EdgeWeightConfig::new(0.05, 0.40)),
        ("zig_import", EdgeWeightConfig::new(0.65, 0.40)),
        ("zig_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("zig_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("haskell_import", EdgeWeightConfig::new(0.70, 0.40)),
        ("haskell_type", EdgeWeightConfig::new(0.65, 0.40)),
        ("haskell_fn", EdgeWeightConfig::new(0.60, 0.40)),
        ("haskell_instance", EdgeWeightConfig::new(0.55, 0.40)),
        ("clojure_require", EdgeWeightConfig::new(0.65, 0.40)),
        ("clojure_fn", EdgeWeightConfig::new(0.60, 0.40)),
        ("clojure_protocol", EdgeWeightConfig::new(0.55, 0.40)),
        ("proto_import", EdgeWeightConfig::new(0.65, 0.40)),
        ("proto_message_ref", EdgeWeightConfig::new(0.60, 0.40)),
        ("proto_service_rpc", EdgeWeightConfig::new(0.55, 0.40)),
        ("graphql_import", EdgeWeightConfig::new(0.60, 0.40)),
        ("graphql_type_ref", EdgeWeightConfig::new(0.65, 0.40)),
        ("graphql_extend", EdgeWeightConfig::new(0.55, 0.40)),
        ("sql_fk", EdgeWeightConfig::new(0.70, 0.40)),
        ("sql_table_ref", EdgeWeightConfig::new(0.60, 0.40)),
        ("sql_view_source", EdgeWeightConfig::new(0.65, 0.40)),
        ("sql_migration", EdgeWeightConfig::new(0.55, 0.40)),
        ("css_import", EdgeWeightConfig::new(0.55, 0.40)),
        ("lua_require", EdgeWeightConfig::new(0.65, 0.40)),
        ("lua_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("lua_method", EdgeWeightConfig::new(0.50, 0.40)),
        ("openapi_internal_ref", EdgeWeightConfig::new(0.65, 0.40)),
        ("openapi_external_ref", EdgeWeightConfig::new(0.60, 0.40)),
        ("openapi_schema_ref", EdgeWeightConfig::new(0.55, 0.40)),
        ("erlang_include", EdgeWeightConfig::new(0.65, 0.40)),
        ("erlang_behaviour", EdgeWeightConfig::new(0.60, 0.40)),
        ("erlang_call", EdgeWeightConfig::new(0.55, 0.40)),
        ("elixir_use", EdgeWeightConfig::new(0.65, 0.40)),
        ("elixir_alias", EdgeWeightConfig::new(0.60, 0.40)),
        ("elixir_behaviour", EdgeWeightConfig::new(0.55, 0.40)),
        ("elixir_fn", EdgeWeightConfig::new(0.50, 0.40)),
        ("dart_import", EdgeWeightConfig::new(0.65, 0.40)),
        ("dart_export", EdgeWeightConfig::new(0.60, 0.40)),
        ("dart_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("dart_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("dart_inheritance", EdgeWeightConfig::new(0.70, 0.40)),
        ("cargo_workspace", EdgeWeightConfig::new(0.60, 0.40)),
        ("cargo_path_dep", EdgeWeightConfig::new(0.65, 0.40)),
        ("cargo_entry_point", EdgeWeightConfig::new(0.55, 0.40)),
        ("ocaml_open", EdgeWeightConfig::new(0.65, 0.40)),
        ("ocaml_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("ocaml_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("ocaml_module_ref", EdgeWeightConfig::new(0.60, 0.40)),
        ("r_source", EdgeWeightConfig::new(0.60, 0.40)),
        ("r_library", EdgeWeightConfig::new(0.55, 0.40)),
        ("r_fn", EdgeWeightConfig::new(0.50, 0.40)),
        ("r_s4", EdgeWeightConfig::new(0.55, 0.40)),
        ("perl_use", EdgeWeightConfig::new(0.65, 0.40)),
        ("perl_require", EdgeWeightConfig::new(0.60, 0.40)),
        ("perl_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("perl_method", EdgeWeightConfig::new(0.50, 0.40)),
        ("perl_inheritance", EdgeWeightConfig::new(0.65, 0.40)),
        ("nim_import", EdgeWeightConfig::new(0.65, 0.40)),
        ("nim_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("nim_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("julia_using", EdgeWeightConfig::new(0.65, 0.40)),
        ("julia_include", EdgeWeightConfig::new(0.60, 0.40)),
        ("julia_type", EdgeWeightConfig::new(0.60, 0.40)),
        ("julia_fn", EdgeWeightConfig::new(0.55, 0.40)),
        ("ansible_include", EdgeWeightConfig::new(0.60, 0.40)),
        ("ansible_role", EdgeWeightConfig::new(0.55, 0.40)),
        ("bazel_deps", EdgeWeightConfig::new(0.65, 0.40)),
        ("bazel_load", EdgeWeightConfig::new(0.60, 0.40)),
        ("bazel_srcs", EdgeWeightConfig::new(0.55, 0.40)),
        ("nix_import", EdgeWeightConfig::new(0.60, 0.40)),
        ("dbt_ref", EdgeWeightConfig::new(0.65, 0.40)),
        ("dbt_source", EdgeWeightConfig::new(0.60, 0.40)),
        ("dbt_macro", EdgeWeightConfig::new(0.55, 0.40)),
        ("latex_input", EdgeWeightConfig::new(0.65, 0.40)),
        ("latex_package", EdgeWeightConfig::new(0.55, 0.40)),
        ("latex_bib", EdgeWeightConfig::new(0.55, 0.40)),
        ("prisma_schema", EdgeWeightConfig::new(0.65, 0.40)),
        ("prisma_client", EdgeWeightConfig::new(0.55, 0.40)),
    ];
    entries.iter().copied().collect()
});

#[derive(Debug, Clone, Copy)]
pub struct LangWeights {
    pub call: f64,
    pub symbol_ref: f64,
    pub type_ref: f64,
    pub lexical_min: f64,
    pub lexical_max: f64,
}

impl LangWeights {
    pub const fn new(
        call: f64,
        symbol_ref: f64,
        type_ref: f64,
        lexical_min: f64,
        lexical_max: f64,
    ) -> Self {
        Self {
            call,
            symbol_ref,
            type_ref,
            lexical_min,
            lexical_max,
        }
    }
}

pub static DEFAULT_LANG_WEIGHTS: LangWeights = LangWeights {
    call: 0.55,
    symbol_ref: 0.60,
    type_ref: 0.50,
    lexical_min: 0.08,
    lexical_max: 0.15,
};

pub static LANG_WEIGHTS: Lazy<FxHashMap<&'static str, LangWeights>> = Lazy::new(|| {
    let entries: &[(&str, LangWeights)] = &[
        ("python", LangWeights::new(0.65, 0.70, 0.50, 0.10, 0.20)),
        ("javascript", LangWeights::new(0.50, 0.55, 0.45, 0.10, 0.20)),
        ("jsx", LangWeights::new(0.50, 0.55, 0.45, 0.10, 0.20)),
        ("typescript", LangWeights::new(0.70, 0.75, 0.65, 0.10, 0.18)),
        ("tsx", LangWeights::new(0.70, 0.75, 0.65, 0.10, 0.18)),
        ("rust", LangWeights::new(0.90, 0.95, 0.85, 0.05, 0.10)),
        ("java", LangWeights::new(0.85, 0.90, 0.80, 0.05, 0.10)),
        ("kotlin", LangWeights::new(0.80, 0.85, 0.75, 0.05, 0.12)),
        ("scala", LangWeights::new(0.80, 0.85, 0.75, 0.05, 0.12)),
        ("go", LangWeights::new(0.80, 0.85, 0.75, 0.05, 0.12)),
        ("c", LangWeights::new(0.60, 0.65, 0.55, 0.08, 0.15)),
        ("cpp", LangWeights::new(0.65, 0.70, 0.60, 0.08, 0.15)),
        ("csharp", LangWeights::new(0.75, 0.80, 0.70, 0.05, 0.12)),
        ("fsharp", LangWeights::new(0.70, 0.75, 0.65, 0.05, 0.12)),
        ("ruby", LangWeights::new(0.60, 0.65, 0.55, 0.08, 0.15)),
        ("php", LangWeights::new(0.60, 0.65, 0.55, 0.08, 0.15)),
        ("shell", LangWeights::new(0.40, 0.45, 0.35, 0.10, 0.18)),
        ("swift", LangWeights::new(0.75, 0.80, 0.70, 0.05, 0.12)),
    ];
    entries.iter().copied().collect()
});
