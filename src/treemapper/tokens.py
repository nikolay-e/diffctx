from __future__ import annotations

import sys
from dataclasses import dataclass

from _diffctx import count_tokens as _rust_count_tokens


@dataclass
class TokenCountResult:
    count: int
    is_exact: bool
    encoding: str


def count_tokens(text: str, encoding: str = "o200k_base") -> TokenCountResult:
    """Count tokens via the Rust tiktoken backend.

    Only `o200k_base` is currently supported; passing any other encoding
    raises `ValueError` to avoid silently mismatching the count and the
    label on the returned result.
    """
    if encoding != "o200k_base":
        raise ValueError(f"unsupported encoding {encoding!r}; only 'o200k_base' is available")
    return TokenCountResult(int(_rust_count_tokens(text)), True, "o200k_base")


def _format_size(byte_size: int) -> str:
    if byte_size < 1024:
        return f"{byte_size} B"
    if byte_size < 1024 * 1024:
        return f"{byte_size / 1024:.1f} KB"
    return f"{byte_size / (1024 * 1024):.1f} MB"


def print_token_summary(text: str, encoding: str = "o200k_base") -> None:
    result = count_tokens(text, encoding)
    text_len = len(text)
    if text_len > 10_000_000:
        sample = text[:100_000]
        ratio = len(sample.encode("utf-8")) / len(sample)
        byte_size = int(text_len * ratio)
    else:
        byte_size = len(text.encode("utf-8"))
    size = _format_size(byte_size)
    if result.is_exact:
        print(f"{result.count:,} tokens ({result.encoding}), {size}", file=sys.stderr)
    else:
        print(f"~{result.count:,} tokens ({result.encoding}), {size}", file=sys.stderr)
