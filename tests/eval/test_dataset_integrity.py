from __future__ import annotations

import hashlib
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
DATASET_VERSIONS = (
    REPO_ROOT / "datasets" / "dcbench" / "v1",
    REPO_ROOT / "datasets" / "real-world-diff" / "v1",
    REPO_ROOT / "datasets" / "eval-splits" / "v1",
)


@pytest.mark.parametrize("version_dir", DATASET_VERSIONS, ids=lambda path: str(path.relative_to(REPO_ROOT)))
def test_dataset_inventory_matches_checksums(version_dir: Path) -> None:
    inventory = version_dir / "checksums.sha256"
    assert (version_dir / "dataset.toml").is_file()
    assert (version_dir / "LICENSES.md").is_file()

    entries = [line.split("  ", 1) for line in inventory.read_text().splitlines() if line]
    expected_paths = {relative.removeprefix("./") for _, relative in entries}
    actual_paths = {
        str(path.relative_to(version_dir)) for path in version_dir.rglob("*") if path.is_file() and path.name != inventory.name
    }
    assert actual_paths == expected_paths

    for expected_digest, relative in entries:
        path = version_dir / relative.removeprefix("./")
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected_digest
