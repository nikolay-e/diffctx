from __future__ import annotations

import logging
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING

from tree_sitter import Parser, Query, QueryCursor

from ...languages import EXTENSION_TO_LANGUAGE
from ...parsers.tree_sitter import _LANG_MODULES, _import_lang_module, get_ts_language
from ..base import EdgeBuilder, EdgeDict, add_ref_edges

if TYPE_CHECKING:
    from ...types import Fragment, FragmentId

logger = logging.getLogger(__name__)

_LANG_ALIASES = {
    "jsx": "javascript",
    "tsx": "typescript",
}

_LANGS_WITH_DEDICATED_BUILDER = frozenset(
    {
        "python",
        "javascript",
        "jsx",
        "typescript",
        "tsx",
        "go",
        "rust",
        "java",
        "kotlin",
        "scala",
        "c",
        "cpp",
        "csharp",
        "ruby",
        "php",
        "swift",
        "lua",
        "perl",
        "elixir",
        "erlang",
        "haskell",
        "clojure",
        "dart",
        "ocaml",
        "r",
        "nim",
        "zig",
        "bash",
    }
)


@dataclass
class _TagsInfo:
    definitions: set[str]
    calls: set[str]
    type_refs: set[str]
    references: set[str]


def _load_tags_scm(lang: str) -> str | None:
    mod = _import_lang_module(lang)
    if mod is None:
        return None
    mod_file = getattr(mod, "__file__", None)
    if not mod_file:
        return None
    tags_path = Path(mod_file).parent / "queries" / "tags.scm"
    if not tags_path.is_file():
        return None
    return tags_path.read_text(encoding="utf-8")


class _TagsQueryCache:
    def __init__(self) -> None:
        self._parsers: dict[str, Parser] = {}
        self._queries: dict[str, Query] = {}
        self._unavailable: set[str] = set()

    def get(self, lang: str) -> tuple[Parser, Query] | None:
        if lang in self._unavailable:
            return None
        if lang in self._parsers:
            return self._parsers[lang], self._queries[lang]

        ts_lang = get_ts_language(lang)
        if ts_lang is None:
            self._unavailable.add(lang)
            return None
        scm = _load_tags_scm(lang)
        if scm is None:
            self._unavailable.add(lang)
            return None
        try:
            query = Query(ts_lang, scm)
        except Exception:
            logger.debug("tags.scm query compile failed for %s", lang, exc_info=True)
            self._unavailable.add(lang)
            return None

        parser = Parser()
        parser.language = ts_lang
        self._parsers[lang] = parser
        self._queries[lang] = query
        return parser, query


_cache = _TagsQueryCache()


def _extract_tags(content: str, lang: str) -> _TagsInfo | None:
    pair = _cache.get(lang)
    if pair is None:
        return None
    parser, query = pair

    try:
        tree = parser.parse(content.encode("utf-8", errors="replace"))
    except Exception:
        return None

    definitions: set[str] = set()
    calls: set[str] = set()
    type_refs: set[str] = set()
    references: set[str] = set()

    cursor = QueryCursor(query)
    for _pattern_idx, captures_dict in cursor.matches(tree.root_node):
        name_nodes = captures_dict.get("name", [])
        if not name_nodes:
            continue
        name_text = name_nodes[0].text
        if name_text is None:
            continue
        name = name_text.decode("utf-8", errors="replace")
        if len(name) < 2:
            continue

        capture_keys = set(captures_dict.keys()) - {"name", "doc"}
        for key in capture_keys:
            if key.startswith("definition."):
                definitions.add(name)
            elif key == "reference.call":
                calls.add(name)
            elif key in ("reference.type", "reference.class", "reference.implementation"):
                type_refs.add(name)
            elif key.startswith("reference."):
                references.add(name)

    return _TagsInfo(
        definitions=definitions,
        calls=calls - definitions,
        type_refs=type_refs - definitions,
        references=references - definitions,
    )


def _lang_for_path(path: Path) -> str | None:
    lang = EXTENSION_TO_LANGUAGE.get(path.suffix.lower())
    if lang is None:
        return None
    return _LANG_ALIASES.get(lang, lang)


class TagsEdgeBuilder(EdgeBuilder):
    weight = 0.40
    reverse_weight_factor = 0.4
    category = "tags"

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        frags_by_lang: dict[str, list[Fragment]] = defaultdict(list)
        for f in fragments:
            lang = _lang_for_path(f.path)
            if lang and lang in _LANG_MODULES and lang not in _LANGS_WITH_DEDICATED_BUILDER:
                frags_by_lang[lang].append(f)

        if not frags_by_lang:
            return {}

        edges: EdgeDict = {}

        for lang, lang_frags in frags_by_lang.items():
            if _cache.get(lang) is not None:
                self._build_lang_edges(lang, lang_frags, edges)

        return edges

    def _build_lang_edges(self, lang: str, lang_frags: list[Fragment], edges: EdgeDict) -> None:
        name_to_defs: dict[str, list[FragmentId]] = defaultdict(list)
        frag_tags: dict[FragmentId, _TagsInfo] = {}

        for f in lang_frags:
            info = _extract_tags(f.content, lang)
            if info is None:
                continue
            frag_tags[f.id] = info
            for name in info.definitions:
                name_to_defs[name].append(f.id)

        for f in lang_frags:
            info = frag_tags.get(f.id)
            if info is None:
                continue
            self_defs = info.definitions
            for ref_set, weight in [(info.calls, 0.40), (info.type_refs, 0.30), (info.references, 0.35)]:
                add_ref_edges(
                    edges,
                    f.id,
                    ref_set,
                    name_to_defs,
                    weight,
                    reverse_factor=self.reverse_weight_factor,
                    skip_self_defs=self_defs,
                )
