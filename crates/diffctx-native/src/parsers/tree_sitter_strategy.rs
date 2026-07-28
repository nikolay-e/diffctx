use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;

use once_cell::sync::Lazy;
use rustc_hash::{FxHashMap, FxHashSet};
use std::time::{Duration, Instant};
use tree_sitter::{Language, Node, ParseOptions, Parser, Tree};

use crate::config::parsers::PARSERS;
use crate::config::tokenization::TOKENIZATION;
use crate::types::{Fragment, FragmentId, FragmentKind, extract_identifiers};

use super::{FragmentationStrategy, create_code_gap_fragments, create_snippet};

const BODY_FIELD_NAMES: &[&str] = &["body", "block", "consequence"];
const BODY_NODE_TYPES: &[&str] = &[
    "block",
    "statement_block",
    "compound_statement",
    "function_body",
];

struct LangConfig {
    extension: &'static str,
    ts_name: &'static str,
    definition_types: &'static [&'static str],
}

const LANG_CONFIGS: &[LangConfig] = &[
    LangConfig {
        extension: ".py",
        ts_name: "python",
        definition_types: &[
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
    },
    LangConfig {
        extension: ".pyw",
        ts_name: "python",
        definition_types: &[
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
    },
    LangConfig {
        extension: ".pyi",
        ts_name: "python",
        definition_types: &[
            "function_definition",
            "class_definition",
            "decorated_definition",
        ],
    },
    LangConfig {
        extension: ".js",
        ts_name: "javascript",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".mjs",
        ts_name: "javascript",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".cjs",
        ts_name: "javascript",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".jsx",
        ts_name: "jsx",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".ts",
        ts_name: "typescript",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".mts",
        ts_name: "typescript",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".cts",
        ts_name: "typescript",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".tsx",
        ts_name: "tsx",
        definition_types: &[
            "function_declaration",
            "class_declaration",
            "method_definition",
            "arrow_function",
            "interface_declaration",
            "type_alias_declaration",
            "enum_declaration",
            "variable_declarator",
        ],
    },
    LangConfig {
        extension: ".go",
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
        extension: ".rs",
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
        extension: ".java",
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
        extension: ".c",
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
        extension: ".h",
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
        extension: ".cpp",
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
        extension: ".cc",
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
        extension: ".cxx",
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
        extension: ".hpp",
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
        extension: ".hh",
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
        extension: ".hxx",
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
        extension: ".rb",
        ts_name: "ruby",
        definition_types: &["method", "class", "module", "singleton_method"],
    },
    LangConfig {
        extension: ".rake",
        ts_name: "ruby",
        definition_types: &["method", "class", "module", "singleton_method"],
    },
    LangConfig {
        extension: ".cs",
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
        extension: ".php",
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
        extension: ".scala",
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
        extension: ".sc",
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
        extension: ".swift",
        ts_name: "swift",
        definition_types: &[
            "class_declaration",
            "protocol_declaration",
            "function_declaration",
            "protocol_function_declaration",
        ],
    },
    // --- Ruby extra extensions ---
    LangConfig {
        extension: ".gemspec",
        ts_name: "ruby",
        definition_types: &["method", "class", "module", "singleton_method"],
    },
    // --- Bash/Shell ---
    LangConfig {
        extension: ".sh",
        ts_name: "bash",
        definition_types: &["function_definition"],
    },
    LangConfig {
        extension: ".bash",
        ts_name: "bash",
        definition_types: &["function_definition"],
    },
    LangConfig {
        extension: ".zsh",
        ts_name: "bash",
        definition_types: &["function_definition"],
    },
    LangConfig {
        extension: ".ksh",
        ts_name: "bash",
        definition_types: &["function_definition"],
    },
    // --- CSS ---
    LangConfig {
        extension: ".css",
        ts_name: "css",
        definition_types: &[
            "rule_set",
            "media_statement",
            "keyframes_statement",
            "import_statement",
        ],
    },
    LangConfig {
        extension: ".scss",
        ts_name: "css",
        definition_types: &[
            "rule_set",
            "media_statement",
            "keyframes_statement",
            "import_statement",
        ],
    },
    LangConfig {
        extension: ".less",
        ts_name: "css",
        definition_types: &[
            "rule_set",
            "media_statement",
            "keyframes_statement",
            "import_statement",
        ],
    },
    // --- Haskell ---
    LangConfig {
        extension: ".hs",
        ts_name: "haskell",
        definition_types: &[
            "function",
            "type_synomym",
            "newtype",
            "data_type",
            "class",
            "instance",
            "signature",
        ],
    },
    LangConfig {
        extension: ".lhs",
        ts_name: "haskell",
        definition_types: &[
            "function",
            "type_synomym",
            "newtype",
            "data_type",
            "class",
            "instance",
            "signature",
        ],
    },
    // --- Elixir ---
    LangConfig {
        extension: ".ex",
        ts_name: "elixir",
        definition_types: &["call"],
    },
    LangConfig {
        extension: ".exs",
        ts_name: "elixir",
        definition_types: &["call"],
    },
    // --- Lua ---
    LangConfig {
        extension: ".lua",
        ts_name: "lua",
        definition_types: &["function_declaration", "function_definition"],
    },
    // --- R ---
    LangConfig {
        extension: ".r",
        ts_name: "r",
        definition_types: &["function_definition"],
    },
    // --- OCaml ---
    LangConfig {
        extension: ".ml",
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
        extension: ".mli",
        ts_name: "ocaml",
        definition_types: &[
            "let_binding",
            "type_definition",
            "module_definition",
            "module_type_definition",
            "value_definition",
        ],
    },
    // --- Erlang ---
    LangConfig {
        extension: ".erl",
        ts_name: "erlang",
        definition_types: &["function_clause", "type_spec", "attribute"],
    },
    LangConfig {
        extension: ".hrl",
        ts_name: "erlang",
        definition_types: &["function_clause", "type_spec", "attribute"],
    },
    // --- Julia ---
    LangConfig {
        extension: ".jl",
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
    // --- Zig ---
    LangConfig {
        extension: ".zig",
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
    // --- Clojure ---
    LangConfig {
        extension: ".clj",
        ts_name: "clojure",
        definition_types: &["list_lit"],
    },
    LangConfig {
        extension: ".cljs",
        ts_name: "clojure",
        definition_types: &["list_lit"],
    },
    LangConfig {
        extension: ".cljc",
        ts_name: "clojure",
        definition_types: &["list_lit"],
    },
    // --- Nix ---
    LangConfig {
        extension: ".nix",
        ts_name: "nix",
        definition_types: &["binding", "inherit"],
    },
    // --- Groovy ---
    LangConfig {
        extension: ".groovy",
        ts_name: "groovy",
        definition_types: &["method_declaration", "class_declaration", "closure"],
    },
    LangConfig {
        extension: ".gradle",
        ts_name: "groovy",
        definition_types: &["method_declaration", "class_declaration", "closure"],
    },
    // --- Objective-C ---
    LangConfig {
        extension: ".m",
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
        extension: ".mm",
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
    // --- Dart ---
    LangConfig {
        extension: ".dart",
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
    // --- GraphQL ---
    LangConfig {
        extension: ".graphql",
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
        extension: ".gql",
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
    // LaTeX: tree-sitter-latex crate is broken, using generic parser instead
    // LangConfig { extension: ".tex", ts_name: "latex", ... },
    // LangConfig { extension: ".latex", ts_name: "latex", ... },
    // LangConfig { extension: ".sty", ts_name: "latex", ... },
    // LangConfig { extension: ".cls", ts_name: "latex", ... }
    // --- Prisma ---
    LangConfig {
        extension: ".prisma",
        ts_name: "prisma",
        definition_types: &[
            "model_declaration",
            "enum_declaration",
            "type_declaration",
            "generator_declaration",
            "datasource_declaration",
        ],
    },
    // --- Svelte ---
    LangConfig {
        extension: ".svelte",
        ts_name: "svelte",
        definition_types: &["script_element", "style_element", "element"],
    },
    // --- HCL / Terraform ---
    LangConfig {
        extension: ".tf",
        ts_name: "hcl",
        definition_types: &["block"],
    },
    LangConfig {
        extension: ".hcl",
        ts_name: "hcl",
        definition_types: &["block"],
    },
    // --- HTML ---
    LangConfig {
        extension: ".html",
        ts_name: "html",
        definition_types: &["element", "script_element", "style_element"],
    },
    LangConfig {
        extension: ".htm",
        ts_name: "html",
        definition_types: &["element", "script_element", "style_element"],
    },
    // --- JSON ---
    LangConfig {
        extension: ".json",
        ts_name: "json",
        definition_types: &["pair"],
    },
    // --- YAML ---
    LangConfig {
        extension: ".yaml",
        ts_name: "yaml",
        definition_types: &["block_mapping_pair"],
    },
    LangConfig {
        extension: ".yml",
        ts_name: "yaml",
        definition_types: &["block_mapping_pair"],
    },
    // --- CMake ---
    LangConfig {
        extension: ".cmake",
        ts_name: "cmake",
        definition_types: &["function_def", "macro_def", "if_condition", "foreach_loop"],
    },
    // --- PHP extra extensions ---
    LangConfig {
        extension: ".phtml",
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
    // --- C++ extra extensions ---
    LangConfig {
        extension: ".c++",
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
        extension: ".h++",
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
        extension: ".ipp",
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
        extension: ".tpp",
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
    // --- Makefile ---
    LangConfig {
        extension: ".mk",
        ts_name: "make",
        definition_types: &["rule"],
    },
];

const NODE_TYPE_KEYWORDS: &[(&[&str], &str)] = &[
    (
        &[
            "function",
            "method",
            "subroutine",
            "FnDecl",
            "short_function",
        ],
        "function",
    ),
    (
        &[
            "class",
            "object_definition",
            "class_interface",
            "class_implementation",
            "category_interface",
            "category_implementation",
        ],
        "class",
    ),
    (&["struct", "struct_definition", "ContainerDecl"], "struct"),
    (&["impl"], "impl"),
    (
        &[
            "trait",
            "interface",
            "protocol",
            "protocol_declaration",
            "mixin",
        ],
        "interface",
    ),
    (&["enum", "adt", "newtype"], "enum"),
    (
        &[
            "module",
            "module_definition",
            "abstract_definition",
            "package",
        ],
        "module",
    ),
    (
        &[
            "type_alias",
            "alias_declaration",
            "type_definition",
            "type_declaration",
        ],
        "type",
    ),
    (
        &[
            "variable_declarator",
            "VarDecl",
            "let_binding",
            "binding",
            "left_assignment",
        ],
        "variable",
    ),
    (
        &[
            "record",
            "model_declaration",
            "datasource_declaration",
            "generator_declaration",
        ],
        "record",
    ),
    (&["property", "property_declaration"], "property"),
    (
        &[
            "declaration",
            "using_declaration",
            "attribute",
            "type_spec",
            "signature",
            "instance",
        ],
        "declaration",
    ),
    (&["rule_set", "rule"], "definition"),
    (&["block"], "definition"),
    (&["section", "subsection"], "definition"),
    (&["environment", "new_command_definition"], "definition"),
    (
        &["element", "script_element", "style_element"],
        "definition",
    ),
    (&["pair", "block_mapping_pair"], "definition"),
    (&["TestDecl"], "definition"),
    (&["closure"], "function"),
    (&["call", "list_lit"], "definition"),
    (&["extension_declaration"], "definition"),
    (
        &["macro_definition", "macro_def", "function_def"],
        "function",
    ),
    (&["if_condition", "foreach_loop"], "definition"),
    (
        &["media_statement", "keyframes_statement", "import_statement"],
        "definition",
    ),
    (
        &[
            "object_type_definition",
            "interface_type_definition",
            "enum_type_definition",
            "input_object_type_definition",
            "union_type_definition",
            "scalar_type_definition",
        ],
        "type",
    ),
    (&["singleton_method"], "function"),
    (&["inherit"], "declaration"),
];

const CONTAINER_KINDS: &[&str] = &["class", "interface", "struct", "impl"];
const FUNCTION_CHILD_TYPES: &[&str] = &["arrow_function", "function", "generator_function"];

fn file_extension(path: &str) -> &str {
    if let Some(dot_pos) = path.rfind('.') {
        &path[dot_pos..]
    } else {
        ""
    }
}

fn find_lang_config(path: &str) -> Option<&'static LangConfig> {
    let ext = file_extension(path).to_ascii_lowercase();
    if let Some(config) = LANG_CONFIGS.iter().find(|c| c.extension == ext) {
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
fn find_lang_config_by_filename(path: &str) -> Option<&'static LangConfig> {
    let name_lower = Path::new(path)
        .file_name()?
        .to_string_lossy()
        .to_lowercase();
    let language = *crate::languages::FILENAME_TO_LANGUAGE.get(name_lower.as_str())?;
    let ts_name = if language == "makefile" {
        "make"
    } else {
        language
    };
    LANG_CONFIGS.iter().find(|c| c.ts_name == ts_name)
}

static LANGUAGE_CACHE: Lazy<FxHashMap<&'static str, Language>> = Lazy::new(|| {
    #[allow(unused_mut)]
    let mut m: FxHashMap<&'static str, Language> = FxHashMap::default();

    #[cfg(feature = "tree-sitter-python")]
    m.insert("python", Language::new(tree_sitter_python::LANGUAGE));

    #[cfg(feature = "tree-sitter-javascript")]
    {
        let js = Language::new(tree_sitter_javascript::LANGUAGE);
        m.insert("javascript", js.clone());
        m.insert("jsx", js);
    }

    #[cfg(feature = "tree-sitter-typescript")]
    {
        m.insert(
            "typescript",
            Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
        );
        m.insert("tsx", Language::new(tree_sitter_typescript::LANGUAGE_TSX));
    }

    #[cfg(feature = "tree-sitter-go")]
    m.insert("go", Language::new(tree_sitter_go::LANGUAGE));

    #[cfg(feature = "tree-sitter-rust")]
    m.insert("rust", Language::new(tree_sitter_rust::LANGUAGE));

    #[cfg(feature = "tree-sitter-java")]
    m.insert("java", Language::new(tree_sitter_java::LANGUAGE));

    #[cfg(feature = "tree-sitter-c")]
    m.insert("c", Language::new(tree_sitter_c::LANGUAGE));

    #[cfg(feature = "tree-sitter-cpp")]
    m.insert("cpp", Language::new(tree_sitter_cpp::LANGUAGE));

    #[cfg(feature = "tree-sitter-ruby")]
    m.insert("ruby", Language::new(tree_sitter_ruby::LANGUAGE));

    #[cfg(feature = "tree-sitter-c-sharp")]
    m.insert("c_sharp", Language::new(tree_sitter_c_sharp::LANGUAGE));

    #[cfg(feature = "tree-sitter-php")]
    m.insert("php", Language::new(tree_sitter_php::LANGUAGE_PHP));

    #[cfg(feature = "tree-sitter-scala")]
    m.insert("scala", Language::new(tree_sitter_scala::LANGUAGE));

    #[cfg(feature = "tree-sitter-swift")]
    m.insert("swift", Language::new(tree_sitter_swift::LANGUAGE));

    #[cfg(feature = "tree-sitter-html")]
    m.insert("html", Language::new(tree_sitter_html::LANGUAGE));

    #[cfg(feature = "tree-sitter-bash")]
    m.insert("bash", Language::new(tree_sitter_bash::LANGUAGE));

    #[cfg(feature = "tree-sitter-css")]
    m.insert("css", Language::new(tree_sitter_css::LANGUAGE));

    #[cfg(feature = "tree-sitter-haskell")]
    m.insert("haskell", Language::new(tree_sitter_haskell::LANGUAGE));

    #[cfg(feature = "tree-sitter-elixir")]
    m.insert("elixir", Language::new(tree_sitter_elixir::LANGUAGE));

    #[cfg(feature = "tree-sitter-lua")]
    m.insert("lua", Language::new(tree_sitter_lua::LANGUAGE));

    #[cfg(feature = "tree-sitter-r")]
    m.insert("r", Language::new(tree_sitter_r::LANGUAGE));

    #[cfg(feature = "tree-sitter-ocaml")]
    m.insert("ocaml", Language::new(tree_sitter_ocaml::LANGUAGE_OCAML));

    #[cfg(feature = "tree-sitter-erlang")]
    m.insert("erlang", Language::new(tree_sitter_erlang::LANGUAGE));

    #[cfg(feature = "tree-sitter-julia")]
    m.insert("julia", Language::new(tree_sitter_julia::LANGUAGE));

    #[cfg(feature = "tree-sitter-zig")]
    m.insert("zig", Language::new(tree_sitter_zig::LANGUAGE));

    #[cfg(feature = "tree-sitter-clojure")]
    m.insert("clojure", Language::new(tree_sitter_clojure::LANGUAGE));

    #[cfg(feature = "tree-sitter-nix")]
    m.insert("nix", Language::new(tree_sitter_nix::LANGUAGE));

    #[cfg(feature = "tree-sitter-groovy")]
    m.insert("groovy", Language::new(tree_sitter_groovy::LANGUAGE));

    #[cfg(feature = "tree-sitter-objc")]
    m.insert("objc", Language::new(tree_sitter_objc::LANGUAGE));

    #[cfg(feature = "tree-sitter-cmake")]
    m.insert("cmake", Language::new(tree_sitter_cmake::LANGUAGE));

    #[cfg(feature = "tree-sitter-make")]
    m.insert("make", Language::new(tree_sitter_make::LANGUAGE));

    #[cfg(feature = "tree-sitter-hcl")]
    m.insert("hcl", Language::new(tree_sitter_hcl::LANGUAGE));

    #[cfg(feature = "tree-sitter-graphql")]
    m.insert("graphql", Language::new(tree_sitter_graphql::LANGUAGE));

    #[cfg(feature = "tree-sitter-dart")]
    m.insert("dart", Language::new(tree_sitter_dart::LANGUAGE));

    #[cfg(feature = "tree-sitter-prisma-io")]
    m.insert("prisma", Language::new(tree_sitter_prisma_io::LANGUAGE));

    #[cfg(feature = "tree-sitter-svelte-ng")]
    m.insert("svelte", Language::new(tree_sitter_svelte_ng::LANGUAGE));

    #[cfg(feature = "tree-sitter-json")]
    m.insert("json", Language::new(tree_sitter_json::LANGUAGE));

    #[cfg(feature = "tree-sitter-yaml")]
    m.insert("yaml", Language::new(tree_sitter_yaml::LANGUAGE));

    m
});

fn get_tree_sitter_language(ts_name: &str) -> Option<Language> {
    LANGUAGE_CACHE.get(ts_name).cloned()
}

thread_local! {
    static PARSER_CACHE: RefCell<FxHashMap<&'static str, Parser>> = RefCell::new(FxHashMap::default());
}

/// Wall-clock deadline for a single tree-sitter parse. Pathological inputs
/// (huge minified bundles, deeply ambiguous grammars) can otherwise pin a
/// rayon worker indefinitely.
const PARSE_TIMEOUT: Duration = Duration::from_secs(2);

fn parse_with_cached_parser(
    ts_name: &'static str,
    language: &Language,
    content: &str,
) -> Option<Tree> {
    PARSER_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        let parser = match cache.get_mut(ts_name) {
            Some(p) => p,
            None => {
                let mut p = Parser::new();
                if p.set_language(language).is_err() {
                    return None;
                }
                cache.insert(ts_name, p);
                cache.get_mut(ts_name).expect("just inserted")
            }
        };
        // tree-sitter convention: progress_callback returns `true` to abort.
        // We abort once the wall-clock deadline has passed.
        let deadline = Instant::now() + PARSE_TIMEOUT;
        let mut progress =
            move |_state: &tree_sitter::ParseState| -> bool { Instant::now() >= deadline };
        let bytes = content.as_bytes();
        let len = bytes.len();
        parser.parse_with_options(
            &mut |i, _| (i < len).then(|| &bytes[i..]).unwrap_or_default(),
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
    })
}

// Additive observability probe: the fragmentation pipeline itself never
// inspects `Node::has_error`, so a mostly-ERROR tree (unbalanced brace,
// stray merge-conflict marker, truncated mid-refactor file) is currently
// indistinguishable from a clean parse to every caller -- both just
// produce a Vec<Fragment>. This exposes that signal without changing any
// existing return path; nothing in `fragment()` calls it yet.
#[allow(dead_code)]
pub(crate) fn parse_has_error(path: &str, content: &str) -> Option<bool> {
    let config = find_lang_config(path)?;
    let language = get_tree_sitter_language(config.ts_name)?;
    let tree = parse_with_cached_parser(config.ts_name, &language, content)?;
    Some(tree.root_node().has_error())
}

fn node_start_line(node: &Node) -> u32 {
    node.start_position().row as u32 + 1
}

fn node_end_line(node: &Node) -> u32 {
    node.end_position().row as u32 + 1
}

fn is_definition_type(node_type: &str, definition_types: &[&str]) -> bool {
    definition_types.contains(&node_type)
}

fn node_type_to_kind(node_type: &str, node: Option<&Node>) -> &'static str {
    if node_type == "decorated_definition" {
        if let Some(n) = node {
            return decorated_definition_kind(n);
        }
    }
    for &(keywords, kind) in NODE_TYPE_KEYWORDS {
        if keywords.iter().any(|kw| node_type.contains(kw)) {
            return kind;
        }
    }
    "definition"
}

fn decorated_definition_kind(node: &Node) -> &'static str {
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "function_definition" | "async_function_definition" => return "function",
                "class_definition" => return "class",
                _ => {}
            }
        }
    }
    "function"
}

fn is_container_kind(kind: &str) -> bool {
    CONTAINER_KINDS.contains(&kind)
}

fn adjust_start_for_ancestor(node: &Node, start: u32) -> u32 {
    let mut ancestor = node.parent();
    if let Some(ref a) = ancestor {
        let a_type = a.kind();
        if a_type != "export_statement" && a_type != "decorated_definition" {
            ancestor = a.parent();
        }
    }
    if let Some(a) = ancestor {
        let a_type = a.kind();
        if a_type == "export_statement" || a_type == "decorated_definition" {
            let ancestor_start = node_start_line(&a);
            if ancestor_start < start {
                return ancestor_start;
            }
        }
    }
    start
}

fn unwrap_decorated<'a>(node: Node<'a>) -> Node<'a> {
    if node.kind() != "decorated_definition" {
        return node;
    }
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            match child.kind() {
                "function_definition" | "class_definition" | "async_function_definition" => {
                    return child;
                }
                _ => {}
            }
        }
    }
    node
}

