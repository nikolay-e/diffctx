use std::cell::RefCell;
use std::sync::Arc;

use once_cell::sync::Lazy;
use rustc_hash::{FxHashMap, FxHashSet};
use tree_sitter::{Language, Node, Parser, Tree};

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
            "type_alias",
            "newtype",
            "adt",
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
            "type_alias",
            "newtype",
            "adt",
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
        definition_types: &[
            "function_declaration",
            "local_function",
            "function_definition_statement",
        ],
    },
    // --- R ---
    LangConfig {
        extension: ".r",
        ts_name: "r",
        definition_types: &["function_definition", "left_assignment"],
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
        definition_types: &["FnDecl", "ContainerDecl", "TestDecl", "VarDecl"],
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
            "class_definition",
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
    // --- Kotlin (parsed as Java - close enough for fragmentation) ---
    LangConfig {
        extension: ".kt",
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
        extension: ".kts",
        ts_name: "java",
        definition_types: &[
            "method_declaration",
            "class_declaration",
            "interface_declaration",
            "enum_declaration",
            "constructor_declaration",
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
    // --- F# (parsed as C# - closest available grammar) ---
    LangConfig {
        extension: ".fs",
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
        extension: ".fsi",
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
        extension: ".fsx",
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
    LANG_CONFIGS.iter().find(|c| c.extension == ext)
}

static LANGUAGE_CACHE: Lazy<FxHashMap<&'static str, Language>> = Lazy::new(|| {
    let mut m: FxHashMap<&'static str, Language> = FxHashMap::default();
    m.insert("python", Language::new(tree_sitter_python::LANGUAGE));
    let js = Language::new(tree_sitter_javascript::LANGUAGE);
    m.insert("javascript", js.clone());
    m.insert("jsx", js);
    m.insert(
        "typescript",
        Language::new(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
    );
    m.insert("tsx", Language::new(tree_sitter_typescript::LANGUAGE_TSX));
    m.insert("go", Language::new(tree_sitter_go::LANGUAGE));
    m.insert("rust", Language::new(tree_sitter_rust::LANGUAGE));
    m.insert("java", Language::new(tree_sitter_java::LANGUAGE));
    m.insert("c", Language::new(tree_sitter_c::LANGUAGE));
    m.insert("cpp", Language::new(tree_sitter_cpp::LANGUAGE));
    m.insert("ruby", Language::new(tree_sitter_ruby::LANGUAGE));
    m.insert("c_sharp", Language::new(tree_sitter_c_sharp::LANGUAGE));
    m.insert("php", Language::new(tree_sitter_php::LANGUAGE_PHP));
    m.insert("scala", Language::new(tree_sitter_scala::LANGUAGE));
    m.insert("swift", Language::new(tree_sitter_swift::LANGUAGE));
    m.insert("html", Language::new(tree_sitter_html::LANGUAGE));
    m.insert("bash", Language::new(tree_sitter_bash::LANGUAGE));
    m.insert("css", Language::new(tree_sitter_css::LANGUAGE));
    m.insert("haskell", Language::new(tree_sitter_haskell::LANGUAGE));
    m.insert("elixir", Language::new(tree_sitter_elixir::LANGUAGE));
    m.insert("lua", Language::new(tree_sitter_lua::LANGUAGE));
    m.insert("r", Language::new(tree_sitter_r::LANGUAGE));
    m.insert("ocaml", Language::new(tree_sitter_ocaml::LANGUAGE_OCAML));
    m.insert("erlang", Language::new(tree_sitter_erlang::LANGUAGE));
    m.insert("julia", Language::new(tree_sitter_julia::LANGUAGE));
    m.insert("zig", Language::new(tree_sitter_zig::LANGUAGE));
    m.insert("clojure", Language::new(tree_sitter_clojure::LANGUAGE));
    m.insert("nix", Language::new(tree_sitter_nix::LANGUAGE));
    m.insert("groovy", Language::new(tree_sitter_groovy::LANGUAGE));
    m.insert("objc", Language::new(tree_sitter_objc::LANGUAGE));
    m.insert("cmake", Language::new(tree_sitter_cmake::LANGUAGE));
    m.insert("make", Language::new(tree_sitter_make::LANGUAGE));
    m.insert("hcl", Language::new(tree_sitter_hcl::LANGUAGE));
    m.insert("graphql", Language::new(tree_sitter_graphql::LANGUAGE));
    m.insert("dart", Language::new(tree_sitter_dart::LANGUAGE));
    m.insert("prisma", Language::new(tree_sitter_prisma_io::LANGUAGE));
    m.insert("svelte", Language::new(tree_sitter_svelte_ng::LANGUAGE));
    m.insert("json", Language::new(tree_sitter_json::LANGUAGE));
    m.insert("yaml", Language::new(tree_sitter_yaml::LANGUAGE));
    m
});

fn get_tree_sitter_language(ts_name: &str) -> Option<Language> {
    LANGUAGE_CACHE.get(ts_name).cloned()
}

thread_local! {
    static PARSER_CACHE: RefCell<FxHashMap<&'static str, Parser>> = RefCell::new(FxHashMap::default());
}

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
        parser.parse(content, None)
    })
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
            "pointer_declarator" | "function_declarator" => {
                if let Some(inner) = name_node.child_by_field_name("declarator") {
                    name_node = inner;
                } else {
                    break;
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
    let first_child_start = match first_child_def_line(node, definition_types, 0) {
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
