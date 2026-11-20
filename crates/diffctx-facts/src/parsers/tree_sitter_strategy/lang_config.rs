//! The tree-sitter grammar behind each file extension, and the node types that
//! count as definitions in it.

use std::path::Path;

pub(super) struct LangConfig {
    pub(super) extensions: &'static [&'static str],
    pub(super) ts_name: &'static str,
    pub(super) definition_types: &'static [&'static str],
}

const JS_DEFINITION_TYPES: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "method_definition",
    "arrow_function",
    "variable_declarator",
];

const TS_DEFINITION_TYPES: &[&str] = &[
    "function_declaration",
    "generator_function_declaration",
    "class_declaration",
    "method_definition",
    "arrow_function",
    "interface_declaration",
    "type_alias_declaration",
    "enum_declaration",
    "variable_declarator",
];

pub(super) const LANG_CONFIGS: &[LangConfig] = &[
    LangConfig {
        extensions: &[".py", ".pyw", ".pyi"],
        ts_name: "python",
        definition_types: &[
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
    },
    LangConfig {
        extensions: &[".js", ".mjs", ".cjs"],
        ts_name: "javascript",
        definition_types: JS_DEFINITION_TYPES,
    },
    LangConfig {
        extensions: &[".jsx"],
        ts_name: "jsx",
        definition_types: JS_DEFINITION_TYPES,
    },
    LangConfig {
        extensions: &[".ts", ".mts", ".cts"],
        ts_name: "typescript",
        definition_types: TS_DEFINITION_TYPES,
    },
    LangConfig {
        extensions: &[".tsx"],
        ts_name: "tsx",
        definition_types: TS_DEFINITION_TYPES,
    },
    LangConfig {
        extensions: &[".go"],
        ts_name: "go",
        definition_types: &[
            "function_declaration",
            "method_declaration",
            "type_declaration",
            "const_declaration",
            "var_declaration",
        ],
    },
    LangConfig {
        extensions: &[".rs"],
        ts_name: "rust",
        definition_types: &[
            "function_item",
            "impl_item",
            "struct_item",
            "enum_item",
            "trait_item",
            "mod_item",
            "const_item",
            "static_item",
            "macro_definition",
            "type_item",
        ],
    },
    LangConfig {
        extensions: &[".java"],
        ts_name: "java",
        definition_types: &[
            "method_declaration",
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "constructor_declaration",
        ],
    },
    LangConfig {
        extensions: &[".c", ".h"],
        ts_name: "c",
        definition_types: &[
            "function_definition",
            "struct_specifier",
            "enum_specifier",
            "declaration",
            "type_definition",
        ],
    },
    LangConfig {
        extensions: &[
            ".cpp", ".cc", ".cxx", ".hpp", ".hh", ".hxx", ".c++", ".h++", ".ipp", ".tpp",
        ],
        ts_name: "cpp",
        definition_types: &[
            "function_definition",
            "class_specifier",
            "struct_specifier",
            "enum_specifier",
            "declaration",
            "type_definition",
            "using_declaration",
            "alias_declaration",
        ],
    },
    LangConfig {
        extensions: &[".rb", ".rake", ".gemspec"],
        ts_name: "ruby",
        definition_types: &["method", "class", "module", "singleton_method"],
    },
    LangConfig {
        extensions: &[".cs"],
        ts_name: "c_sharp",
        definition_types: &[
            "method_declaration",
            "class_declaration",
            "interface_declaration",
            "struct_declaration",
            "enum_declaration",
            "record_declaration",
            "property_declaration",
            "constructor_declaration",
        ],
    },
    LangConfig {
        extensions: &[
            ".php", ".phtml", ".php3", ".php4", ".php5", ".php7", ".phps",
        ],
        ts_name: "php",
        definition_types: &[
            "function_definition",
            "method_declaration",
            "class_declaration",
            "interface_declaration",
            "trait_declaration",
            "enum_declaration",
        ],
    },
    LangConfig {
        extensions: &[".scala", ".sc"],
        ts_name: "scala",
        definition_types: &[
            "class_definition",
            "object_definition",
            "trait_definition",
            "function_definition",
            "function_declaration",
        ],
    },
    LangConfig {
        extensions: &[".swift"],
        ts_name: "swift",
        definition_types: &[
            "class_declaration",
            "protocol_declaration",
            "function_declaration",
            "protocol_function_declaration",
        ],
    },
    LangConfig {
        extensions: &[".kt", ".kts"],
        ts_name: "kotlin",
        definition_types: &[
            "class_declaration",
            "object_declaration",
            "companion_object",
            "function_declaration",
            "secondary_constructor",
            "property_declaration",
            "type_alias",
        ],
    },
    LangConfig {
        extensions: &[".sh", ".bash", ".zsh", ".ksh"],
        ts_name: "bash",
        definition_types: &["function_definition"],
    },
    LangConfig {
        extensions: &[".css", ".scss", ".less"],
        ts_name: "css",
        definition_types: &[
            "rule_set",
            "media_statement",
            "keyframes_statement",
            "import_statement",
        ],
    },
    LangConfig {
        extensions: &[".hs", ".lhs"],
        ts_name: "haskell",
        definition_types: &[
            "function",
            // The grammar's own spelling up to 0.23; 0.24 corrects it.
            "type_synomym",
            "type_synonym",
            "newtype",
            "data_type",
            "class",
            "instance",
            "signature",
        ],
    },
    LangConfig {
        extensions: &[".ex", ".exs"],
        ts_name: "elixir",
        definition_types: &["call"],
    },
    LangConfig {
        extensions: &[".lua"],
        ts_name: "lua",
        definition_types: &["function_declaration", "function_definition"],
    },
    LangConfig {
        extensions: &[".r"],
        ts_name: "r",
        definition_types: &["function_definition"],
    },
    LangConfig {
        extensions: &[".ml", ".mli"],
        ts_name: "ocaml",
        definition_types: &[
            "let_binding",
            "type_definition",
            "module_definition",
            "module_type_definition",
            "value_definition",
        ],
    },
    LangConfig {
        extensions: &[".erl", ".hrl"],
        ts_name: "erlang",
        definition_types: &[
            "function_clause",
            "type_spec",
            "attribute",
            "type_alias",
            "opaque",
        ],
    },
    LangConfig {
        extensions: &[".jl"],
        ts_name: "julia",
        definition_types: &[
            "function_definition",
            "short_function_definition",
            "macro_definition",
            "struct_definition",
            "abstract_definition",
            "module_definition",
        ],
    },
    LangConfig {
        extensions: &[".zig"],
        ts_name: "zig",
        definition_types: &[
            "function_declaration",
            "test_declaration",
            "variable_declaration",
            "struct_declaration",
            "enum_declaration",
            "union_declaration",
        ],
    },
    LangConfig {
        extensions: &[".clj", ".cljs", ".cljc"],
        ts_name: "clojure",
        definition_types: &["list_lit"],
    },
    LangConfig {
        extensions: &[".nix"],
        ts_name: "nix",
        definition_types: &["binding", "inherit"],
    },
    LangConfig {
        extensions: &[".groovy", ".gradle"],
        ts_name: "groovy",
        definition_types: &["method_declaration", "class_declaration", "closure"],
    },
    LangConfig {
        extensions: &[".m", ".mm"],
        ts_name: "objc",
        definition_types: &[
            "class_interface",
            "class_implementation",
            "method_declaration",
            "protocol_declaration",
            "category_interface",
            "category_implementation",
        ],
    },
    LangConfig {
        extensions: &[".dart"],
        ts_name: "dart",
        definition_types: &[
            "class_declaration",
            "function_signature",
            "method_signature",
            "enum_declaration",
            "extension_declaration",
            "mixin_declaration",
        ],
    },
    LangConfig {
        extensions: &[".graphql", ".gql"],
        ts_name: "graphql",
        definition_types: &[
            "object_type_definition",
            "interface_type_definition",
            "enum_type_definition",
            "input_object_type_definition",
            "union_type_definition",
            "scalar_type_definition",
        ],
    },
    LangConfig {
        extensions: &[".prisma"],
        ts_name: "prisma",
        definition_types: &[
            "model_declaration",
            "enum_declaration",
            "type_declaration",
            "generator_declaration",
            "datasource_declaration",
        ],
    },
    LangConfig {
        extensions: &[".svelte"],
        ts_name: "svelte",
        definition_types: &["script_element", "style_element", "element"],
    },
    LangConfig {
        extensions: &[".tf", ".hcl"],
        ts_name: "hcl",
        definition_types: &["block"],
    },
    LangConfig {
        extensions: &[".html", ".htm", ".vue"],
        ts_name: "html",
        definition_types: &["element", "script_element", "style_element"],
    },
    LangConfig {
        extensions: &[".json"],
        ts_name: "json",
        definition_types: &["pair"],
    },
    LangConfig {
        extensions: &[".yaml", ".yml"],
        ts_name: "yaml",
        definition_types: &["block_mapping_pair"],
    },
    LangConfig {
        extensions: &[".cmake"],
        ts_name: "cmake",
        definition_types: &["function_def", "macro_def", "if_condition", "foreach_loop"],
    },
    LangConfig {
        extensions: &[".mk"],
        ts_name: "make",
        definition_types: &["rule"],
    },
];

fn file_extension(path: &str) -> &str {
    if let Some(dot_pos) = path.rfind('.') {
        &path[dot_pos..]
    } else {
        ""
    }
}

pub(super) fn find_lang_config(path: &str) -> Option<&'static LangConfig> {
    let ext = file_extension(path).to_ascii_lowercase();
    if let Some(config) = LANG_CONFIGS
        .iter()
        .find(|c| c.extensions.contains(&ext.as_str()))
    {
        return Some(config);
    }
    find_lang_config_by_filename(path)
}

