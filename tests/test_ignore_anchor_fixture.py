from __future__ import annotations

import json
from pathlib import Path

import pytest

from diffctx.ignore import _process_ignore_line

_CASES = json.loads((Path(__file__).parent / "fixtures" / "ignore_anchor_cases.json").read_text())


@pytest.mark.parametrize("case", _CASES, ids=[f"{c['line']}@{c['rel'] or '/'}" for c in _CASES])
def test_python_anchoring_matches_the_shared_fixture(case):
    # The Rust `anchor_diffctx_ignore_line` runs the same table; the two used
    # to be kept in step by a "mirrors" comment alone.
    assert _process_ignore_line(case["line"], case["rel"]) == case["expected"]
