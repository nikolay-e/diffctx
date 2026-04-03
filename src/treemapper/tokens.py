from __future__ import annotations

import logging
import sys
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)


@dataclass
class TokenCountResult:
    count: int
    is_exact: bool
    encoding: str


_encoder_cache: dict[str, Any] = {}


def _get_encoder(encoding: str) -> Any | None:
    if encoding in _encoder_cache:
        return _encoder_cache[encoding]
    try:
        import tiktoken

        enc = tiktoken.get_encoding(encoding)
        _encoder_cache[encoding] = enc
        return enc
    except Exception:
        return None


def count_tokens(text: str, encoding: str = "o200k_base") -> TokenCountResult:
    encoder = _get_encoder(encoding)
    if not encoder:
        logger.debug("tiktoken unavailable, using char/4 approximation")
        return TokenCountResult(len(text) // 4, False, "approximation")

    return TokenCountResult(len(encoder.encode_ordinary(text)), True, encoding)


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