fn unwrap_declarator<'a>(mut name_node: Node<'a>) -> Node<'a> {
    loop {
        match name_node.kind() {
            "pointer_declarator"
            | "function_declarator"
            | "init_declarator"
            | "array_declarator" => {
                if let Some(inner) = name_node.child_by_field_name("declarator") {
                    name_node = inner;
                } else {
                    break;
                }
            }
            // C++ `reference_declarator` (`int &r`) and `parenthesized_declarator`
            // (`int (*f)()`) hold their inner declarator as a positional child,
            // NOT under a `declarator` field, so descend the first named
            // declarator/identifier child instead of querying the field.
            "reference_declarator" | "parenthesized_declarator" => {
                match (0..name_node.named_child_count())
                    .filter_map(|i| name_node.named_child(i))
                    .find(|c| {
                        let k = c.kind();
                        k.ends_with("declarator") || k.ends_with("identifier")
                    }) {
                    Some(inner) => name_node = inner,
                    None => break,
                }
            }
            _ => break,
        }
    }
    name_node
}

fn extract_symbol_name(node: &Node, source: &[u8]) -> Option<String> {
    let unwrapped = unwrap_decorated(*node);
    for field_name in &["name", "declarator", "type"] {
        if let Some(name_node) = unwrapped.child_by_field_name(field_name) {
            let name_node = unwrap_declarator(name_node);
            if name_node.kind() == "identifier" || name_node.named_child_count() == 0 {
                let text = &source[name_node.byte_range()];
                return std::str::from_utf8(text).ok().map(|s| s.to_string());
            }
        }
    }
    None
}

