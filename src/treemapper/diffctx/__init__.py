from __future__ import annotations

from .pipeline import build_diff_context


class GitError(Exception):
    """Raised when an underlying git operation fails.

    Mirrors the Rust `_diffctx` error surface as a plain Python exception so
    callers do not need to depend on PyO3-side classes.
    """


__all__ = ["GitError", "build_diff_context"]
