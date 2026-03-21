from __future__ import annotations

import logging
import sys
from dataclasses import dataclass
from typing import Any

logger = logging.getLogger(__name__)

CHUNK_SIZE = 500_000
CHUNK_THRESHOLD = 1_000_000
SAMPLE_CHAR_THRESHOLD = 50_000_000  # 50M characters - use sampling above this
SAMPLE_COUNT = 5


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

    text_len = len(text)
    if text_len <= CHUNK_THRESHOLD:
        logger.debug("Token counting: exact mode (%d chars)", text_len)
        return TokenCountResult(len(encoder.encode(text)), True, encoding)

    if text_len > SAMPLE_CHAR_THRESHOLD:
        logger.debug("Token counting: sampled mode (%d chars, %d samples)", text_len, SAMPLE_COUNT)
        return _count_tokens_sampled(text, text_len, encoder, encoding)

    logger.debug("Token counting: chunked mode (%d chars)", text_len)
    total = 0
    for i in range(0, text_len, CHUNK_SIZE):
        chunk = text[i : i + CHUNK_SIZE]
        total += len(encoder.encode(chunk))
    return TokenCountResult(total, False, encoding)


def _count_tokens_sampled(text: str, text_len: int, encoder: Any, encoding: str) -> TokenCountResult:
    num_chunks = text_len // CHUNK_SIZE
    step = max(1, num_chunks // SAMPLE_COUNT)
    sampled_tokens = 0
    sampled_chars = 0

    for i in range(0, num_chunks, step):
        start = i * CHUNK_SIZE
        chunk = text[start : start + CHUNK_SIZE]
        sampled_tokens += len(encoder.encode(chunk))
        sampled_chars += len(chunk)
        if sampled_chars >= SAMPLE_COUNT * CHUNK_SIZE:
            break

    if sampled_chars == 0:
        return TokenCountResult(text_len // 4, False, "approximation")

    tokens_per_char = sampled_tokens / sampled_chars
    estimated_total = int(tokens_per_char * text_len)
    return TokenCountResult(estimated_total, False, encoding)


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
