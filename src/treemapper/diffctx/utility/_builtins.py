# pylint: disable=duplicate-code
from __future__ import annotations

import re

from ..stopwords import _DOCS_STOPWORDS, CODE_STOPWORDS

_EXPANSION_STOPWORDS = CODE_STOPWORDS | _DOCS_STOPWORDS

_CALL_RE = re.compile(r"(\w+)\s*\(")
_TYPE_REF_RE = re.compile(r"(?::|->)\s*([A-Z]\w+)")
_GENERIC_TYPE_RE = re.compile(r"[\[<,]\s*([A-Z]\w*)")

_TF_EXTENSIONS = frozenset({".tf", ".tfvars", ".hcl"})
_CONFIG_EXTENSIONS_FOR_DIFF = frozenset({".yaml", ".yml", ".json", ".toml", ".ini"})
_TF_VAR_NEED_RE = re.compile(r"var\.(\w+)")
_TF_RES_REF_NEED_RE = re.compile(r"(?<![.\w])([a-zA-Z]\w*)\.(\w+)(?:\[\*?\w*\])?\.[\w\[\]*]+")
_TF_SKIP_REF_TYPES = frozenset({"var", "local", "data", "module", "path", "terraform", "count", "each", "self"})

_LANGUAGE_BUILTINS: frozenset[str] = frozenset(
    {
        # Python builtins
        "range",
        "enumerate",
        "zip",
        "sorted",
        "reversed",
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
        "round",
        "pow",
        "divmod",
        "repr",
        "dir",
        "vars",
        "globals",
        "locals",
        "breakpoint",
        "property",
        "classmethod",
        "staticmethod",
        "dataclass",
        "object",
        # Python exceptions
        "exception",
        "baseexception",
        "valueerror",
        "typeerror",
        "keyerror",
        "indexerror",
        "attributeerror",
        "importerror",
        "runtimeerror",
        "stopiteration",
        "generatorexit",
        "oserror",
        "ioerror",
        "filenotfounderror",
        "permissionerror",
        "notimplementederror",
        "zerodivisionerror",
        "overflowerror",
        "memoryerror",
        "recursionerror",
        "unicodeerror",
        "assertionerror",
        "lookuperror",
        "arithmeticerror",
        # JavaScript / DOM APIs
        "array.from",
        "object.keys",
        "object.values",
        "object.entries",
        "array.isarray",
        "number.isnan",
        "number.isfinite",
        "parseint",
        "parsefloat",
        "isnan",
        "isfinite",
        "settimeout",
        "setinterval",
        "clearinterval",
        "cleartimeout",
        "requestanimationframe",
        "cancelanimationframe",
        "typeof",
        "void",
        # Go builtins
        "make",
        "append",
        "panic",
        "recover",
        "cap",
        "println",
        "printf",
        "sprintf",
        "fprintf",
        "errorf",
        # Rust — only distinctive, non-generic names
        "vec",
        "arc",
        "unwrap",
        # React hooks (distinctive use* naming convention)
        "usestate",
        "useeffect",
        "usecontext",
        "usereducer",
        "usecallback",
        "usememo",
        "useref",
        "uselayouteffect",
        "useimperativehandle",
        "usedebugvalue",
        "useid",
        "usetransition",
        "usedeferredvalue",
        "createcontext",
        "forwardref",
        "createref",
        "suspense",
        "strictmode",
        "profiler",
        # React Router / Redux / React Query hooks
        "usenavigate",
        "useparams",
        "uselocation",
        "usesearchparams",
        "useloaderdata",
        "useactiondata",
        "usefetcher",
        "useoutletcontext",
        "usedispatch",
        "useselector",
        "usestore",
        "usequery",
        "usemutation",
        "usesubscription",
        # Test framework globals
        "describe",
        "beforeeach",
        "aftereach",
        "beforeall",
        "afterall",
        "assert",
    }
)

_CLOSURE_MIN_EDGE_WEIGHT = 0.5
_INVARIANT_RE = re.compile(r"\b(?:assert|require|ensure|precondition|postcondition|invariant)\s*\(\s*(\w+)", re.IGNORECASE)

_COMMENT_PREFIXES = ("#", "//", "* ", "/*", "--", '"""', "'''", "<!--")

_JS_IMPORT_RE = re.compile(r"""import\s+\{([^}]+)\}\s+from\s+['"]([^'"]+)['"]""")
_PY_IMPORT_RE = re.compile(r"""from\s+(\S+)\s+import\s+(.+)""")
_JS_LOCAL_IMPORT_RE = re.compile(r"""import\s+(?:\{([^}]+)\}|([A-Z]\w+))\s+from\s+['"]([^'"]+)['"]""")

_ONE_CLASS_PER_FILE_SUFFIXES = frozenset({".swift", ".java", ".kt"})
_CLOSURE_EDGE_CATEGORIES = frozenset({"structural", "semantic"})