fn find_body_node<'a>(node: &Node<'a>) -> Option<Node<'a>> {
    for &field in BODY_FIELD_NAMES {
        if let Some(child) = node.child_by_field_name(field) {
            return Some(child);
        }
    }
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            if BODY_NODE_TYPES.iter().any(|&t| t == child.kind()) {
                return Some(child);
            }
        }
    }
    None
}

fn has_function_child(node: &Node) -> bool {
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            if FUNCTION_CHILD_TYPES.iter().any(|&t| t == child.kind()) {
                return true;
            }
        }
    }
    false
}

fn create_and_append_fragment(
    path: &Arc<str>,
    lines: &[&str],
    start: u32,
    end: u32,
    kind: &str,
    sym_name: Option<&str>,
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
) -> bool {
    let snippet = match create_snippet(lines, start, end) {
        Some(s) => s,
        None => return false,
    };
    let identifiers = extract_identifiers(&snippet, TOKENIZATION.fragment_min_identifier_length);
    fragments.push(Fragment {
        id: FragmentId::new(Arc::clone(path), start, end),
        kind: FragmentKind::from_str(kind),
        content: Arc::from(snippet),
        identifiers,
        token_count: 0,
        symbol_name: sym_name.map(|s| s.to_string()),
    });
    covered.push((start, end));
    true
}

