import sys
from dataclasses import dataclass
from functools import lru_cache
from typing import Any, Optional

CHUNK_SIZE = 500_000
CHUNK_THRESHOLD = 1_000_000
SAMPLE_CHAR_THRESHOLD = 50_000_000  # 50M characters - use sampling above this
SAMPLE_COUNT = 5


@dataclass
class TokenCountResult:
    count: int
    is_exact: bool
    encoding: str


@lru_cache(maxsize=4)
def _get_encoder(encoding: str) -> Optional[Any]:
    try:
        import tiktoken

        return tiktoken.get_encoding(encoding)
    except (ImportError, Exception):
        return None


def count_tokens(text: str, encoding: str = "o200k_base") -> TokenCountResult:
    encoder = _get_encoder(encoding)
    if not encoder:
        return TokenCountResult(len(text) // 4, False, "approximation")

    text_len = len(text)
    if text_len <= CHUNK_THRESHOLD:
        return TokenCountResult(len(encoder.encode(text)), True, encoding)

    if text_len > SAMPLE_CHAR_THRESHOLD:
        return _count_tokens_sampled(text, text_len, encoder, encoding)

    total = 0
    for i in range(0, text_len, CHUNK_SIZE):
        chunk = text[i : i + CHUNK_SIZE]
        total += len(encoder.encode(chunk))
    # BPE tokenizers are context-sensitive: chunking can give inaccurate counts
    return TokenCountResult(total, False, encoding)


def _count_tokens_sampled(text: str, text_len: int, encoder: Any, encoding: str) -> TokenCountResult:
    num_chunks = text_len // CHUNK_SIZE
    step = max(1, num_chunks // SAMPLE_COUNT)
    sampled_tokens = 0
    sampled_chars = 0  # len(chunk) returns characters, not bytes

    for i in range(0, num_chunks, step):
        start = i * CHUNK_SIZE
        chunk = text[start : start + CHUNK_SIZE]
        sampled_tokens += len(encoder.encode(chunk))
        sampled_chars += len(chunk)
        if sampled_chars >= SAMPLE_COUNT * CHUNK_SIZE:
            break

    if sampled_chars == 0:
        return TokenCountResult(text_len // 4, False, "approximation")

    # text_len is also in characters, so units are consistent
    tokens_per_char = sampled_tokens / sampled_chars
    estimated_total = int(tokens_per_char * text_len)
    return TokenCountResult(estimated_total, False, encoding)


def _format_size(byte_size: int) -> str:
    if byte_size < 1024:
        return f"{byte_size} B"
    elif byte_size < 1024 * 1024:
        return f"{byte_size / 1024:.1f} KB"
    else:
        return f"{byte_size / (1024 * 1024):.1f} MB"


def print_token_summary(text: str, encoding: str = "o200k_base") -> None:
    result = count_tokens(text, encoding)
    size = _format_size(len(text.encode("utf-8")))
    if result.is_exact:
        print(f"{result.count:,} tokens ({result.encoding}), {size}", file=sys.stderr)
    else:
        print(f"~{result.count:,} tokens ({result.encoding}), {size}", file=sys.stderr)
