from __future__ import annotations

import re

_NAMED_IMPORT_RE = re.compile(r"import\s*\{([^}]+)\}\s*from\s*['\"]([^'\"]+)['\"]", re.MULTILINE)
_DEFAULT_IMPORT_RE = re.compile(r"import\s+(\w+)\s+from\s*['\"]([^'\"]+)['\"]", re.MULTILINE)
_NAMESPACE_IMPORT_RE = re.compile(r"import\s*\*\s*as\s+(\w+)\s+from\s*['\"]([^'\"]+)['\"]", re.MULTILINE)
_SIDE_EFFECT_IMPORT_RE = re.compile(r"import\s*['\"]([^'\"]+)['\"]", re.MULTILINE)
_DYNAMIC_IMPORT_RE = re.compile(r"import\s*\(\s*['\"]([^'\"]+)['\"]\s*\)", re.MULTILINE)
_TYPE_IMPORT_RE = re.compile(r"import\s+type\s+\{([^}]+)\}\s*from\s*['\"]([^'\"]+)['\"]", re.MULTILINE)
_TYPE_DEFAULT_IMPORT_RE = re.compile(r"^import\s+type\s+(\w+)\s+from\s+['\"]([^'\"]+)['\"]", re.MULTILINE)
_REQUIRE_RE = re.compile(
    r"(?:const|let|var)\s+(?:\{([^}]+)\}|(\w+))\s*=\s*require\s*\(\s*['\"]([^'\"]+)['\"]\s*\)",
    re.MULTILINE,
)

_NAMED_EXPORT_RE = re.compile(r"export\s+(?:const|let|var|function\*?|class|async\s+function)\s+(\w+)", re.MULTILINE)
_DEFAULT_EXPORT_RE = re.compile(r"export\s+default\s+(?:(?:class|function\*?|async\s+function)\s+)?(\w+)?", re.MULTILINE)
_EXPORT_LIST_RE = re.compile(r"export\s*\{([^}]+)\}", re.MULTILINE)
_REEXPORT_RE = re.compile(r"export\s*\{([^}]*)\}\s*from\s*['\"]([^'\"]+)['\"]", re.MULTILINE)
_EXPORT_ALL_RE = re.compile(r"export\s*\*\s*from\s*['\"]([^'\"]+)['\"]", re.MULTILINE)

_FUNCTION_DECL_RE = re.compile(r"function\s+(\w+)\s*\(", re.MULTILINE)
_ASYNC_FUNCTION_RE = re.compile(r"async\s+function\s+(\w+)\s*\(", re.MULTILINE)
_ARROW_FUNCTION_RE = re.compile(r"(?:const|let|var)\s+(\w+)\s*=\s*(?:async\s+)?\([^)]*\)\s*=>", re.MULTILINE)
_CLASS_RE = re.compile(
    r"class\s+(\w+)(?:\s+extends\s+([\w.]+))?(?:\s+implements\s+([A-Z][\w,\s]{0,200}))?\s*\{",
    re.MULTILINE,
)

_CALL_EXPR_RE = re.compile(r"\b(\w+)\s*\(", re.MULTILINE)
_MEMBER_CALL_RE = re.compile(r"\.(\w+)\s*\(", re.MULTILINE)
_NEW_EXPR_RE = re.compile(r"new\s+(\w+)\s*\(", re.MULTILINE)
_OPTIONAL_CHAIN_RE = re.compile(r"\?\.\s*(\w+)", re.MULTILINE)

