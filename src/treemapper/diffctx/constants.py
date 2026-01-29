from __future__ import annotations

from .config import CODE_EXTENSIONS, CONFIG_EXTENSIONS, DOC_EXTENSIONS

__all__ = [
    "CODE_EXTENSIONS",
    "CONFIG_EXTENSIONS",
    "DOC_EXTENSIONS",
    "expand_config_key",
]


def expand_config_key(key: str) -> set[str]:
    if len(key) < 6:
        return set()
    return {key}
