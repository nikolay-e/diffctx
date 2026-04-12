#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from pathlib import Path

LINES_RE = re.compile(r"^(\d+)-(\d+)$")
_WORKSPACE_PREFIX_RE = re.compile(r"^/workspace/[^/]+/")


def normalize_gold_path(path: str) -> str:
    return _WORKSPACE_PREFIX_RE.sub("", path)


REPOS_DIR = Path(os.environ.get("CB_REPOS_DIR", str(Path.home() / ".cache" / "contextbench_repos")))
REPOS_DIR.mkdir(parents=True, exist_ok=True)

RECOGNIZED_EXT = {
    ".py",
    ".pyi",
    ".js",
    ".jsx",
    ".ts",
    ".tsx",
    ".go",
    ".rs",
    ".java",
    ".c",
    ".h",
    ".cpp",
    ".cc",
    ".hpp",
    ".cs",
    ".rb",
    ".php",
    ".swift",
    ".kt",
    ".scala",
    ".lua",
    ".dart",
    ".ex",
    ".exs",
    ".erl",
    ".hrl",
    ".clj",
    ".hs",
    ".ml",
    ".mli",
    ".r",
    ".jl",
    ".nim",
    ".zig",
    ".pl",
    ".pm",
    ".sh",
    ".bash",
    ".zsh",
    ".sql",
    ".html",
    ".css",
    ".scss",
    ".less",
    ".vue",
    ".svelte",
    ".md",
    ".rst",
    ".txt",
    ".json",
    ".yaml",
    ".yml",
    ".toml",
    ".xml",
    ".dockerfile",
    ".tf",
    ".hcl",
}


def patch_files(patch: str) -> tuple[set[str], set[str], set[str]]:
    added, deleted, modified = set(), set(), set()
    cur_a = cur_b = None
    for line in patch.splitlines():
        if line.startswith("diff --git "):
            cur_a = cur_b = None
        elif line.startswith("--- "):
            p = line[4:]
            cur_a = None if p == "/dev/null" else (p[2:] if p.startswith("a/") else p)
        elif line.startswith("+++ "):
            p = line[4:]
            cur_b = None if p == "/dev/null" else (p[2:] if p.startswith("b/") else p)
            if cur_a is None and cur_b is not None:
                added.add(cur_b)
            elif cur_a is not None and cur_b is None:
                deleted.add(cur_a)
            elif cur_a == cur_b and cur_a is not None:
                modified.add(cur_a)
            elif cur_a is not None and cur_b is not None:
                modified.add(cur_b)
                modified.add(cur_a)
    return added, deleted, modified


def run_cmd(cmd, cwd=None, check=True, timeout=120):
    return subprocess.run(cmd, cwd=cwd, text=True, capture_output=True, check=check, timeout=timeout)


def ensure_repo(repo_url: str, repo_name: str, base_commit: str) -> Path | None:
    repo_dir = REPOS_DIR / repo_name.replace("/", "__")
    if not repo_dir.exists():
        r = run_cmd(["git", "clone", "--quiet", repo_url, str(repo_dir)], check=False, timeout=600)
        if r.returncode != 0:
            print(f"  CLONE FAIL: {r.stderr[:200]}")
            return None
    r = run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", base_commit], check=False)
    if r.returncode != 0:
        run_cmd(["git", "-C", str(repo_dir), "fetch", "--all", "--quiet"], check=False, timeout=600)
        r = run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", base_commit], check=False)
        if r.returncode != 0:
            print(f"  CHECKOUT FAIL: {r.stderr[:200]}")
            return None
    return repo_dir


def apply_as_commit(repo_dir: Path, patch_text: str) -> bool:
    with tempfile.NamedTemporaryFile(mode="w", suffix=".patch", delete=False) as f:
        f.write(patch_text)
        patch_path = f.name
    try:
        r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", patch_path], check=False)
        if r.returncode != 0:
            r = run_cmd(["git", "-C", str(repo_dir), "apply", "--index", "--3way", patch_path], check=False)
            if r.returncode != 0:
                print(f"  APPLY FAIL: {r.stderr[:300]}")
                return False
        run_cmd(["git", "-C", str(repo_dir), "commit", "-m", "bench", "--allow-empty", "--no-verify"], check=False)
        return True
    finally:
        os.unlink(patch_path)


DUMP_DIR = Path(tempfile.gettempdir()) / "diffctx_dump"
SCORES_FILE = Path(tempfile.gettempdir()) / "diffctx_scores.jsonl"


