from __future__ import annotations

import re

_LOCAL_DEF_RE = re.compile(
    r"^\s*(?:def|class)\s+(\w+)|^\s*(\w+)\s*[:=]|^\s*for\s+(\w+)\s+in|^\s*with\s+.*\s+as\s+(\w+)",
    re.MULTILINE,
)
_IDENT_RE = re.compile(r"[A-Za-z_]\w{2,}")

_BUILTINS = frozenset(
    {
        "print",
        "len",
        "range",
        "int",
        "str",
        "list",
        "dict",
        "set",
        "tuple",
        "bool",
        "float",
        "type",
        "super",
        "self",
        "cls",
        "None",
        "True",
        "False",
        "isinstance",
        "issubclass",
        "hasattr",
        "getattr",
        "setattr",
        "delattr",
        "callable",
        "iter",
        "next",
        "any",
        "all",
        "abs",
        "min",
        "max",
        "sum",
        "sorted",
        "reversed",
        "enumerate",
        "zip",
        "map",
        "filter",
        "open",
        "input",
        "repr",
        "hash",
        "id",
        "dir",
        "vars",
        "globals",
        "locals",
        "property",
        "staticmethod",
        "classmethod",
        "return",
        "import",
        "from",
        "class",
        "def",
        "for",
        "while",
        "with",
        "try",
        "except",
        "finally",
        "raise",
        "assert",
        "yield",
        "pass",
        "break",
        "continue",
        "lambda",
        "not",
        "and",
        "elif",
        "else",
        "del",
        "global",
        "nonlocal",
        "async",
        "await",
        "const",
        "let",
        "var",
        "function",
        "new",
        "this",
        "null",
        "undefined",
        "typeof",
        "void",
        "delete",
        "throw",
        "catch",
        "switch",
        "case",
        "default",
        "export",
        "require",
        "module",
        "console",
        "Array",
        "Object",
        "String",
        "Number",
        "Boolean",
        "Promise",
        "Error",
        "Math",
        "JSON",
        "Date",
        "Map",
        "Set",
        "RegExp",
        "Symbol",
        "Proxy",
        "Reflect",
        "WeakMap",
        "WeakSet",
        "parseInt",
        "parseFloat",
        "isNaN",
        "isFinite",
        "setTimeout",
        "setInterval",
        "clearTimeout",
        "clearInterval",
        "describe",
        "test",
        "expect",
        "beforeEach",
        "afterEach",
        "jest",
        "pytest",
        "mock",
    }
)


def extract_external_symbols(diff_text: str) -> set[str]:
    added_lines = [line[1:] for line in diff_text.splitlines() if line.startswith("+") and not line.startswith("+++")]
    all_idents: set[str] = set()
    locally_defined: set[str] = set()
    for line in added_lines:
        all_idents.update(m.group() for m in _IDENT_RE.finditer(line))
        for m in _LOCAL_DEF_RE.finditer(line):
            name = next((g for g in m.groups() if g), None)
            if name:
                locally_defined.add(name)
    return {s for s in all_idents if s not in locally_defined and s.lower() not in _BUILTINS and len(s) > 2}


def extract_definitions_in_context(fragments: list[dict]) -> set[str]:
    defined: set[str] = set()
    for frag in fragments:
        symbol = frag.get("symbol")
        if symbol and frag.get("kind") in ("function", "class", "method", "function_signature", "class_signature"):
            defined.add(symbol)
    return defined


def def_coverage(diff_text: str, selected_fragments: list[dict]) -> float:
    external = extract_external_symbols(diff_text)
    if not external:
        return 1.0
    defined = extract_definitions_in_context(selected_fragments)
    covered = len(external & defined)
    return covered / len(external)
