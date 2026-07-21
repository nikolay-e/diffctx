"""Generate multi-source annotation candidates for pending dcbench instances.

Bias guard (README, rule 2): candidates come from four independent generators
so gold labels are not skewed toward any single retrieval signal:
  cochange — files historically co-committed with the diff files (pre-base history)
  bm25     — lexical retrieval over the worktree with patch identifiers
  graph    — diffctx EGO selection at a generous budget
  random   — distractors

Writes `candidates.yaml` next to each instance's annotation.yaml:
  candidates: [{path, sources: [..]}...]  (diff files excluded)

Usage: python -m benchmarks.dcbench.gen_candidates [--limit N] [--jobs 3]
           [--single <inst_dir>] [--per-instance-timeout 600]
"""

from __future__ import annotations

import argparse
import random
import subprocess
import sys
import tempfile
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import yaml

from benchmarks.baselines._idents import code_tokenize, extract_idents_from_patch
from benchmarks.baselines.bm25_baseline import _walk_repo_files

DCBENCH = Path(__file__).parent
TOP_COCHANGE = 15
TOP_BM25 = 15
TOP_GRAPH = 20
N_RANDOM = 10
COCHANGE_HISTORY = 200


COCHANGE_DEADLINE_S = 30


def git(repo: Path, *args: str, timeout: int = 60) -> str:
    try:
        r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return ""
    return r.stdout if r.returncode == 0 else ""


MAX_COCHANGE_PATHSPECS = 10


def cochange_candidates(repo: Path, base: str, diff_files: set[str]) -> list[str]:
    paths = list(diff_files)[:MAX_COCHANGE_PATHSPECS]
    shas = git(repo, "log", "--format=%H", f"-n{COCHANGE_HISTORY}", base, "--", *paths, timeout=COCHANGE_DEADLINE_S).split()
    if not shas:
        return []
    try:
        r = subprocess.run(
            ["git", "-C", str(repo), "diff-tree", "--no-commit-id", "--name-only", "-r", "--stdin"],
            input="\n".join(shas) + "\n",
            capture_output=True,
            text=True,
            timeout=COCHANGE_DEADLINE_S,
        )
    except subprocess.TimeoutExpired:
        return []
    counts: Counter[str] = Counter()
    for line in (r.stdout or "").splitlines():
        f = line.strip()
        if not f or f in diff_files:
            continue
        if len(f) == 40 and all(c in "0123456789abcdef" for c in f):
            continue
        counts[f] += 1
    return [f for f, _ in counts.most_common(TOP_COCHANGE)]


