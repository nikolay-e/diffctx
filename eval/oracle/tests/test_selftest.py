import os
from pathlib import Path

import pytest

import selftest


@pytest.mark.parametrize("name", sorted(selftest.FIXTURES))
def test_the_oracle_finds_a_body_only_edit_a_deletion_and_a_move(name: str, tmp_path: Path):
    fixture = selftest.FIXTURES[name]
    repo = tmp_path / name
    base, head = selftest.build(fixture, repo)

    assert selftest.oracle_changed(fixture, repo, base, head) == fixture.changed


@pytest.mark.skipif(not os.environ.get("DIFFCTX_BIN"), reason="DIFFCTX_BIN names the engine under test")
@pytest.mark.parametrize("name", sorted(selftest.FIXTURES))
def test_the_engine_reports_the_body_only_edit_with_its_caller(name: str, tmp_path: Path):
    fixture = selftest.FIXTURES[name]
    repo = tmp_path / name
    base, head = selftest.build(fixture, repo)

    assert selftest.engine_problems(fixture, repo, base, head, Path(os.environ["DIFFCTX_BIN"])) == []