_TYPE_ANNOTATION_RE = re.compile(r":\s*([A-Z]\w+)(?:<[^>]+>)?", re.MULTILINE)
_GENERIC_TYPE_RE = re.compile(r"<\s*([A-Z]\w+)(?:\s*,\s*[A-Z]\w+)*\s*>", re.MULTILINE)
_GENERIC_TYPE_INNER_RE = re.compile(r"[A-Z]\w+")
_EXTENDS_TYPE_RE = re.compile(r"extends\s+([A-Z]\w+)", re.MULTILINE)
_IMPLEMENTS_TYPE_RE = re.compile(r"implements\s+([\w,\s]+)", re.MULTILINE)
_TYPE_ALIAS_RE = re.compile(r"type\s+(\w+)(?:<[^>]+>)?\s*=", re.MULTILINE)
_INTERFACE_RE = re.compile(r"interface\s+(\w+)(?:<[^>]+>)?(?:\s+extends\s+([A-Z][\w,\s]{0,200}))?\s*\{", re.MULTILINE)
_RETURN_TYPE_RE = re.compile(r"\)\s*:\s*([A-Z]\w+)(?:<[^>]+>)?", re.MULTILINE)

_UTILITY_TYPES = frozenset(
    [
        "Partial",
        "Required",
        "Readonly",
        "Record",
        "Pick",
        "Omit",
        "Exclude",
        "Extract",
        "NonNullable",
        "Parameters",
        "ConstructorParameters",
        "ReturnType",
        "InstanceType",
        "ThisParameterType",
        "OmitThisParameter",
        "ThisType",
        "Uppercase",
        "Lowercase",
        "Capitalize",
        "Uncapitalize",
        "Awaited",
        "Promise",
        "Array",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
    ]
)

_IDENT_RE = re.compile(r"\b([A-Za-z_$][A-Za-z0-9_$]*)\b")

_JS_KEYWORDS = frozenset(
    [
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "let",
        "new",
        "null",
        "of",
        "package",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "with",
        "yield",
        "as",
        "is",
        "keyof",
        "readonly",
        "infer",
        "never",
        "unknown",
        "any",
        "string",
        "number",
        "boolean",
        "object",
        "symbol",
        "bigint",
        "module",
        "namespace",
        "declare",
        "abstract",
        "satisfies",
    ]
)

_BUILTIN_GLOBALS = frozenset(
    [
        "console",
        "window",
        "document",
        "global",
        "process",
        "require",
        "module",
        "exports",
        "Buffer",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
        "setImmediate",
        "clearImmediate",
        "JSON",
        "Math",
        "Date",
        "RegExp",
        "Error",
        "TypeError",
        "RangeError",
        "SyntaxError",
        "Object",
        "Array",
        "String",
        "Number",
        "Boolean",
        "Symbol",
        "BigInt",
        "Function",
        "Promise",
        "Proxy",
        "Reflect",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "ArrayBuffer",
        "DataView",
        "Int8Array",
        "Uint8Array",
        "Float32Array",
        "Float64Array",
        "Intl",
        "fetch",
        "URL",
        "URLSearchParams",
        "Headers",
        "Request",
        "Response",
        "FormData",
        "Blob",
        "File",
        "FileReader",
        "WebSocket",
        "Worker",
        "SharedWorker",
        "MessageChannel",
        "MessagePort",
        "performance",
        "navigator",
        "location",
        "history",
        "localStorage",
        "sessionStorage",
        "indexedDB",
        "crypto",
        "TextEncoder",
        "TextDecoder",
        "atob",
        "btoa",
        "queueMicrotask",
        "structuredClone",
        "AbortController",
        "AbortSignal",
        "Event",
        "EventTarget",
        "CustomEvent",
        "Node",
        "Element",
        "HTMLElement",
        "DocumentFragment",
        "NodeList",
        "HTMLCollection",
        "MutationObserver",
        "IntersectionObserver",
        "ResizeObserver",
        "requestAnimationFrame",
        "cancelAnimationFrame",
        "getComputedStyle",
        "matchMedia",
        "addEventListener",
        "removeEventListener",
        "dispatchEvent",
        "alert",
        "confirm",
        "prompt",
        "open",
        "close",
        "print",
        "scroll",
        "scrollTo",
        "scrollBy",
        "focus",
        "blur",
        "getSelection",
    ]
)
