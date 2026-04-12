from __future__ import annotations

import logging
import re
from pathlib import Path

from ..types import Fragment
from .base import MIN_FRAGMENT_LINES, YAML_EXTENSIONS, check_library_available, create_fragment_from_lines

logger = logging.getLogger(__name__)

_TF_EXTENSIONS = {".tf", ".hcl"}
# Matches any top-level HCL block header: identifier optionally followed by quoted labels, then {
# Covers: resource, variable, data, output, module, locals, terraform, provider,
#         moved, import, check, removed, and any future HCL block types.
_TF_BLOCK_HEADER_RE = re.compile(r"^(resource|data|module|variable|output|locals|terraform|provider)\s+")

_TF_COMPOUND_REF_RE = re.compile(r"(?<![.\w])([a-zA-Z]\w*)\.(\w+)(?:\[\*?\w*\])?\.[\w\[\]*]+")
_TF_COMPOUND_REF_SKIP = frozenset({"var", "local", "data", "module", "path", "terraform", "count", "each", "self"})


def _tf_block_symbol(header_line: str) -> str | None:
    m = re.match(r'^resource\s+"([^"]+)"\s+"([^"]+)"', header_line)
    if m:
        return f"{m.group(1)}.{m.group(2)}"
    m = re.match(r'^data\s+"([^"]+)"\s+"([^"]+)"', header_line)
    if m:
        return f"data.{m.group(1)}.{m.group(2)}"
    m = re.match(r'^variable\s+"([^"]+)"', header_line)
    if m:
        return m.group(1)
    names = re.findall(r'"([^"]+)"', header_line)
    return names[-1] if names else None


def _tf_find_block_end(lines: list[str], start: int) -> int:
    depth = 0
    for i in range(start, len(lines)):
        for ch in lines[i]:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return i
    return len(lines) - 1


def _extract_compound_tf_refs(content: str) -> frozenset[str]:
    refs: set[str] = set()
    for m in _TF_COMPOUND_REF_RE.finditer(content):
        ref_type = m.group(1).lower()
        if ref_type not in _TF_COMPOUND_REF_SKIP:
            refs.add(f"{m.group(1).lower()}.{m.group(2).lower()}")
    return frozenset(refs)


class TerraformStrategy:
    priority = 46

    def can_handle(self, path: Path, _content: str) -> bool:
        return path.suffix.lower() in _TF_EXTENSIONS

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        blocks = _collect_tf_blocks(lines)
        if not blocks:
            return []

        return _assemble_tf_fragments(path, lines, blocks)


def _collect_tf_blocks(lines: list[str]) -> list[tuple[int, int, str | None]]:
    blocks: list[tuple[int, int, str | None]] = []
    i = 0
    while i < len(lines):
        if _TF_BLOCK_HEADER_RE.match(lines[i]):
            end_i = _tf_find_block_end(lines, i)
            sym = _tf_block_symbol(lines[i].strip())
            blocks.append((i, end_i, sym))
            i = end_i + 1
        else:
            i += 1
    return blocks


def _assemble_tf_fragments(path: Path, lines: list[str], blocks: list[tuple[int, int, str | None]]) -> list[Fragment]:
    fragments: list[Fragment] = []
    prev_end = -1

    for start_i, end_i, sym in blocks:
        if start_i > prev_end + 1:
            frag = create_fragment_from_lines(path, lines, prev_end + 2, start_i, "config", "data")
            if frag:
                fragments.append(frag)
        frag = create_fragment_from_lines(path, lines, start_i + 1, end_i + 1, "config", "data", symbol_name=sym)
        if frag:
            compound_refs = _extract_compound_tf_refs(frag.content)
            if compound_refs:
                frag.identifiers = frag.identifiers | compound_refs
            fragments.append(frag)
        prev_end = end_i

    if prev_end < len(lines) - 1:
        frag = create_fragment_from_lines(path, lines, prev_end + 2, len(lines), "config", "data")
        if frag:
            fragments.append(frag)

    return fragments


def _import_ruamel_yaml() -> None:
    from ruamel.yaml import YAML  # noqa: F401


class RuamelYamlStrategy:
    priority = 52

    def __init__(self) -> None:
        self._available = check_library_available(_import_ruamel_yaml)

    def can_handle(self, path: Path, _content: str) -> bool:
        if not self._available:
            return False
        return path.suffix.lower() in YAML_EXTENSIONS

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        from ruamel.yaml import YAML

        lines = content.splitlines()
        if not lines:
            return []

        yaml = YAML()
        try:
            data = yaml.load(content)
        except Exception as e:
            logger.debug("YAML parsing failed for %s: %s", path, e)
            return []

        if not hasattr(data, "lc") or not isinstance(data, dict):
            logger.debug("YAML data lacks location info or is not a dict: %s", path)
            return []

        fragments: list[Fragment] = []
        keys = list(data.keys())

        for i, key in enumerate(keys):
            start_line = data.lc.key(key)[0] + 1  # type: ignore[attr-defined]

            if i + 1 < len(keys):
                end_line = data.lc.key(keys[i + 1])[0]  # type: ignore[attr-defined]
            else:
                end_line = len(lines)

            if end_line - start_line + 1 < MIN_FRAGMENT_LINES:
                continue

            frag = create_fragment_from_lines(path, lines, start_line, end_line, "config", "data")
            if frag:
                fragments.append(frag)

        return fragments


class ConfigStrategy:
    priority = 50

    def can_handle(self, path: Path, _content: str) -> bool:
        return path.suffix.lower() in YAML_EXTENSIONS | {".json", ".toml"}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        suffix = path.suffix.lower()

        if suffix in YAML_EXTENSIONS:
            key_re = re.compile(r"^([a-zA-Z_][a-zA-Z0-9_-]*):\s*")
        elif suffix == ".toml":
            key_re = re.compile(r"^\[([a-zA-Z_][a-zA-Z0-9_.-]*)\]")
        else:
            key_re = re.compile(r'^\s*"([^"]+)":\s*')

        boundaries: list[int] = []
        for i, line in enumerate(lines):
            if key_re.match(line):
                boundaries.append(i)

        if not boundaries:
            frag = create_fragment_from_lines(path, lines, 1, len(lines), "config", "data")
            return [frag] if frag else []

        fragments: list[Fragment] = []
        boundaries.append(len(lines))

        for idx in range(len(boundaries) - 1):
            start, end = boundaries[idx], boundaries[idx + 1] - 1

            frag = create_fragment_from_lines(path, lines, start + 1, end + 1, "config", "data")
            if frag:
                fragments.append(frag)

        return fragments
