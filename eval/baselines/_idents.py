from __future__ import annotations

import re

_TOKEN_RE = re.compile(r"[A-Za-z_$][\w$]*")
_CAMEL_SPLIT_RE = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])")
_DIFF_META_PREFIXES = ("diff ", "index ", "--- ", "+++ ", "@@", "\\ No newline")
_KEYWORDS = frozenset(
    {
        # cross-language keywords commonly polluting BM25 queries
        "if",
        "else",
        "elif",
        "for",
        "while",
        "do",
        "return",
        "break",
        "continue",
        "pass",
        "def",
        "function",
        "func",
        "fn",
        "lambda",
        "class",
        "struct",
        "enum",
        "interface",
        "trait",
        "impl",
        "import",
        "from",
        "package",
        "use",
        "using",
        "include",
        "require",
        "module",
        "public",
        "private",
        "protected",
        "internal",
        "static",
        "const",
        "let",
        "var",
        "final",
        "auto",
        "true",
        "false",
        "null",
        "none",
        "nil",
        "void",
        "new",
        "this",
        "self",
        "super",
        "int",
        "str",
        "bool",
        "char",
        "byte",
        "long",
        "short",
        "float",
        "double",
        "try",
        "catch",
        "except",
        "finally",
        "throw",
        "throws",
        "raise",
        "in",
        "is",
        "as",
        "with",
        "yield",
        "async",
        "await",
    }
)


def _emit_query_idents(tok: str, out: set[str]) -> None:
    if len(tok) < 3 or tok.isdigit() or tok.lower() in _KEYWORDS:
        return
    out.add(tok.lower())
    for part in _CAMEL_SPLIT_RE.split(tok):
        if len(part) >= 3 and part.lower() not in _KEYWORDS:
            out.add(part.lower())
    for part in tok.split("_"):
        if len(part) >= 3 and part.lower() not in _KEYWORDS:
            out.add(part.lower())


def extract_idents_from_patch(patch: str) -> set[str]:
    """Extract retrieval-query identifiers from a unified diff.

    Matches Aider's `\\W+` split spirit but tightens it for BM25:
    - skips diff metadata lines (paths, hunk headers)
    - keeps both `+` and `-` line bodies (renamed-away symbols still locate context)
    - drops single-language keywords (BM25 IDF helps but query-length bloat hurts)
    - emits both raw tokens AND camelCase / snake_case sub-tokens
      (`getUserById` → `getUserById`, `get`, `User`, `Id` ...).
    """
    idents: set[str] = set()
    for raw in patch.splitlines():
        if any(raw.startswith(p) for p in _DIFF_META_PREFIXES):
            continue
        body = raw[1:] if raw[:1] in ("+", "-", " ") else raw
        for tok in _TOKEN_RE.findall(body):
            _emit_query_idents(tok, idents)
    return idents


def code_tokenize(text: str) -> list[str]:
    """Code-aware tokenizer for BM25 corpus / queries.

    Same camel/snake decomposition as `extract_idents_from_patch` but
    optimized for bulk file content (no diff stripping, no keyword filter
    — IDF handles them in long documents).
    """
    out: list[str] = []
    for tok in _TOKEN_RE.findall(text):
        if len(tok) < 2 or tok.isdigit():
            continue
        out.append(tok.lower())
        for part in _CAMEL_SPLIT_RE.split(tok):
            if len(part) >= 2:
                out.append(part.lower())
        for part in tok.split("_"):
            if len(part) >= 2:
                out.append(part.lower())
    return out


_BINARY_EXTS = frozenset(
    {
        ".png",
        ".jpg",
        ".jpeg",
        ".gif",
        ".webp",
        ".bmp",
        ".ico",
        ".tif",
        ".tiff",
        ".svg",
        ".pdf",
        ".zip",
        ".tar",
        ".gz",
        ".tgz",
        ".bz2",
        ".xz",
        ".7z",
        ".rar",
        ".mp3",
        ".mp4",
        ".wav",
        ".ogg",
        ".webm",
        ".mov",
        ".avi",
        ".woff",
        ".woff2",
        ".ttf",
        ".otf",
        ".eot",
        ".so",
        ".dylib",
        ".dll",
        ".exe",
        ".o",
        ".a",
        ".lib",
        ".pyc",
        ".pyo",
        ".class",
        ".jar",
        ".war",
        ".bin",
        ".dat",
        ".keystore",
        ".p12",
        ".pem",
        ".key",
    }
)


def is_skippable_path(rel_path: str, full_path) -> bool:
    """Filter rules for repo-walking corpus builders (BM25 / Aider)."""
    suffix = full_path.suffix.lower() if hasattr(full_path, "suffix") else ""
    if suffix in _BINARY_EXTS:
        return True
    parts = full_path.parts if hasattr(full_path, "parts") else rel_path.split("/")
    skip_dirs = {".git", "node_modules", "vendor", "target", "build", "dist", ".venv", "venv", "__pycache__"}
    if any(p in skip_dirs for p in parts):
        return True
    try:
        if full_path.is_file() and full_path.stat().st_size > 200_000:
            return True
    except OSError:
        return True
    return False
