from __future__ import annotations

import re

IMPORT_PATTERNS = {
    "python": re.compile(r"(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))"),
    "go_single": re.compile(r'^\s*import\s+"([^"]+)"', re.MULTILINE),
    "go_block": re.compile(r"import\s*\((.*?)\)", re.DOTALL),
    "go_line": re.compile(r'^\s*(?:\w+\s+)?"([^"]+)"', re.MULTILINE),
    "rust_use": re.compile(r"^\s*use\s+(?:crate::)?([a-z_][a-z0-9_]*(?:::[a-z_][a-z0-9_]*)*)", re.MULTILINE),
    "rust_mod": re.compile(r"^\s*(?:pub\s+)?mod\s+([a-z_][a-z0-9_]*)\s*[;{]", re.MULTILINE),
    "rust_extern_crate": re.compile(r"^\s*extern\s+crate\s+([a-z_][a-z0-9_]*)", re.MULTILINE),
    "javascript_static": re.compile(r"""import\s{1,10}[^'"]{0,500}['"]([^'"]{1,500})['"]"""),
    "javascript_require": re.compile(r"""require\s{0,10}\(\s{0,10}['"]([^'"]{1,500})['"]\s{0,10}\)"""),
    "javascript_export_from": re.compile(r"""export\s{1,10}[^'"]{0,500}\s{1,10}from\s{1,10}['"]([^'"]{1,500})['"]"""),
    "java_import": re.compile(r"^\s*import\s+(?:static\s+)?([a-zA-Z_][\w.]*)\s*;", re.MULTILINE),
    "java_package": re.compile(r"^\s*package\s+([a-zA-Z_][\w.]*)\s*;", re.MULTILINE),
    "csharp_using": re.compile(r"^\s*using\s+(?:static\s+)?([A-Z][\w.]*)\s*;", re.MULTILINE),
    "c_include": re.compile(r'^\s*#include\s*[<"]([^>"]+)[>"]', re.MULTILINE),
    "ruby_require": re.compile(r"""^\s*require(?:_relative)?\s+['"]([^'"]+)['"]""", re.MULTILINE),
    "php_use": re.compile(r"^\s*use\s+([A-Z][\w\\]*)", re.MULTILINE),
    "shell_source": re.compile(r"""^\s*(?:source|\\.)\s+['"]?([^'"\s]+)['"]?""", re.MULTILINE),
    "swift_import": re.compile(r"^\s*import\s+([A-Za-z_]\w*)", re.MULTILINE),
}

DEFINITION_PATTERNS = {
    "go_func": re.compile(r"^func\s+(?:\([^)]+\)\s+)?(\w+)\s*\(", re.MULTILINE),
    "go_type": re.compile(r"^type\s+(\w+)\s+(?:struct|interface|func)", re.MULTILINE),
    "go_const_var": re.compile(r"^(?:const|var)\s+(\w+)\s+", re.MULTILINE),
    "rust_fn": re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([a-z_][a-z0-9_]*)", re.MULTILINE),
    "rust_struct": re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Z]\w*)", re.MULTILINE),
    "rust_enum": re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Z]\w*)", re.MULTILINE),
    "rust_trait": re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?trait\s+([A-Z]\w*)", re.MULTILINE),
    "rust_impl": re.compile(r"^\s*impl(?:<[^>]+>)?\s+(?:\w+\s+for\s+)?([A-Z]\w*)", re.MULTILINE),
    "rust_type_alias": re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Z]\w*)", re.MULTILINE),
    "java_class": re.compile(r"^\s*(?:public\s+)?(?:abstract\s+)?(?:final\s+)?class\s+([A-Z]\w*)", re.MULTILINE),
    "java_interface": re.compile(r"^\s*(?:public\s+)?interface\s+([A-Z]\w*)", re.MULTILINE),
    "java_enum": re.compile(r"^\s*(?:public\s+)?enum\s+([A-Z]\w*)", re.MULTILINE),
    "java_method": re.compile(
        r"^\s{0,20}(?:(?:public|private|protected)\s+)?(?:static\s+)?[\w<>\[\]]{1,100}\s+([a-z]\w*)\s*\(", re.MULTILINE
    ),
    "csharp_class": re.compile(r"^\s*(?:public\s+)?(?:partial\s+)?(?:abstract\s+)?class\s+([A-Z]\w*)", re.MULTILINE),
    "csharp_interface": re.compile(r"^\s*(?:public\s+)?interface\s+(I[A-Z]\w*)", re.MULTILINE),
    "c_function": re.compile(r"^(?:static\s+)?(?:\w+\s+)+(\w+)\s*\([^)]*\)\s*\{", re.MULTILINE),
}

REFERENCE_PATTERNS = {
    "go_func_call": re.compile(r"\b([A-Z]\w+)\s*\("),
    "go_type_ref": re.compile(r"\*?([A-Z]\w*)\b"),
    "go_pkg_call": re.compile(r"\b(\w+)\.([A-Z]\w*)"),
    "rust_type_ref": re.compile(r"(?<![a-z_])([A-Z]\w*)\b"),
    "rust_fn_call": re.compile(r"(?<!\w)([a-z_][a-z0-9_]*)\s?!?\s?\("),
    "rust_path_call": re.compile(r"([a-z_][a-z0-9_]*)::([a-z_][a-z0-9_]*|[A-Z]\w*)"),
    "java_type_ref": re.compile(r"\b([A-Z]\w*)\b"),
    "java_method_call": re.compile(r"\.([a-z]\w*)\s*\("),
}

CONFIG_KEY_PATTERNS: dict[str, list[re.Pattern[str]]] = {
    ".yaml": [re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*:", re.MULTILINE)],
    ".yml": [re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*:", re.MULTILINE)],
    ".json": [re.compile(r'"([a-zA-Z_][a-zA-Z0-9_-]*)"\s*:')],
    ".toml": [
        re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*=", re.MULTILINE),
        re.compile(r"^\[([a-zA-Z_][a-zA-Z0-9_.-]*)\]", re.MULTILINE),
    ],
    ".ini": [re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*=", re.MULTILINE)],
    ".env": [re.compile(r"^([A-Za-z_]\w*)\s*=", re.MULTILINE)],
}

DOC_PATTERNS = {
    "citation": re.compile(r"\[@([a-zA-Z0-9_:-]+)\]"),
    "md_internal_link": re.compile(r"\[([^\]]+)\]\(#([^)]+)\)"),
    "md_heading": re.compile(r"^#{1,6}\s+([^\n]{1,1000})$", re.MULTILINE),
}