def run_diffctx(repo_dir: Path, budget: int):
    DUMP_DIR.mkdir(exist_ok=True)
    for f in DUMP_DIR.iterdir():
        f.unlink()
    if SCORES_FILE.exists():
        SCORES_FILE.unlink()
    env = {**os.environ, "DIFFCTX_DUMP_DIR": str(DUMP_DIR), "DIFFCTX_DUMP_SCORES": str(SCORES_FILE)}
    r = subprocess.run(
        [
            sys.executable,
            "-m",
            "treemapper",
            str(repo_dir),
            "--diff",
            "HEAD~1..HEAD",
            "--budget",
            str(budget),
            "--format",
            "json",
            "-q",
            "-o",
            "-",
        ],
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=300,
    )
    if r.returncode != 0:
        return None, f"rc={r.returncode}: {r.stderr[:200]}"
    try:
        return json.loads(r.stdout), None
    except json.JSONDecodeError as e:
        return None, f"json: {e}"


def read_dump_set(name: str) -> set[str]:
    p = DUMP_DIR / name
    if not p.exists():
        return set()
    return {line.strip() for line in p.read_text().splitlines() if line.strip()}


def read_scores_for_files(target_files: set[str]) -> dict[str, list[dict]]:
    result: dict[str, list[dict]] = {f: [] for f in target_files}
    if not SCORES_FILE.exists():
        return result
    for line in SCORES_FILE.read_text().splitlines():
        if not line.strip():
            continue
        entry = json.loads(line)
        if entry["path"] in target_files:
            result[entry["path"]].append(entry)
    return result


def diagnose_missing_file(repo_dir: Path, file_path: str, was_deleted: bool):
    if was_deleted:
        return "DELETED_BY_PATCH (file no longer exists in HEAD)"
    abs_path = repo_dir / file_path
    if not abs_path.exists():
        return f"NOT_IN_REPO at {abs_path}"
    ext = abs_path.suffix.lower()
    if ext not in RECOGNIZED_EXT:
        return f"UNRECOGNIZED_EXT {ext!r} (treemapper would skip)"
    try:
        size = abs_path.stat().st_size
    except OSError as e:
        return f"STAT_ERR {e}"
    if size == 0:
        return "EMPTY_FILE"
    if size > 1_000_000:
        return f"HUGE_FILE ({size} bytes — likely skipped)"
    r = run_cmd(["git", "-C", str(repo_dir), "ls-files", "--error-unmatch", file_path], check=False)
    if r.returncode != 0:
        return "NOT_GIT_TRACKED (gitignored or untracked)"
    return f"PRESENT_BUT_NOT_SELECTED ({size} bytes, {ext}) — diffctx didn't pick it"


def _print_patch_coverage(p_set: set[str], selected: set[str], deleted: set[str], repo_dir: Path) -> None:
    patch_in = p_set & selected
    patch_lost = p_set - selected
    print("\n[PATCH FILE COVERAGE]")
    print(f"  in_patch:           {len(p_set)}")
    print(f"  diffctx returned:   {len(patch_in)}  ({100 * len(patch_in) / max(1, len(p_set)):.0f}%)")
    print(f"  LOST from patch:    {len(patch_lost)}")
    if patch_lost:
        print("  WHY EACH PATCH FILE WAS LOST:")
        for f in sorted(patch_lost)[:10]:
            reason = diagnose_missing_file(repo_dir, f, was_deleted=(f in deleted))
            print(f"    {f}\n        -> {reason}")


def _print_nontrivial_report(
    nontrivial: set[str],
    nontrivial_hits: set[str],
    nontrivial_missed: set[str],
    repo_dir: Path,
    fragmented: set[str],
    universe: set[str],
    sel_dump: set[str],
) -> None:
    print("\n[NONTRIVIAL COVERAGE]")
    print(f"  nontrivial gold:    {len(nontrivial)}")
    print(f"  diffctx found:      {len(nontrivial_hits)}  ({100 * len(nontrivial_hits) / max(1, len(nontrivial)):.0f}%)")
    scores_by_file = read_scores_for_files(nontrivial)

    for f in sorted(nontrivial_missed)[:10]:
        stage = _classify_failure_stage(f, sel_dump, fragmented, universe)
        file_scores = scores_by_file.get(f, [])
        max_score = max((s["ppr_score"] for s in file_scores), default=0.0)
        statuses = {s["status"] for s in file_scores}
        print(f"    {f}")
        print(f"        stage:     {stage}")
        print(f"        max_ppr:   {max_score:.6f}  ({len(file_scores)} fragments)")
        if statuses:
            print(f"        statuses:  {statuses}")

    for f in sorted(nontrivial_hits):
        file_scores = scores_by_file.get(f, [])
        max_score = max((s["ppr_score"] for s in file_scores), default=0.0)
        print(f"    HIT: {f}  max_ppr={max_score:.6f}")