fn emit_chunk(
    path: &Arc<str>,
    lines: &[&str],
    start: u32,
    end: u32,
    parent_symbol: Option<&str>,
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
) {
    if end < start || end - start + 1 < PARSERS.min_fragment_lines {
        return;
    }
    let sym_name = parent_symbol.map(|ps| format!("{ps}[{start}]"));
    create_and_append_fragment(
        path,
        lines,
        start,
        end,
        "chunk",
        sym_name.as_deref(),
        fragments,
        covered,
    );
}

fn create_sub_fragments(
    node: &Node,
    path: &Arc<str>,
    lines: &[&str],
    parent_symbol: Option<&str>,
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
    depth: u32,
) {
    if depth > PARSERS.max_sub_depth {
        return;
    }
    let body = match find_body_node(node) {
        Some(b) => b,
        None => return,
    };

    let named_count = body.named_child_count();
    let children: Vec<Node> = (0..named_count)
        .filter_map(|i| body.named_child(i))
        .filter(|c| c.end_position().row >= c.start_position().row)
        .collect();

    if children.len() < 2 {
        return;
    }

    let mut chunk_start_line = node_start_line(&children[0]);
    let mut chunk_end_line = node_end_line(&children[0]);

    for child in &children[1..] {
        let child_start = node_start_line(child);
        let child_end = node_end_line(child);
        if child_end - chunk_start_line + 1 > PARSERS.sub_fragment_target_lines {
            emit_chunk(
                path,
                lines,
                chunk_start_line,
                chunk_end_line,
                parent_symbol,
                fragments,
                covered,
            );
            chunk_start_line = child_start;
            chunk_end_line = child_end;
        } else {
            chunk_end_line = child_end;
        }
    }

    emit_chunk(
        path,
        lines,
        chunk_start_line,
        chunk_end_line,
        parent_symbol,
        fragments,
        covered,
    );
}

