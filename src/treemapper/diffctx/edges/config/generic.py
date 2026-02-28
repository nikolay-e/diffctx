from __future__ import annotations

import re
from pathlib import Path

from ...constants import CODE_EXTENSIONS, expand_config_key
from ...types import Fragment
from ..base import EdgeBuilder, EdgeDict

_CONFIG_EXTENSIONS = {".yaml", ".yml", ".json", ".toml", ".ini", ".env"}

_CONFIG_KEY_STOPWORDS = frozenset(
    {
        "action",
        "actions",
        "assert",
        "author",
        "before",
        "branch",
        "change",
        "client",
        "config",
        "create",
        "default",
        "delete",
        "deploy",
        "description",
        "enable",
        "engine",
        "engines",
        "export",
        "exports",
        "format",
        "health",
        "ignore",
        "import",
        "inputs",
        "keywords",
        "module",
        "modules",
        "number",
        "object",
        "openapi",
        "option",
        "options",
        "output",
        "outputs",
        "params",
        "plugin",
        "plugins",
        "private",
        "public",
        "remove",
        "render",
        "report",
        "require",
        "result",
        "return",
        "script",
        "scripts",
        "server",
        "source",
        "status",
        "string",
        "target",
        "update",
        "verbose",
        "version",
    }
)

_CONFIG_KEY_RE = re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*:", re.MULTILINE)
_JSON_KEY_RE = re.compile(r'"([a-zA-Z_][a-zA-Z0-9_-]*)"\s*:')
_TOML_KEY_RE = re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*=", re.MULTILINE)
_INI_KEY_RE = re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*=", re.MULTILINE)
_ENV_KEY_RE = re.compile(r"^([A-Za-z_]\w*)\s*=", re.MULTILINE)


def _get_patterns_for_suffix(suffix: str) -> list[re.Pattern[str]]:
    patterns_map = {
        ".yaml": [_CONFIG_KEY_RE],
        ".yml": [_CONFIG_KEY_RE],
        ".json": [_JSON_KEY_RE],
        ".toml": [_TOML_KEY_RE],
        ".ini": [_INI_KEY_RE],
        ".env": [_ENV_KEY_RE],
    }
    return patterns_map.get(suffix, [])


def _extract_config_keys_from_content(suffix: str, content: str) -> set[str]:
    patterns = _get_patterns_for_suffix(suffix)
    keys: set[str] = set()
    for pattern in patterns:
        for match in pattern.finditer(content):
            raw_key = match.group(1).lower()
            keys.update(expand_config_key(raw_key))
    return keys


def _is_config_file(path: Path) -> bool:
    return path.suffix.lower() in _CONFIG_EXTENSIONS


def _is_code_file(path: Path) -> bool:
    return path.suffix.lower() in CODE_EXTENSIONS


class ConfigToCodeEdgeBuilder(EdgeBuilder):
    weight = 0.45
    reverse_weight_factor = 0.70
    category = "config_generic"

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        config_changed = [f for f in changed_files if _is_config_file(f)]
        if not config_changed:
            return []

        all_keys: set[str] = set()
        for cfg_file in config_changed:
            try:
                content = cfg_file.read_text(encoding="utf-8")
                all_keys.update(_extract_config_keys_from_content(cfg_file.suffix.lower(), content))
            except (OSError, UnicodeDecodeError):
                continue

        if not all_keys:
            return []

        key_patterns = self._build_key_patterns(all_keys)
        discovered: list[Path] = []
        changed_set = set(changed_files)

        for candidate in all_candidate_files:
            if candidate in changed_set or not _is_code_file(candidate):
                continue
            if self._file_matches_any_key(candidate, key_patterns):
                discovered.append(candidate)

        return discovered

    def _build_key_patterns(self, keys: set[str]) -> dict[str, re.Pattern[str]]:
        patterns: dict[str, re.Pattern[str]] = {}
        for key in keys:
            if len(key) >= 6 and key not in _CONFIG_KEY_STOPWORDS:
                patterns[key] = re.compile(rf"\b{re.escape(key)}\b", re.IGNORECASE)
        return patterns

    def _file_matches_any_key(self, file_path: Path, patterns: dict[str, re.Pattern[str]]) -> bool:
        try:
            content = file_path.read_text(encoding="utf-8")
            for pattern in patterns.values():
                if pattern.search(content):
                    return True
            return False
        except (OSError, UnicodeDecodeError):
            return False

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        config_frags = [f for f in fragments if _is_config_file(f.path)]
        code_frags = [f for f in fragments if _is_code_file(f.path)]

        if not config_frags or not code_frags:
            return {}

        edges: EdgeDict = {}
        for cfg in config_frags:
            keys = _extract_config_keys_from_content(cfg.path.suffix.lower(), cfg.content)
            if not keys:
                continue
            key_patterns = self._build_key_patterns(keys)
            for code_frag in code_frags:
                if self._fragment_matches_keys(code_frag.content, key_patterns):
                    self.add_edge(edges, cfg.id, code_frag.id)

        return edges

    def _fragment_matches_keys(self, content: str, patterns: dict[str, re.Pattern[str]]) -> bool:
        for pattern in patterns.values():
            if pattern.search(content):
                return True
        return False