def _classify_failure_stage(f: str, sel_dump: set[str], fragmented: set[str], universe: set[str]) -> str:
    if f in sel_dump:
        return "SELECTED_BUT_PATH_MISMATCH"
    if f in fragmented:
        return "FRAGMENTED_BUT_NOT_SELECTED"
    if f in universe:
        return "IN_UNIVERSE_BUT_NOT_FRAGMENTED (max_fragments cap?)"
    return "NOT_IN_UNIVERSE (edge discovery missed it)"


def evaluate_one(inst: dict, budget: int) -> dict:
    iid = inst["instance_id"]
    print("\n" + "=" * 78)
    print(f"INSTANCE: {iid}")
    print(f"Repo: {inst['repo']}  Lang: {inst['language']}")
    print(f"Base: {inst['base_commit'][:12]}")

    gold_blocks = json.loads(inst["gold_context"]) if isinstance(inst["gold_context"], str) else inst["gold_context"]
    for g in gold_blocks:
        g["file"] = normalize_gold_path(g["file"])
    gold_set = {g["file"] for g in gold_blocks}
    added, deleted, modified = patch_files(inst["patch"])
    p_set = added | deleted | modified
    nontrivial = gold_set - p_set

    print(f"\n[GOLD]      {len(gold_set):3d} files, {len(gold_blocks):3d} blocks")
    for f in sorted(gold_set)[:8]:
        marker = " (in patch)" if f in p_set else " (NONTRIVIAL)"
        print(f"  {f}{marker}")
    if len(gold_set) > 8:
        print(f"  ... and {len(gold_set) - 8} more")

    print(f"\n[PATCH]     {len(p_set):3d} files (added={len(added)}, deleted={len(deleted)}, modified={len(modified)})")
    if deleted:
        print(f"  DELETED FILES: {sorted(deleted)}")

    print(f"\n[NONTRIVIAL GOLD] {len(nontrivial):3d} files")

    repo_dir = ensure_repo(inst["repo_url"], inst["repo"], inst["base_commit"])
    if not repo_dir:
        return {"id": iid, "status": "clone_fail"}

    if not apply_as_commit(repo_dir, inst["patch"]):
        run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", inst["base_commit"]], check=False)
        return {"id": iid, "status": "apply_fail"}

    t0 = time.time()
    output, err = run_diffctx(repo_dir, budget)
    elapsed = time.time() - t0

    if not output:
        run_cmd(["git", "-C", str(repo_dir), "reset", "--hard", "HEAD~1"], check=False)
        print(f"  DIFFCTX FAIL: {err}")
        return {"id": iid, "status": "diffctx_fail"}

    selected = {f["path"] for f in output["fragments"]}
    print(f"\n[DIFFCTX]   {len(selected):3d} files, {output['fragment_count']:3d} fragments, {elapsed:.1f}s")

    _print_patch_coverage(p_set, selected, deleted, repo_dir)

    nontrivial_hits = nontrivial & selected
    nontrivial_missed = nontrivial - selected
    universe = read_dump_set("universe.txt")
    fragmented = read_dump_set("fragmented.txt")
    sel_dump = read_dump_set("selected.txt")
    candidates_info = (DUMP_DIR / "candidates.txt").read_text().strip() if (DUMP_DIR / "candidates.txt").exists() else ""

    print(f"\n[PIPELINE STAGES]\n  {candidates_info}")
    print(f"  universe: {len(universe)}  fragmented: {len(fragmented)}  selected: {len(sel_dump)}")

    _print_nontrivial_report(nontrivial, nontrivial_hits, nontrivial_missed, repo_dir, fragmented, universe, sel_dump)

    extra = selected - gold_set
    if extra:
        print(f"\n[DIFFCTX EXTRA]  {len(extra)} files not in gold")
        for f in sorted(extra)[:5]:
            print(f"    {f} ({'patch' if f in p_set else 'discovered'})")

    run_cmd(["git", "-C", str(repo_dir), "reset", "--hard", "HEAD~1"], check=False)

    file_recall = len(gold_set & selected) / len(gold_set)
    nt_recall = len(nontrivial_hits) / len(nontrivial) if nontrivial else 0.0
    patch_coverage = len(p_set & selected) / len(p_set) if p_set else 0.0

    stage_per_file: dict[str, str] = {}
    for f in nontrivial:
        if f in selected:
            stage_per_file[f] = "selected"
        else:
            stage_per_file[f] = _classify_failure_stage(f, sel_dump, fragmented, universe)

    return {
        "id": iid,
        "status": "ok",
        "language": inst["language"],
        "n_gold": len(gold_set),
        "n_patch": len(p_set),
        "n_nontrivial": len(nontrivial),
        "n_deleted_in_patch": len(deleted),
        "patch_coverage": round(patch_coverage, 3),
        "file_recall": round(file_recall, 3),
        "nt_recall": round(nt_recall, 3),
        "stage_per_file": stage_per_file,
    }