fn first_child_def_line(node: &Node, definition_types: &[&str], depth: u32) -> Option<u32> {
    if depth > PARSERS.container_search_max_depth {
        return None;
    }
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            if is_definition_type(child.kind(), definition_types) {
                return Some(node_start_line(&child));
            }
            if let Some(result) = first_child_def_line(&child, definition_types, depth + 1) {
                return Some(result);
            }
        }
    }
    None
}

fn try_container_split(
    node: &Node,
    source: &[u8],
    path: &Arc<str>,
    lines: &[&str],
    definition_types: &[&str],
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
    added_ends: &mut FxHashSet<(String, u32)>,
    depth: u32,
    start: u32,
    end: u32,
    kind: &str,
    sym_name: Option<&str>,
) -> bool {
    // For a `decorated_definition` the inner class/def node is itself a
    // definition type, so searching from the wrapper would report the header
    // line as the "first child" and truncate the container header to the
    // decorator alone. Search from the unwrapped definition instead.
    let search_root = unwrap_decorated(*node);
    let first_child_start = match first_child_def_line(&search_root, definition_types, 0) {
        Some(l) => l,
        None => return false,
    };
    if first_child_start <= start {
        return false;
    }
    let header_end = first_child_start - 1;
    if let Some(snippet) = create_snippet(lines, start, header_end) {
        let identifiers =
            extract_identifiers(&snippet, TOKENIZATION.fragment_min_identifier_length);
        fragments.push(Fragment {
            id: FragmentId::new(Arc::clone(path), start, header_end),
            kind: FragmentKind::from_str(kind),
            content: Arc::from(snippet),
            identifiers,
            token_count: 0,
            symbol_name: sym_name.map(|s| s.to_string()),
        });
        covered.push((start, header_end));
    }
    added_ends.insert((kind.to_string(), end));
    recurse_children(
        node,
        source,
        path,
        lines,
        definition_types,
        fragments,
        covered,
        added_ends,
        depth,
    );
    true
}