// CMakeLists.txt and Makefile/GNUmakefile have compiled-in grammars
// (LANG_CONFIGS ts_name "cmake"/"make") but no extension of their own — the
// dot-suffix lookup above yields ".txt" and "" respectively, so they never
// matched. Reuse the crate's shared filename map instead of hand-rolling a
// second filename list here; "makefile" is normalized to the "make" ts_name
// because languages::FILENAME_TO_LANGUAGE keeps the friendlier external
// string ("makefile") for the markdown-code-fence / discoverability surface.
pub(super) fn find_lang_config_for(path: &str, content: &str) -> Option<&'static LangConfig> {
    match diffctx_base::languages::sniff_language(path, content) {
        Some(language) => LANG_CONFIGS.iter().find(|c| c.ts_name == language),
        None => find_lang_config(path),
    }
}

fn find_lang_config_by_filename(path: &str) -> Option<&'static LangConfig> {
    let name_lower = Path::new(path)
        .file_name()?
        .to_string_lossy()
        .to_lowercase();
    let language = *diffctx_base::languages::FILENAME_TO_LANGUAGE.get(name_lower.as_str())?;
    let ts_name = if language == "makefile" {
        "make"
    } else {
        language
    };
    LANG_CONFIGS.iter().find(|c| c.ts_name == ts_name)
}
