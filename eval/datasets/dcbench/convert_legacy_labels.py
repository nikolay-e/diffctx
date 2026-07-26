"""Convert real-world-diff 10-reviewer labels into dcbench tier-R instances.

Usage: python -m eval convert-legacy-labels [--repos-root test-repos]
"""

from __future__ import annotations

import argparse
import csv
import json
import subprocess
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parents[3]
DCBENCH = REPO_ROOT / "datasets" / "dcbench" / "v1"
LEGACY = REPO_ROOT / "datasets" / "real-world-diff" / "v1"
MAX_PATCH_BYTES = 900_000


def git(repo: Path, *args: str) -> str | None:
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True)
    return r.stdout.decode("utf-8", errors="replace") if r.returncode == 0 else None


def changed_files_of(patch: str) -> set[str]:
    out = set()
    for line in patch.splitlines():
        if line.startswith("diff --git a/"):
            tail = line[len("diff --git a/") :]
            parts = tail.split(" b/", 1)
            if len(parts) == 2:
                out.update(parts)
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repos-root", type=Path, default=REPO_ROOT / "test-repos")
    args = ap.parse_args()

    shas = {}
    with open(LEGACY / "commits.tsv") as f:
        for row in csv.reader(f, delimiter="\t"):
            if len(row) >= 3:
                shas[f"{row[0]}/{int(row[1]):03d}-{row[2][:7]}"] = (row[0], row[2])

    labels = json.loads((LEGACY / "gold_labels.json").read_text())
    written = skipped = 0
    for entry in labels:
        key = entry["commit"]
        if key not in shas:
            print(f"[skip] {key}: not in commits.tsv")
            skipped += 1
            continue
        repo_name, sha = shas[key]
        repo = args.repos_root / repo_name
        patch = git(repo, "format-patch", "-1", "--stdout", "--no-signature", sha)
        parent = (git(repo, "rev-parse", f"{sha}^") or "").strip()
        if not patch or not parent:
            print(f"[skip] {key}: cannot extract patch/parent from {repo}")
            skipped += 1
            continue
        inst_dir = DCBENCH / "instances" / f"{repo_name}__{sha[:7]}"
        inst_dir.mkdir(parents=True, exist_ok=True)

        patch_bytes = patch.encode("utf-8", errors="surrogateescape")
        truncated = len(patch_bytes) > MAX_PATCH_BYTES
        if not truncated:
            (inst_dir / "patch.diff").write_bytes(patch_bytes)

        changed = changed_files_of(patch)
        gold = [
            {"path": p, "tier": "essential", "role": "unspecified", "in_diff": p in changed}
            for p in sorted(set(entry.get("should_include", [])))
        ]
        annotation = {
            "repo": repo_name,
            "base_commit": parent,
            "commit": sha,
            "patch_stored": not truncated,
            "gold": gold,
            "forbidden": [{"path": p} for p in sorted(set(entry.get("should_not_include", [])))],
            "nontrivial_gold_count": sum(1 for g in gold if not g["in_diff"]),
            "annotator": "legacy-review-2026-06",
            "candidates_from": ["review"],
            "notes": entry.get("concern_log", ""),
        }
        (inst_dir / "annotation.yaml").write_text(yaml.safe_dump(annotation, sort_keys=False, allow_unicode=True, width=100))
        written += 1

    print(f"written={written} skipped={skipped}")
    nontrivial = sum(
        1 for p in (DCBENCH / "instances").glob("*/annotation.yaml") if yaml.safe_load(p.read_text())["nontrivial_gold_count"] > 0
    )
    print(f"instances with nontrivial gold: {nontrivial}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
