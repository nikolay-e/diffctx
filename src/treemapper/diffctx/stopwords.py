from __future__ import annotations

import keyword
from collections.abc import Iterable
from pathlib import Path
from typing import ClassVar

from .constants import CODE_EXTENSIONS, DOC_EXTENSIONS

PY_KEYWORDS: frozenset[str] = frozenset(k.lower() for k in keyword.kwlist)

_CODE_STOPWORDS: frozenset[str] = frozenset(
    {
        # Common placeholders and comments
        "todo",
        "fixme",
        "note",
        "hack",
        "xxx",
        "foo",
        "bar",
        "baz",
        "qux",
        "tmp",
        "temp",
        # Python-specific
        "self",
        "cls",
        # Common method/function names (too generic for identifier matching)
        "get",
        "set",
        "add",
        "put",
        "delete",
        "remove",
        "update",
        "create",
        "read",
        "write",
        "open",
        "close",
        "start",
        "stop",
        "run",
        "execute",
        "call",
        "invoke",
        "handle",
        "process",
        "validate",
        "check",
        "test",
        "init",
        "setup",
        "teardown",
        "load",
        "save",
        "parse",
        "format",
        "convert",
        "transform",
        "render",
        "build",
        "make",
        "copy",
        "clone",
        "reset",
        "clear",
        "flush",
        "send",
        "receive",
        "emit",
        "dispatch",
        "notify",
        "subscribe",
        "publish",
        "connect",
        "disconnect",
        "bind",
        "listen",
        "accept",
        # Common variable/parameter names
        "data",
        "value",
        "values",
        "result",
        "results",
        "item",
        "items",
        "element",
        "elements",
        "key",
        "keys",
        "name",
        "names",
        "text",
        "string",
        "str",
        "num",
        "number",
        "count",
        "index",
        "idx",
        "size",
        "length",
        "len",
        "list",
        "array",
        "map",
        "dict",
        "obj",
        "object",
        "args",
        "kwargs",
        "params",
        "options",
        "config",
        "settings",
        "context",
        "ctx",
        "state",
        "status",
        "type",
        "kind",
        "mode",
        "flag",
        "flags",
        "path",
        "file",
        "dir",
        "url",
        "uri",
        "host",
        "port",
        "input",
        "output",
        "source",
        "target",
        "dest",
        "src",
        "dst",
        "err",
        "error",
        "errors",
        "msg",
        "message",
        "messages",
        "log",
        "logger",
        "debug",
        "info",
        "warn",
        "warning",
        "func",
        "function",
        "method",
        "callback",
        "handler",
        "listener",
        "event",
        "events",
        "request",
        "response",
        "req",
        "res",
        "body",
        "header",
        "headers",
        "query",
        "param",
        "id",
        "uid",
        "uuid",
        "user",
        "users",
        "model",
        "models",
        "view",
        "views",
        "service",
        "services",
        "client",
        "server",
        "api",
        "app",
        "main",
        "util",
        "utils",
        "helper",
        "helpers",
        "common",
        "base",
        "core",
        "default",
        "defaults",
    }
)

CODE_STOPWORDS: frozenset[str] = _CODE_STOPWORDS | PY_KEYWORDS


class TokenProfile:
    CODE = "code"
    DOCS = "docs"
    LEGAL = "legal"
    DATA = "data"
    GENERIC = "generic"

    _PROFILES: ClassVar[dict[str, tuple[frozenset[str], int]]] = {
        CODE: (CODE_STOPWORDS, 3),
        DOCS: (frozenset(), 3),
        LEGAL: (frozenset(), 4),
        DATA: (frozenset(), 2),
        GENERIC: (CODE_STOPWORDS, 3),
    }

    @classmethod
    def get_stopwords(cls, profile: str) -> frozenset[str]:
        return cls._PROFILES.get(profile, cls._PROFILES[cls.GENERIC])[0]

    @classmethod
    def get_min_len(cls, profile: str) -> int:
        return cls._PROFILES.get(profile, cls._PROFILES[cls.GENERIC])[1]

    @classmethod
    def from_path(cls, path_str: str) -> str:
        p = Path(path_str)
        suffix = p.suffix.lower()
        name_lower = p.name.lower()

        if suffix in CODE_EXTENSIONS:
            return cls.CODE

        if suffix in DOC_EXTENSIONS or suffix in {".markdown", ".tex"}:
            return cls.DOCS

        data_exts = {".csv", ".json", ".jsonl", ".yaml", ".yml", ".toml", ".xml", ".ini", ".env"}
        if suffix in data_exts:
            return cls.DATA

        legal_names = {"license", "licence", "legal", "terms", "agreement", "contract", "policy", "privacy", "tos", "eula"}
        stem = p.stem.lower()
        if stem in legal_names or any(lw in name_lower for lw in ("license", "legal", "terms")):
            return cls.LEGAL

        return cls.GENERIC


def is_reasonable_ident(ident: str, *, min_len: int = 3, profile: str = "code") -> bool:
    if not ident or len(ident) < min_len:
        return False
    low = ident.lower()
    stopwords = TokenProfile.get_stopwords(profile)
    if low in stopwords:
        return False
    if low.isdigit():
        return False
    return True


def filter_idents(idents: Iterable[str], *, min_len: int = 3, profile: str = "code") -> list[str]:
    return [s for s in idents if is_reasonable_ident(s, min_len=min_len, profile=profile)]
