"""Annotate every gold entry with its dependency-graph hop distance from the
diff files, computed on the post-change worktree via the project graph.

Writes `hop: N` (N = BFS distance over the file-projected undirected graph;
-1 = unreachable/not in graph) into each gold entry and `max_gold_hop` at the
instance level.

Usage: python -m eval annotate-dcbench-hops [--repos-root test-repos]
           [--only-annotated] [--limit N] [--jobs 4]
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from collections import defaultdict, deque
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parents[3]
DCBENCH = REPO_ROOT / "datasets" / "dcbench" / "v1"


def file_hops(worktree: Path, diff_files: set[str], targets: set[str]) -> dict[str, int]:
    from diffctx._native.graph_export import graph_to_json_string
    from diffctx._native.project_graph import build_project_graph

    doc = json.loads(graph_to_json_string(build_project_graph(worktree)))
    adj: dict[str, set[str]] = defaultdict(set)
    nodes = {n["path"] for n in doc["nodes"]}
    for e in doc["edges"]:
        s = e["source"].rsplit(":", 1)[0]
        t = e["target"].rsplit(":", 1)[0]
        if s != t:
            adj[s].add(t)
            adj[t].add(s)
    dist: dict[str, int] = {f: 0 for f in diff_files if f in nodes}
    queue = deque(dist)
    while queue:
        u = queue.popleft()
        for v in adj[u]:
            if v not in dist:
                dist[v] = dist[u] + 1
                queue.append(v)
    return {t: dist.get(t, -1) for t in targets}


def process(inst_dir: str, repos_root: str) -> str:
    inst = Path(inst_dir)
    ann_path = inst / "annotation.yaml"
    ann = yaml.safe_load(ann_path.read_text())
    gold = ann.get("gold") or []
    if not gold:
        return f"{inst.name}: skip (no gold)"
    if all("hop" in g for g in gold):
        return f"{inst.name}: skip (done)"
    repo = Path(repos_root) / ann["repo"]
    r = subprocess.run(
        ["git", "-C", str(repo), "diff-tree", "--no-commit-id", "--name-only", "-r", ann["commit"]],
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        return f"{inst.name}: FAIL diff-tree"
    diff_files = {line.strip() for line in r.stdout.splitlines() if line.strip()}
    targets = {g["path"] for g in gold}
    with tempfile.TemporaryDirectory() as td:
        wt = Path(td) / "wt"
        c = subprocess.run(
            ["git", "-C", str(repo), "worktree", "add", "--detach", "-q", str(wt), ann["commit"]],
            capture_output=True,
            text=True,
        )
        if c.returncode != 0:
            return f"{inst.name}: FAIL worktree {c.stderr.strip()[:80]}"
        try:
            hops = file_hops(wt, diff_files, targets)
        except Exception as e:
            return f"{inst.name}: FAIL graph {e}"
        finally:
            subprocess.run(["git", "-C", str(repo), "worktree", "remove", "--force", str(wt)], capture_output=True)
    for g in gold:
        g["hop"] = hops.get(g["path"], -1)
    ann["max_gold_hop"] = max((g["hop"] for g in gold), default=-1)
    ann["hop_graph_params"] = {
        "max_edges_per_node": int(os.environ.get("DIFFCTX_MAX_EDGES_PER_NODE", "64")),
        "projection": "file-level undirected BFS from diff files",
    }
    ann_path.write_text(yaml.safe_dump(ann, sort_keys=False, allow_unicode=True, width=100))
    return f"{inst.name}: ok max_hop={ann['max_gold_hop']}"


def gold_instance_dirs(only_annotated: bool, limit: int) -> list[str]:
    dirs = []
    for inst in sorted((DCBENCH / "instances").iterdir()):
        ann = yaml.safe_load((inst / "annotation.yaml").read_text())
        if only_annotated and ann.get("annotator") == "pending":
            continue
        if ann.get("gold"):
            dirs.append(str(inst))
    return dirs[:limit] if limit else dirs


def run_isolated(inst_dir: str, repos_root: Path, timeout_s: int) -> str:
    cmd = [
        sys.executable,
        "-m",
        "eval.datasets.dcbench.annotate_hops",
        "--repos-root",
        str(repos_root),
        "--single",
        inst_dir,
    ]
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout_s, cwd=str(REPO_ROOT))
    except subprocess.TimeoutExpired:
        return f"{Path(inst_dir).name}: FAIL timeout>{timeout_s}s (graph build, #116 class)"
    out = (r.stdout or "").strip().splitlines()
    if r.returncode != 0:
        return f"{Path(inst_dir).name}: FAIL rc={r.returncode} (worker died: OOM/abort, #116 class)"
    return out[-1] if out else f"{Path(inst_dir).name}: FAIL no output"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repos-root", type=Path, default=REPO_ROOT / "test-repos")
    ap.add_argument("--only-annotated", action="store_true")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--single", type=Path, default=None)
    ap.add_argument("--per-instance-timeout", type=int, default=600)
    args = ap.parse_args()

    if args.single:
        result = process(str(args.single), str(args.repos_root))
        print(result, flush=True)
        return 1 if ": FAIL" in result else 0

    dirs = gold_instance_dirs(args.only_annotated, args.limit)
    print(f"annotating hops for {len(dirs)} instances")

    failures = 0
    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for res in pool.map(lambda d: run_isolated(d, args.repos_root, args.per_instance_timeout), dirs):
            print(res, flush=True)
            failures += ": FAIL" in res
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