def _print_threshold_sanity_check():
    v = subprocess.run(
        [
            sys.executable,
            "-c",
            "from treemapper.diffctx.filtering import _LOW_RELEVANCE_THRESHOLD; print(_LOW_RELEVANCE_THRESHOLD)",
        ],
        capture_output=True,
        text=True,
    ).stdout.strip()
    print(f"diffctx _LOW_RELEVANCE_THRESHOLD = {v}", file=sys.stderr)


def main():
    _print_threshold_sanity_check()

    ap = argparse.ArgumentParser()
    ap.add_argument("--limit", type=int, default=5)
    ap.add_argument("--budget", type=int, default=8000)
    ap.add_argument("--nontrivial-only", action="store_true", default=True)
    args = ap.parse_args()

    from datasets import load_dataset

    ds = load_dataset("Contextbench/ContextBench", "contextbench_verified", split="train")
    insts = list(ds)
    if args.nontrivial_only:
        kept = []
        for i in insts:
            gb = json.loads(i["gold_context"]) if isinstance(i["gold_context"], str) else i["gold_context"]
            gold = {normalize_gold_path(g["file"]) for g in gb}
            added, deleted, modified = patch_files(i["patch"])
            if gold - (added | deleted | modified):
                kept.append(i)
        insts = kept
    insts = insts[: args.limit]

    print(f"Diagnosing {len(insts)} nontrivial instances at budget={args.budget}\n")
    results = []
    for inst in insts:
        try:
            r = evaluate_one(inst, args.budget)
            results.append(r)
        except Exception as e:
            print(f"  ERROR: {type(e).__name__}: {e}")

    print("\n" + "=" * 78)
    print("SUMMARY")
    print("=" * 78)
    ok = [r for r in results if r["status"] == "ok"]
    fail = [r for r in results if r["status"] != "ok"]
    print(f"Total: {len(results)}  ok: {len(ok)}  fail: {len(fail)}")
    for r in fail:
        print(f"  FAIL [{r['status']}]: {r['id']}")
    if ok:
        print(f"\nAvg patch_coverage: {sum(r['patch_coverage'] for r in ok)/len(ok):.3f}")
        print(f"Avg file_recall:    {sum(r['file_recall'] for r in ok)/len(ok):.3f}")
        print(f"Avg nontrivial:     {sum(r['nt_recall'] for r in ok)/len(ok):.3f}")
        total_deleted = sum(r["n_deleted_in_patch"] for r in ok)
        print(f"Total deleted files across all instances: {total_deleted}")

        stages: dict[str, int] = {}
        total_nt = 0
        for r in ok:
            for _f, stage in r.get("stage_per_file", {}).items():
                stages[stage] = stages.get(stage, 0) + 1
                total_nt += 1
        if total_nt:
            print(f"\nStage-wise breakdown ({total_nt} nontrivial gold files):")
            for stage in sorted(stages, key=lambda s: -stages[s]):
                pct = 100 * stages[stage] / total_nt
                print(f"  {stage:50s}: {stages[stage]:4d} ({pct:5.1f}%)")

        print("\nIf patch_coverage < 0.95: BUG — diffctx is losing files from its own diff input.")
        print("If patch_coverage > 0.95: not a patch-loss bug, look elsewhere.")


if __name__ == "__main__":
    main()
