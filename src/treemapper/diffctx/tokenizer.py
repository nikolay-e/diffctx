from __future__ import annotations

import re
import threading
from pathlib import Path
from typing import TYPE_CHECKING

from .constants import CODE_EXTENSIONS, CONFIG_EXTENSIONS, DOC_EXTENSIONS

if TYPE_CHECKING:
    from spacy.language import Language

_FALLBACK_WORD_RE = re.compile(r"\b[a-zA-Z]\w*\b")
_MIN_TOKEN_LEN = 3
_MAX_TEXT_LEN = 50_000

_nlp: Language | None = None
_nlp_available: bool | None = None
_nlp_lock = threading.Lock()


def _get_nlp() -> Language | None:
    global _nlp, _nlp_available

    if _nlp_available is False:
        return None

    if _nlp is None:
        with _nlp_lock:
            if _nlp is None:
                try:
                    import spacy

                    try:
                        _nlp = spacy.load("en_core_web_sm", disable=["ner", "parser"])
                    except OSError:
                        from spacy.cli.download import download

                        download("en_core_web_sm")
                        _nlp = spacy.load("en_core_web_sm", disable=["ner", "parser"])
                    _nlp_available = True
                except ImportError:
                    _nlp_available = False
                    return None
                except Exception:
                    _nlp_available = False
                    return None

    return _nlp


def _extract_tokens_nlp(text: str) -> frozenset[str]:
    nlp = _get_nlp()
    if nlp is None:
        return _extract_tokens_fallback(text)

    if len(text) > _MAX_TEXT_LEN:
        text = text[:_MAX_TEXT_LEN]

    doc = nlp(text)
    return frozenset(
        token.lemma_.lower()
        for token in doc
        if token.is_alpha and len(token.lemma_) >= _MIN_TOKEN_LEN and not token.is_stop and not token.is_punct
    )


def _extract_tokens_fallback(text: str) -> frozenset[str]:
    words = _FALLBACK_WORD_RE.findall(text.lower())
    return frozenset(w for w in words if len(w) >= _MIN_TOKEN_LEN)


def _extract_token_list_nlp(text: str) -> list[str]:
    nlp = _get_nlp()
    if nlp is None:
        words = _FALLBACK_WORD_RE.findall(text.lower())
        return [w for w in words if len(w) >= _MIN_TOKEN_LEN]

    if len(text) > _MAX_TEXT_LEN:
        text = text[:_MAX_TEXT_LEN]

    doc = nlp(text)
    return [
        token.lemma_.lower()
        for token in doc
        if token.is_alpha and len(token.lemma_) >= _MIN_TOKEN_LEN and not token.is_stop and not token.is_punct
    ]


def extract_tokens(text: str, *, profile: str = "auto", use_nlp: bool = True) -> frozenset[str]:
    if profile == "code" or not use_nlp:
        return _extract_tokens_fallback(text)
    return _extract_tokens_nlp(text)


def extract_token_list(text: str, *, profile: str = "auto", use_nlp: bool = True) -> list[str]:
    if profile == "code" or not use_nlp:
        words = _FALLBACK_WORD_RE.findall(text.lower())
        return [w for w in words if len(w) >= _MIN_TOKEN_LEN]
    return _extract_token_list_nlp(text)


def detect_profile(path_str: str) -> str:
    p = Path(path_str)
    suffix = p.suffix.lower()

    if suffix in CODE_EXTENSIONS:
        return "code"

    if suffix in DOC_EXTENSIONS or suffix in {".markdown", ".tex", ".latex"}:
        return "docs"

    if suffix in CONFIG_EXTENSIONS or suffix in {".env"}:
        return "data"

    return "generic"


def is_nlp_available() -> bool:
    _get_nlp()
    return _nlp_available is True