def bm25_candidates(worktree: Path, patch_text: str, diff_files: set[str]) -> list[str]:
    from rank_bm25 import BM25Okapi

    idents = sorted(extract_idents_from_patch(patch_text))
    if not idents:
        return []
    corpus, rel_paths = [], []
    for full in _walk_repo_files(worktree):
        try:
            toks = code_tokenize(full.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            continue
        if toks:
            corpus.append(toks)
            rel_paths.append(full.relative_to(worktree).as_posix())
    if not corpus:
        return []
    scores = BM25Okapi(corpus).get_scores(idents)
    ranked = sorted(zip(rel_paths, scores), key=lambda x: -x[1])
    return [p for p, s in ranked if s > 0 and p not in diff_files][:TOP_BM25]


GRAPH_TIMEOUT_S = 240
STANDALONE = DCBENCH.parent.parent / "diffctx" / "target" / "release" / "diffctx"


def graph_candidates(worktree: Path, diff_files: set[str]) -> list[str]:
    if not STANDALONE.exists():
        return []
    r = subprocess.run(
        [
            str(STANDALONE),
            str(worktree),
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            "32000",
            "--format",
            "yaml",
            "--no-content",
            "--timeout",
            str(GRAPH_TIMEOUT_S),
        ],
        capture_output=True,
        text=True,
        timeout=GRAPH_TIMEOUT_S + 60,
    )
    if r.returncode != 0:
        return []
    seen: list[str] = []
    for line in r.stdout.splitlines():
        stripped = line.strip()
        if stripped.startswith("- path:"):
            p = stripped.split(":", 1)[1].strip().strip('"')
            if p and p not in diff_files and p not in seen:
                seen.append(p)
    return seen[:TOP_GRAPH]


def random_candidates(worktree: Path, diff_files: set[str], exclude: set[str]) -> list[str]:
    pool = [
        p.relative_to(worktree).as_posix()
        for p in _walk_repo_files(worktree)
        if p.relative_to(worktree).as_posix() not in diff_files
    ]
    pool = [p for p in pool if p not in exclude]
    rng = random.Random(42)
    return rng.sample(pool, min(N_RANDOM, len(pool)))


def process(inst_dir: str, repos_root: str) -> str:
    inst = Path(inst_dir)
    out_path = inst / "candidates.yaml"
    if out_path.exists():
        return f"{inst.name}: skip (done)"
    ann = yaml.safe_load((inst / "annotation.yaml").read_text())
    repo = Path(repos_root) / ann["repo"]
    patch_file = inst / "patch.diff"
    patch_text = (
        patch_file.read_text(errors="replace")
        if patch_file.exists()
        else git(repo, "format-patch", "-1", "--stdout", "--no-signature", ann["commit"])
    )
    diff_files = {
        f.strip() for f in git(repo, "diff-tree", "--no-commit-id", "--name-only", "-r", ann["commit"]).splitlines() if f.strip()
    }
    if not diff_files:
        return f"{inst.name}: FAIL no diff files"

    with tempfile.TemporaryDirectory() as td:
        wt = Path(td) / "wt"
        c = subprocess.run(
            ["git", "-C", str(repo), "worktree", "add", "--detach", "-q", str(wt), ann["commit"]], capture_output=True, text=True
        )
        if c.returncode != 0:
            return f"{inst.name}: FAIL worktree {c.stderr.strip()[:80]}"
        try:
            sources = {
                "cochange": cochange_candidates(repo, ann["base_commit"], diff_files),
                "bm25": bm25_candidates(wt, patch_text, diff_files),
                "graph": graph_candidates(wt, diff_files),
            }
            non_random = {p for v in sources.values() for p in v}
            sources["random"] = random_candidates(wt, diff_files, non_random)
        except Exception as e:
            return f"{inst.name}: FAIL {type(e).__name__}: {e}"
        finally:
            subprocess.run(["git", "-C", str(repo), "worktree", "remove", "--force", str(wt)], capture_output=True)

    merged: dict[str, list[str]] = {}
    for src, paths in sources.items():
        for p in paths:
            merged.setdefault(p, []).append(src)
    doc = {
        "generators": {k: len(v) for k, v in sources.items()},
        "candidates": [{"path": p, "sources": s} for p, s in sorted(merged.items())],
    }
    out_path.write_text(yaml.safe_dump(doc, sort_keys=False, allow_unicode=True, width=100))
    return f"{inst.name}: ok candidates={len(merged)} ({doc['generators']})"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repos-root", type=Path, default=DCBENCH.parent.parent / "test-repos")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--jobs", type=int, default=3)
    ap.add_argument("--single", type=Path, default=None)
    ap.add_argument("--per-instance-timeout", type=int, default=600)
    ap.add_argument("--pending-only", action="store_true", help="only instances with annotator: pending (default: all)")
    args = ap.parse_args()

    if args.single:
        print(process(str(args.single), str(args.repos_root)), flush=True)
        return 0

    dirs = []
    for inst in sorted((DCBENCH / "instances").iterdir()):
        ann = yaml.safe_load((inst / "annotation.yaml").read_text())
        if args.pending_only and ann.get("annotator") != "pending":
            continue
        dirs.append(str(inst))
    if args.limit:
        dirs = dirs[: args.limit]
    print(f"generating candidates for {len(dirs)} instances")

    def run_isolated(inst_dir: str) -> str:
        cmd = [
            sys.executable,
            "-m",
            "benchmarks.dcbench.gen_candidates",
            "--repos-root",
            str(args.repos_root),
            "--single",
            inst_dir,
        ]
        try:
            r = subprocess.run(
                cmd, capture_output=True, text=True, timeout=args.per_instance_timeout, cwd=str(DCBENCH.parent.parent)
            )
        except subprocess.TimeoutExpired:
            return f"{Path(inst_dir).name}: FAIL timeout>{args.per_instance_timeout}s"
        out = (r.stdout or "").strip().splitlines()
        if r.returncode != 0:
            return f"{Path(inst_dir).name}: FAIL rc={r.returncode}"
        return out[-1] if out else f"{Path(inst_dir).name}: FAIL no output"

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for res in pool.map(run_isolated, dirs):
            print(res, flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