fn handle_definition_node(
    node: &Node,
    source: &[u8],
    path: &Arc<str>,
    lines: &[&str],
    definition_types: &[&str],
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
    added_ends: &mut FxHashSet<(String, u32)>,
    depth: u32,
) {
    let start = node_start_line(node);
    let end = node_end_line(node);
    let kind = node_type_to_kind(node.kind(), Some(node));

    if added_ends.contains(&(kind.to_string(), end)) {
        recurse_children(
            node,
            source,
            path,
            lines,
            definition_types,
            fragments,
            covered,
            added_ends,
            depth,
        );
        return;
    }

    let sym_name = extract_symbol_name(node, source);
    let start = adjust_start_for_ancestor(node, start);

    if is_container_kind(kind)
        && try_container_split(
            node,
            source,
            path,
            lines,
            definition_types,
            fragments,
            covered,
            added_ends,
            depth,
            start,
            end,
            kind,
            sym_name.as_deref(),
        )
    {
        return;
    }

    if end - start + 1 >= PARSERS.min_fragment_lines {
        if create_and_append_fragment(
            path,
            lines,
            start,
            end,
            kind,
            sym_name.as_deref(),
            fragments,
            covered,
        ) {
            added_ends.insert((kind.to_string(), end));
        }
    }

    if end - start + 1 > PARSERS.sub_fragment_threshold_lines {
        create_sub_fragments(
            node,
            path,
            lines,
            sym_name.as_deref(),
            fragments,
            covered,
            0,
        );
    }

    if node.kind() == "variable_declarator" && has_function_child(node) {
        return;
    }

    recurse_children(
        node,
        source,
        path,
        lines,
        definition_types,
        fragments,
        covered,
        added_ends,
        depth,
    );
}

fn extract_definitions(
    node: &Node,
    source: &[u8],
    path: &Arc<str>,
    lines: &[&str],
    definition_types: &[&str],
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
    added_ends: &mut FxHashSet<(String, u32)>,
    depth: u32,
) {
    if depth > PARSERS.max_recursion_depth {
        return;
    }

    if is_definition_type(node.kind(), definition_types) {
        handle_definition_node(
            node,
            source,
            path,
            lines,
            definition_types,
            fragments,
            covered,
            added_ends,
            depth,
        );
    } else {
        recurse_children(
            node,
            source,
            path,
            lines,
            definition_types,
            fragments,
            covered,
            added_ends,
            depth,
        );
    }
}

fn recurse_children(
    node: &Node,
    source: &[u8],
    path: &Arc<str>,
    lines: &[&str],
    definition_types: &[&str],
    fragments: &mut Vec<Fragment>,
    covered: &mut Vec<(u32, u32)>,
    added_ends: &mut FxHashSet<(String, u32)>,
    depth: u32,
) {
    let child_count = node.child_count();
    for i in 0..child_count {
        if let Some(child) = node.child(i) {
            extract_definitions(
                &child,
                source,
                path,
                lines,
                definition_types,
                fragments,
                covered,
                added_ends,
                depth + 1,
            );
        }
    }
}

pub struct TreeSitterStrategy {
    _private: (),
}

impl TreeSitterStrategy {
    pub fn new() -> Self {
        Self { _private: () }
    }
}

impl FragmentationStrategy for TreeSitterStrategy {
    fn can_handle(&self, path: &str, _content: &str) -> bool {
        find_lang_config(path).is_some()
    }

    fn fragment(&self, path: Arc<str>, content: &str) -> Vec<Fragment> {
        let config = match find_lang_config(&path) {
            Some(c) => c,
            None => return Vec::new(),
        };

        let language = match get_tree_sitter_language(config.ts_name) {
            Some(l) => l,
            None => return Vec::new(),
        };

        let tree = match parse_with_cached_parser(config.ts_name, &language, content) {
            Some(t) => t,
            None => return Vec::new(),
        };

        let source = content.as_bytes();
        let lines: Vec<&str> = content.split('\n').collect();

        let mut fragments: Vec<Fragment> = Vec::new();
        let mut covered: Vec<(u32, u32)> = Vec::new();
        let mut added_ends: FxHashSet<(String, u32)> = FxHashSet::default();

        extract_definitions(
            &tree.root_node(),
            source,
            &path,
            &lines,
            config.definition_types,
            &mut fragments,
            &mut covered,
            &mut added_ends,
            0,
        );

        let gap_frags = create_code_gap_fragments(Arc::clone(&path), &lines, &covered);
        fragments.extend(gap_frags);

        fragments
    }
}

#[cfg(test)]
mod grammar_tests {
    use super::*;
    use std::time::Instant;

    struct Case {
        ext: &'static str,
        source: &'static str,
        broken_tail: &'static str,
        expected_kind: FragmentKind,
        expected_symbol: Option<&'static str>,
    }

