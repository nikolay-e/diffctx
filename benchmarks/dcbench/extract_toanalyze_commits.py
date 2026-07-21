"""Extract the 263 curated coverage-repo commits from test-repos/TOANALYZE.md
into dcbench instances with pending-annotation stubs.

Usage: python -m benchmarks.dcbench.extract_toanalyze_commits [--repos-root test-repos]
"""

from __future__ import annotations

import argparse
import re
import subprocess
from pathlib import Path

import yaml

DCBENCH = Path(__file__).parent
MAX_PATCH_BYTES = 900_000
SECTION_RE = re.compile(r"^## ([a-z0-9._-]+) \(\d+ commits?\)", re.MULTILINE)
ROW_RE = re.compile(r"^\| \d+ \| `([0-9a-f]{40})` \| (\d+) \| [^|]* \| (.+?) \|$", re.MULTILINE)


def git(repo: Path, *args: str) -> bytes | None:
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True)
    return r.stdout if r.returncode == 0 else None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repos-root", type=Path, default=DCBENCH.parent.parent / "test-repos")
    args = ap.parse_args()

    text = (args.repos_root / "TOANALYZE.md").read_text()
    curated_start = text.find("# Curated Commits")
    if curated_start < 0:
        raise SystemExit("Curated Commits section not found")
    text = text[curated_start:]

    sections = list(SECTION_RE.finditer(text))
    written = skipped = existing = 0
    for i, sec in enumerate(sections):
        repo_name = sec.group(1)
        body = text[sec.end() : sections[i + 1].start() if i + 1 < len(sections) else len(text)]
        repo = args.repos_root / repo_name
        if not (repo / ".git").exists():
            print(f"[skip-repo] {repo_name}: no clone")
            continue
        for row in ROW_RE.finditer(body):
            sha, n_files, description = row.group(1), int(row.group(2)), row.group(3).strip()
            inst_dir = DCBENCH / "instances" / f"{repo_name}__{sha[:7]}"
            if (inst_dir / "annotation.yaml").exists():
                existing += 1
                continue
            patch = git(repo, "format-patch", "-1", "--stdout", "--no-signature", sha)
            parent = (git(repo, "rev-parse", f"{sha}^") or b"").decode().strip()
            if not patch or not parent:
                print(f"[skip] {repo_name}@{sha[:7]}: patch/parent unavailable")
                skipped += 1
                continue
            inst_dir.mkdir(parents=True, exist_ok=True)
            truncated = len(patch) > MAX_PATCH_BYTES
            if not truncated:
                (inst_dir / "patch.diff").write_bytes(patch)
            annotation = {
                "repo": repo_name,
                "base_commit": parent,
                "commit": sha,
                "patch_stored": not truncated,
                "gold": [],
                "forbidden": [],
                "nontrivial_gold_count": 0,
                "annotator": "pending",
                "candidates_from": [],
                "curation": {"source": "TOANALYZE-2026-07-07", "n_files": n_files, "description": description},
                "notes": "",
            }
            (inst_dir / "annotation.yaml").write_text(yaml.safe_dump(annotation, sort_keys=False, allow_unicode=True, width=100))
            written += 1
    print(f"written={written} skipped={skipped} already_existing={existing}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
