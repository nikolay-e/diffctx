from __future__ import annotations

from .config import CODE_EXTENSIONS, CONFIG_EXTENSIONS, DOC_EXTENSIONS

__all__ = [
    "CODE_EXTENSIONS",
    "CONFIG_EXTENSIONS",
    "DOC_EXTENSIONS",
    "expand_config_key",
]


def expand_config_key(key: str) -> set[str]:
    if len(key) < 4:
        return set()
    result: set[str] = {key}
    if "_" in key:
        result.update(p for p in key.split("_") if len(p) >= 4)
        joined = key.replace("_", "")
        if len(joined) >= 5:
            result.add(joined)
    return result