    // One representative construct per registered grammar (LANG_CONFIGS'
    // 37 unique ts_names). expected_kind/expected_symbol are the real,
    // observed extract_symbol_name/node_type_to_kind output for each
    // snippet -- this is a characterization oracle: it locks in today's
    // correct behavior so a regression in either function (e.g. dropping
    // a field name from the ["name", "declarator", "type"] probe order,
    // or a NODE_TYPE_KEYWORDS edit) is caught even though every anchor-line
    // YAML case still passes.
    const CASES: &[Case] = &[
        Case {
            ext: "py",
            source: "def foo():\n    pass\n",
            broken_tail: "def bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "js",
            source: "function foo() {\n  return 1;\n}\n",
            broken_tail: "function bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "jsx",
            source: "function Foo() {\n  return null;\n}\n",
            broken_tail: "function Bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "ts",
            source: "function foo(): number {\n  return 1;\n}\n",
            broken_tail: "function bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "tsx",
            source: "function Foo() {\n  return null;\n}\n",
            broken_tail: "function Bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "go",
            source: "package main\n\nfunc Foo() int {\n  return 1\n}\n",
            broken_tail: "func Bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "rs",
            source: "fn foo() -> i32 {\n    1\n}\n",
            broken_tail: "fn bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "java",
            source: "class Foo {\n  void bar() {}\n}\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        // C's function name lives behind child_by_field_name("declarator")
        // -> unwrap_declarator, not a direct "name" field: deleting the
        // declarator entry from the dispatch table must fail this case.
        Case {
            ext: "c",
            source: "int foo() {\n    return 0;\n}\n",
            broken_tail: "int bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "cpp",
            source: "class Foo {\npublic:\n    void bar();\n};\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "cs",
            source: "class Foo {\n    void Bar() {}\n}\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "php",
            source: "<?php\nfunction foo() {\n    return 1;\n}\n",
            broken_tail: "function bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "scala",
            source: "class Foo {\n  def bar(): Int = 1\n}\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "swift",
            source: "class Foo {\n  func bar() {}\n}\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "rb",
            source: "def foo\n  1\nend\n",
            broken_tail: "def bar\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "sh",
            source: "foo() {\n  echo hi\n}\n",
            broken_tail: "bar() {\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "css",
            source: ".foo {\n  color: red;\n}\n",
            broken_tail: ".bar {\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "hs",
            source: "foo :: Int\nfoo = 1\n",
            broken_tail: "bar ::\n",
            expected_kind: FragmentKind::Declaration,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "ex",
            source: "def foo do\n  1\nend\n",
            broken_tail: "def bar do\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "lua",
            source: "function foo()\n  return 1\nend\n",
            broken_tail: "function bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        // extract_symbol_name resolves to the literal "function" keyword
        // node here, not the assigned-to identifier `foo` -- an existing
        // R-grammar quirk (the definition_type is the anonymous
        // `function() {}` expression, not the `foo <-` assignment around
        // it). Locked in as-is; not something this task's scope fixes.
        Case {
            ext: "r",
            source: "foo <- function() {\n  1\n}\n",
            broken_tail: "bar <- function(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("function"),
        },
        Case {
            ext: "erl",
            source: "foo() -> 1.\n",
            broken_tail: "bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "jl",
            source: "function foo()\n    1\nend\n",
            broken_tail: "function bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: None,
        },
        Case {
            ext: "zig",
            source: "fn foo() void {}\n",
            broken_tail: "fn bar(\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: Some("foo"),
        },
        Case {
            ext: "clj",
            source: "(defn foo [] 1)\n",
            broken_tail: "(defn bar [\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "nix",
            source: "{ foo = 1; }\n",
            broken_tail: "bar =\n",
            expected_kind: FragmentKind::Variable,
            expected_symbol: None,
        },
        Case {
            ext: "groovy",
            source: "class Foo {\n  void bar() {}\n}\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "m",
            source: "@interface Foo\n@end\n",
            broken_tail: "@interface Bar\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: None,
        },
        Case {
            ext: "dart",
            source: "class Foo {\n  void bar() {}\n}\n",
            broken_tail: "class Bar {\n",
            expected_kind: FragmentKind::Class,
            expected_symbol: Some("Foo"),
        },
        Case {
            ext: "graphql",
            source: "type Foo {\n  bar: String\n}\n",
            broken_tail: "type Bar {\n",
            expected_kind: FragmentKind::Type,
            expected_symbol: None,
        },
        Case {
            ext: "prisma",
            source: "model Foo {\n  id Int @id\n}\n",
            broken_tail: "model Bar {\n",
            expected_kind: FragmentKind::Record,
            expected_symbol: None,
        },
        Case {
            ext: "svelte",
            source: "<script>\n  let x = 1;\n</script>\n",
            broken_tail: "<script>\nlet y =\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "tf",
            source: "resource \"foo\" \"bar\" {\n  baz = 1\n}\n",
            broken_tail: "resource \"bar\" \"baz\" {\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "cmake",
            source: "function(foo)\nendfunction()\n",
            broken_tail: "function(bar\n",
            expected_kind: FragmentKind::Function,
            expected_symbol: None,
        },
        Case {
            ext: "mk",
            source: "foo:\n\techo hi\n",
            broken_tail: "bar:\n\tunterminated \\\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "yaml",
            source: "foo: bar\n",
            broken_tail: "bar:\n  nested:\n    - unterminated [\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "json",
            source: "{\"foo\": 1}\n",
            broken_tail: "\"bar\": {\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
        Case {
            ext: "html",
            source: "<div id=\"foo\"></div>\n",
            broken_tail: "<div id=\"bar\"\n",
            expected_kind: FragmentKind::Definition,
            expected_symbol: None,
        },
    ];

    fn fragment_ext(ext: &str, source: &str) -> Vec<Fragment> {
        TreeSitterStrategy::new().fragment(Arc::from(format!("case.{ext}")), source)
    }

    fn fragment_named(name: &str, source: &str) -> Vec<Fragment> {
        TreeSitterStrategy::new().fragment(Arc::from(name), source)
    }

    #[test]
    fn representative_construct_per_grammar_reports_symbol_and_kind() {
        for case in CASES {
            let frags = fragment_ext(case.ext, case.source);
            assert!(
                !frags.is_empty(),
                "{}: expected at least one fragment",
                case.ext
            );
            let first = &frags[0];
            assert_eq!(
                first.kind, case.expected_kind,
                "{}: kind mismatch (got {:?})",
                case.ext, first.kind
            );
            assert_eq!(
                first.symbol_name.as_deref(),
                case.expected_symbol,
                "{}: symbol_name mismatch (got {:?})",
                case.ext,
                first.symbol_name
            );
        }
    }

    // Regression for parsers/mod.rs:32: a syntactically broken file must
    // still (a) never panic, (b) never report a fragment span outside the
    // file, and (c) keep the symbol_name of any definition that appears
    // before the broken region -- the failure mode under test is
    // create_code_gap_fragments silently swallowing everything into a
    // symbol_name: None blob once tree-sitter's ERROR recovery kicks in.
    #[test]
    fn broken_file_per_grammar_does_not_panic_and_keeps_valid_spans() {
        for case in CASES {
            let broken = format!("{}\n{}", case.source, case.broken_tail);
            let total_lines = broken.split('\n').count() as u32;
            let frags = fragment_ext(case.ext, &broken);

            for f in &frags {
                assert!(
                    f.id.start_line >= 1,
                    "{}: start_line must be >= 1",
                    case.ext
                );
                assert!(
                    f.id.start_line <= f.id.end_line,
                    "{}: start_line must be <= end_line",
                    case.ext
                );
                assert!(
                    f.id.end_line <= total_lines,
                    "{}: end_line {} exceeds file length {}",
                    case.ext,
                    f.id.end_line,
                    total_lines
                );
            }

            // Gap-filling Chunk fragments (create_code_gap_fragments) are
            // the ones whose non-overlap is an actual algorithmic
            // guarantee (built from a disjoint covered-line complement);
            // semantic definition fragments can legitimately nest (e.g. an
            // outer container header plus an inner definition sharing a
            // line), so only the chunk set is checked for disjointness.
            let mut chunk_spans: Vec<(u32, u32)> = frags
                .iter()
                .filter(|f| f.kind == FragmentKind::Chunk && f.symbol_name.is_none())
                .map(|f| (f.id.start_line, f.id.end_line))
                .collect();
            chunk_spans.sort_unstable();
            for w in chunk_spans.windows(2) {
                assert!(
                    w[0].1 < w[1].0,
                    "{}: gap-fill chunks overlap: {:?}",
                    case.ext,
                    w
                );
            }

            if let Some(expected_symbol) = case.expected_symbol {
                assert!(
                    frags
                        .iter()
                        .any(|f| f.symbol_name.as_deref() == Some(expected_symbol)
                            && f.kind == case.expected_kind),
                    "{}: definition before the error point lost its symbol_name/kind",
                    case.ext
                );
            }
        }
    }

    #[test]
    fn every_lang_config_extension_is_discoverable_via_extension_to_language() {
        for config in LANG_CONFIGS {
            assert!(
                crate::languages::EXTENSION_TO_LANGUAGE.contains_key(config.extension),
                "LANG_CONFIGS extension {:?} (ts_name {:?}) has no EXTENSION_TO_LANGUAGE \
                 entry, so candidate_files.rs silently excludes it from discovery even \
                 though a grammar exists for it",
                config.extension,
                config.ts_name
            );
        }
    }

    #[test]
    fn cmake_and_makefile_filenames_dispatch_to_their_compiled_grammars() {
        let cmake_frags = fragment_named("CMakeLists.txt", "function(foo)\nendfunction()\n");
        assert!(
            cmake_frags.iter().any(|f| f.kind == FragmentKind::Function),
            "CMakeLists.txt should route through the cmake grammar via find_lang_config_by_filename"
        );

        let make_frags = fragment_named("Makefile", "foo:\n\techo hi\n");
        assert!(
            !make_frags.is_empty(),
            "Makefile should route through the make grammar via find_lang_config_by_filename"
        );
    }

    #[test]
    fn parse_error_is_observable_via_has_error_probe() {
        let clean = "def foo():\n    pass\n";
        let broken = "def foo(\n    pass\n";
        assert_eq!(parse_has_error("clean.py", clean), Some(false));
        assert_eq!(parse_has_error("broken.py", broken), Some(true));
        assert_eq!(parse_has_error("no_grammar.unknownext", clean), None);
    }

    // Regression for tree_sitter_strategy.rs:1055 (PARSE_TIMEOUT / minified
    // files): a ~230KB single-line minified bundle collapses every
    // definition onto physical line 1, so line-granularity fragmentation
    // cannot separate them. The dangerous failure mode is unbounded output
    // (one fragment per repeated construct, or output size scaling with
    // the pathological repeat count) -- this pins that the algorithm's
    // existing dedup-by-(kind,end_line) keeps the fragment count bounded
    // and no single fragment grows past the source itself.
    #[test]
    fn minified_single_line_bundle_yields_bounded_fragment_count() {
        let n = 8000;
        let mut content = String::with_capacity(n * 28);
        for i in 0..n {
            content.push_str(&format!("function f{i}(){{return {i};}}"));
        }

        let t0 = Instant::now();
        let frags = fragment_named("bundle.min.js", &content);
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "minified parse must not hang the pipeline"
        );

        assert!(
            frags.len() < 100,
            "pathological single-line repetition ({n} defs) must not explode fragment count: got {}",
            frags.len()
        );
        for f in &frags {
            // +1: create_snippet appends a trailing '\n' when the source
            // line lacks one, so a whole-line fragment can be exactly one
            // byte longer than the (newline-free) source.
            assert!(
                f.content.len() <= content.len() + 1,
                "no fragment may exceed the source file size"
            );
        }
    }
}
