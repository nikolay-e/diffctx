"""Verify every dcbench instance is reproducible: pinned base_commit exists and
patch.diff applies cleanly with strict `git apply --index --check`.

Usage: python -m benchmarks.dcbench.verify_instances [--repos-root test-repos] [--jobs 8]
"""

from __future__ import annotations

import argparse
import subprocess
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import yaml

DCBENCH = Path(__file__).parent
REQUIRED_KEYS = {"repo", "base_commit", "commit", "gold", "annotator"}


def run(cmd: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)


def verify(inst_dir: Path, repos_root: Path) -> tuple[str, str]:
    name = inst_dir.name
    ann_path = inst_dir / "annotation.yaml"
    if not ann_path.exists():
        return name, "FAIL: missing annotation.yaml"
    ann = yaml.safe_load(ann_path.read_text())
    missing = REQUIRED_KEYS - set(ann)
    if missing:
        return name, f"FAIL: annotation missing keys {sorted(missing)}"
    repo = repos_root / ann["repo"]
    if not (repo / ".git").exists():
        return name, f"FAIL: repo clone missing ({ann['repo']})"
    for sha_key in ("base_commit", "commit"):
        r = run(["git", "-C", str(repo), "cat-file", "-e", f"{ann[sha_key]}^{{commit}}"])
        if r.returncode != 0:
            return name, f"FAIL: {sha_key} {ann[sha_key][:12]} not in repo"
    if not ann.get("patch_stored", True):
        return name, "OK (patch not stored, size-capped; commit pinned)"
    patch = inst_dir / "patch.diff"
    if not patch.exists():
        return name, "FAIL: patch_stored=true but patch.diff missing"
    with tempfile.TemporaryDirectory() as td:
        wt = Path(td) / "wt"
        r = run(["git", "-C", str(repo), "worktree", "add", "--detach", "-q", str(wt), ann["base_commit"]])
        if r.returncode != 0:
            return name, f"FAIL: worktree at base_commit: {r.stderr.strip()[:120]}"
        try:
            r = run(["git", "-C", str(wt), "apply", "--cached", "--check", str(patch.resolve())])
            if r.returncode != 0:
                return name, f"FAIL: apply --cached --check: {r.stderr.strip()[:120]}"
        finally:
            run(["git", "-C", str(repo), "worktree", "remove", "--force", str(wt)])
    return name, "OK"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repos-root", type=Path, default=DCBENCH.parent.parent / "test-repos")
    ap.add_argument("--jobs", type=int, default=8)
    args = ap.parse_args()

    dirs = sorted(p for p in (DCBENCH / "instances").iterdir() if p.is_dir())
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        results = list(pool.map(lambda d: verify(d, args.repos_root), dirs))
    fails = [(n, s) for n, s in results if s.startswith("FAIL")]
    capped = sum(1 for _, s in results if "not stored" in s)
    for n, s in fails:
        print(f"{n}: {s}")
    print(f"total={len(results)} ok={len(results) - len(fails)} (incl. {capped} size-capped) fail={len(fails)}")
    return 1 if fails else 0


if __name__ == "__main__":
    raise SystemExit(main())
