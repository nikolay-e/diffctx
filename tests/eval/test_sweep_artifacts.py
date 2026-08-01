"""Sweep artifact directory names must parse for every method the matrix runs.

`aggregate_sweep` reads `metadata.json` when a cell produced one and falls back
to parsing the directory name when it did not — which is exactly the case that
matters, because a cell that died before writing metadata is the one being
diagnosed. The fallback pattern excluded `-`, so `cell-internal-bm25-...` parsed
as no method at all, and the control arm of the #125 fusion gate would have gone
missing from the comparison precisely when a cell failed.
"""

from __future__ import annotations

import pytest

from eval.analysis.aggregate_sweep import _METHOD_ORDER, _method_sort_key, _parse_artifact


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("cell-ego-b8000-L0-swebench", ("ego", 8000, 0, "swebench")),
        ("cell-rrf-b8000-L2-swebench", ("rrf", 8000, 2, "swebench")),
        # The hyphenated one. Unambiguous despite `-` being in the method class,
        # because the next segment must be `b<digits|ALL>`.
        ("cell-internal-bm25-b8000-L0-swebench", ("internal-bm25", 8000, 0, "swebench")),
        ("cell-internal-bm25-bALL-L1-dcbench", ("internal-bm25", None, 1, "dcbench")),
        # Legacy layout, no depth segment: depth reads as the -1 sentinel.
        ("cell-internal-bm25-b8000-swebench", ("internal-bm25", 8000, -1, "swebench")),
    ],
)
def test_every_matrix_method_parses_from_its_directory_name(name, expected):
    assert _parse_artifact(name) == expected


def test_a_method_the_matrix_runs_has_a_column_position():
    """An unknown method sorts to rank 99 rather than failing, so a missing
    entry is invisible in the output — the table just puts it last. Both fusion
    arms have to be beside the modes they are compared against."""
    for method in ("rrf", "internal-bm25", "bm25", "ego"):
        assert method in _METHOD_ORDER, f"{method} would sort to the tail unnoticed"
    assert _method_sort_key("internal-bm25") == _METHOD_ORDER.index("internal-bm25")
    assert _method_sort_key("not-a-method") == 99
