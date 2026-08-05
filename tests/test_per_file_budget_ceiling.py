"""#194: one large changed data file must not crowd every other file out.

The reporter's shape: a range touching one generated JSON blob plus many
source files emitted 364 of 370 sections from the blob and zero sections
from 34 source files. The selection contract pinned here: under a budget
the blob alone would exhaust, every changed source file still surfaces.
The leftover-flowback half of the ceiling (a lone file may exceed its
share once no competitor wants the budget) is pinned deterministically by
`per_file_ceiling_blocks_monopoly_but_releases_leftovers` in select.rs.
"""

from __future__ import annotations

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def test_blob_does_not_crowd_out_source_files(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    for i in range(6):
        repo.add_file(
            f"src/service_{i}.py",
            f"def handler_{i}(req):\n    return req + {i}\n",
        )
    records = ",\n".join(
        f'  {{"id": {j}, "word": "word_{j}", "translation": "tr_{j}", "example": "sentence {j}"}}' for j in range(800)
    )
    repo.add_file("data/records.json", "[\n" + records + "\n]\n")
    repo.commit("initial")

    for i in range(6):
        repo.add_file(
            f"src/service_{i}.py",
            f"def handler_{i}(req):\n    return req + {i} + 1\n",
        )
    records2 = ",\n".join(
        f'  {{"id": {j}, "word": "word_{j}", "translation": "tr_{j}", "example": "sentence {j} v2"}}' for j in range(1600)
    )
    repo.add_file("data/records.json", "[\n" + records2 + "\n]\n")
    repo.commit("bump data and services")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", budget_tokens=4000)
    frags = result.get("fragments") or []
    files = {f["path"] for f in frags}
    sources = {p for p in files if p.startswith("src/")}
    assert len(sources) == 6, f"every changed source file must survive the blob; got {sorted(files)}"
    assert "data/records.json" in files, "the blob itself must not vanish either"
