from __future__ import annotations

import logging
import re
from pathlib import Path

from ..types import Fragment
from .base import MIN_FRAGMENT_LINES, YAML_EXTENSIONS, check_library_available, create_fragment_from_lines


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
        yaml.preserve_quotes = True

        try:
            data = yaml.load(content)
        except Exception as e:
            logging.debug("YAML parsing failed for %s: %s", path, e)
            return []

        if not hasattr(data, "lc") or not isinstance(data, dict):
            logging.debug("YAML data lacks location info or is not a dict: %s", path)
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
            key_re = re.compile(r'^\s{0,4}"([^"]+)":\s*')

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
