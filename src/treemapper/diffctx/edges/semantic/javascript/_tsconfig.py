from __future__ import annotations

import json
import re
from pathlib import Path

from ._resolve import _JSON_EXT, _resolve_absolute_import

_JSONC_LINE_COMMENT_RE = re.compile(r"//.*$", re.MULTILINE)
_JSONC_BLOCK_COMMENT_RE = re.compile(r"/\*.*?\*/", re.DOTALL)
_TRAILING_COMMA_RE = re.compile(r",\s*([}\]])")

_MAX_EXTENDS_DEPTH = 5


def _strip_jsonc_comments(text: str) -> str:
    text = _JSONC_BLOCK_COMMENT_RE.sub("", text)
    text = _JSONC_LINE_COMMENT_RE.sub("", text)
    text = _TRAILING_COMMA_RE.sub(r"\1", text)
    return text


class TsconfigResolver:
    def __init__(self, repo_root: Path):
        self._repo_root = repo_root
        self._config_cache: dict[Path, dict[str, object] | None] = {}

    def resolve(
        self,
        import_source: str,
        source_file: Path,
        candidate_set: set[Path],
    ) -> Path | None:
        config = self._find_config(source_file)
        if not config:
            return None

        paths = config.get("paths")
        if not isinstance(paths, dict) or not paths:
            return None

        base = self._resolve_base_dir(config)
        return self._match_path_patterns(paths, import_source, base, candidate_set)

    def _resolve_base_dir(self, config: dict[str, object]) -> Path:
        base_url = config.get("baseUrl", ".")
        config_dir = config.get("_config_dir", self._repo_root)
        if not isinstance(config_dir, Path):
            config_dir = Path(str(config_dir))
        if not isinstance(base_url, str):
            base_url = "."
        return config_dir / base_url

    @staticmethod
    def _match_path_patterns(
        paths: dict[str, object],
        import_source: str,
        base: Path,
        candidate_set: set[Path],
    ) -> Path | None:
        for pattern, targets in paths.items():
            if not isinstance(targets, list):
                continue
            prefix = pattern.replace("*", "")
            if not import_source.startswith(prefix):
                continue
            suffix = import_source[len(prefix) :]
            for target in targets:
                if not isinstance(target, str):
                    continue
                resolved_path = (base / target.replace("*", suffix)).resolve()
                result = _resolve_absolute_import(resolved_path, candidate_set)
                if result is not None:
                    return result
        return None

    def _find_config(self, source_file: Path) -> dict[str, object] | None:
        current = source_file.parent
        while True:
            if current in self._config_cache:
                return self._config_cache[current]

            tsconfig_path = current / "tsconfig.json"
            if tsconfig_path.is_file():
                config = self._load_config(tsconfig_path, 0)
                self._config_cache[current] = config
                return config

            if current == self._repo_root or current == current.parent:
                self._config_cache[current] = None
                return None
            current = current.parent

    def _load_config(self, config_path: Path, depth: int) -> dict[str, object] | None:
        if depth >= _MAX_EXTENDS_DEPTH:
            return None
        try:
            raw = config_path.read_text(encoding="utf-8")
            data = json.loads(_strip_jsonc_comments(raw))
        except (OSError, json.JSONDecodeError, UnicodeDecodeError):
            return None

        if not isinstance(data, dict):
            return None

        extends = data.get("extends")
        parent_opts: dict[str, object] = {}
        if isinstance(extends, str):
            parent_path = self._resolve_extends_path(extends, config_path)
            if parent_path and parent_path.is_file():
                parent_config = self._load_config(parent_path, depth + 1)
                if parent_config:
                    parent_opts = dict(parent_config)

        compiler_opts = data.get("compilerOptions", {})
        if not isinstance(compiler_opts, dict):
            compiler_opts = {}

        parent_compiler = parent_opts.get("compilerOptions", {})
        if not isinstance(parent_compiler, dict):
            parent_compiler = {}

        merged_compiler = {**parent_compiler, **compiler_opts}

        result: dict[str, object] = {}
        if "paths" in merged_compiler:
            result["paths"] = merged_compiler["paths"]
        if "baseUrl" in merged_compiler:
            result["baseUrl"] = merged_compiler["baseUrl"]
        result["_config_dir"] = config_path.parent

        return result

    @staticmethod
    def _resolve_extends_path(extends: str, config_path: Path) -> Path | None:
        if extends.startswith("."):
            parent_path = (config_path.parent / extends).resolve()
            if not parent_path.suffix:
                parent_path = parent_path.with_suffix(_JSON_EXT)
            return parent_path
        node_modules = config_path.parent / "node_modules" / extends
        if node_modules.is_file():
            return node_modules
        if node_modules.with_suffix(_JSON_EXT).is_file():
            return node_modules.with_suffix(_JSON_EXT)
        return None
