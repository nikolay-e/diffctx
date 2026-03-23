from __future__ import annotations

import re

from .config import CODE_EXTENSIONS, CONFIG_EXTENSIONS, DOC_EXTENSIONS

__all__ = [
    "CODE_EXTENSIONS",
    "CONFIG_EXTENSIONS",
    "DOC_EXTENSIONS",
    "expand_config_key",
]


def expand_config_key(key: str) -> set[str]:
    if len(key) < 2:
        return set()
    result: set[str] = {key}
    if "_" in key or "-" in key:
        parts = re.split(r"[_-]", key)
        result.update(p for p in parts if len(p) >= 3)
        joined = key.replace("_", "").replace("-", "")
        if len(joined) >= 4:
            result.add(joined)
    return result
