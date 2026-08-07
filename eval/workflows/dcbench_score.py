"""Score a scoring mode against dcbench — real commits, human/LLM annotations.

The oracle YAML corpus is a regression suite, not an evaluation: its own README
says it does not reproduce scale effects, and its metric
`recall * (1 - forbidden_rate)` saturates on one wrong file (#65). Every fusion
verdict recorded so far (#125) reads a below-threshold *count* on that corpus,
which cannot separate a ranking loss from a metric artefact.

dcbench measures what fusion was opened for: `nontrivial` recall — gold files
that are NOT in the diff, i.e. context the tool had to retrieve rather than
retain. Trivial retention dominates a pooled recall number and hides the signal
(paper v2, "Trivial Retention vs Context Retrieval").

Budget is fixed rather than auto on purpose. Output tracks whatever budget it is
given near-exactly (#167), so an auto run compares selection policies at
different token spends and the recall column becomes unreadable.

CLI:
    python -m eval.workflows.dcbench_score --mode ego --out results/dcbench/ego
"""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import re
import subprocess
import time
from pathlib import Path

import yaml

PROJECT_ROOT = Path(__file__).resolve().parents[2]
INSTANCES = PROJECT_ROOT / "datasets/dcbench/v1/instances"
CLONES = PROJECT_ROOT / "test-repos"
NATIVE = PROJECT_ROOT / "target/release/diffctx"

_COMMIT_RE = re.compile(r"[0-9a-f]{7,40}")
_REPO_NAME_RE = re.compile(r"[A-Za-z0-9._-]{1,64}")


def load_instances() -> list[dict]:
    out = []
    for ann in sorted(INSTANCES.glob("*/annotation.yaml")):
        a = yaml.safe_load(ann.read_text())
        a["instance_id"] = ann.parent.name
        out.append(a)
    return out


def ensure_worktree(repo: str, root: Path) -> Path:
    if not _REPO_NAME_RE.fullmatch(repo):
        raise ValueError(f"dataset repo name is not a bare name: {repo!r}")
    wt = root / "wt" / repo
    if (wt / ".git").exists():
        return wt
    wt.parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", "-C", str(CLONES / repo), "worktree", "add", "--detach", "-f", str(wt), "HEAD"],
        check=True,
        capture_output=True,
    )
    return wt


def checkout(wt: Path, sha: str) -> bool:
    r = subprocess.run(
        ["git", "-C", str(wt), "checkout", "--force", "--detach", sha],
        capture_output=True,
        timeout=900,
    )
    return r.returncode == 0


def run_one(
    wt: Path,
    sha: str,
    mode: str,
    budget: int,
    timeout_s: int,
    extra_env: dict[str, str] | None = None,
    tau: float | None = None,
) -> dict:
    if not _COMMIT_RE.fullmatch(sha):
        return {"status": "bad_sha", "elapsed_s": 0.0}
    env = None
    if extra_env:
        import os

        env = dict(os.environ, **extra_env)
    started = time.monotonic()
    try:
        proc = subprocess.run(
            [
                str(NATIVE),
                str(wt),
                "--diff",
                f"{sha}^..{sha}",
                "-f",
                "json",
                "-q",
                "--scoring",
                mode,
                "--budget",
                str(budget),
                "--timeout",
                str(timeout_s),
            ]
            + (["--tau", str(tau)] if tau is not None else []),
            capture_output=True,
            text=True,
            timeout=timeout_s + 30,
            env=env,
        )
    except subprocess.TimeoutExpired:
        return {"status": "hang", "elapsed_s": round(time.monotonic() - started, 1)}
    elapsed = round(time.monotonic() - started, 1)

    if proc.returncode != 0 or not proc.stdout.strip():
        kind = "hang" if proc.returncode in (124, 143) else "no_output"
        return {"status": kind, "elapsed_s": elapsed, "rc": proc.returncode}

    try:
        doc = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {"status": "bad_json", "elapsed_s": elapsed}

    frags = doc.get("fragments") or []
    return {
        "status": "produced",
        "elapsed_s": elapsed,
        # Tokens of the rendered bodies, which is what a consumer pays. The
        # JSON envelope is excluded so the figure is comparable across formats.
        "content_tokens": token_count("\n".join(f.get("content") or "" for f in frags)),
        "selected_files": sorted({f["path"] for f in frags}),
        "n_frags": len(frags),
        "changed_frags": sum(1 for f in frags if f.get("role") == "changed"),
    }


def token_count(text: str) -> int:
    import tiktoken

    return len(tiktoken.get_encoding("o200k_base").encode(text))


def _hit(gold: set[str], selected: set[str]) -> int:
    # Gold paths are repo-relative; the renderer emits worktree-relative ones.
    # Exact equality alone would score every instance zero.
    return sum(1 for g in gold if any(s == g or s.endswith("/" + g) for s in selected))


def score(inst: dict, result: dict) -> dict:
    if result.get("status") != "produced":
        return {}
    selected = set(result["selected_files"])
    gold = inst.get("gold") or []

    all_g = {g["path"] for g in gold}
    nontrivial = {g["path"] for g in gold if not g.get("in_diff")}
    essential = {g["path"] for g in gold if g.get("tier") == "essential"}
    ess_nt = essential & nontrivial
    forbidden = {f["path"] for f in (inst.get("forbidden") or [])}

    req_hits = _hit(all_g, selected)
    forb_hits = _hit(forbidden, selected)
    labelled = req_hits + forb_hits

    def rate(g: set[str]) -> float | None:
        return round(_hit(g, selected) / len(g), 4) if g else None

    return {
        "recall_all": rate(all_g),
        "recall_nontrivial": rate(nontrivial),
        "recall_essential": rate(essential),
        "recall_essential_nontrivial": rate(ess_nt),
        "forbidden_rate": round(forb_hits / len(forbidden), 4) if forbidden else None,
        "precision_labelled": round(req_hits / labelled, 4) if labelled else None,
        "n_gold": len(all_g),
        "n_nontrivial": len(nontrivial),
        "n_forbidden": len(forbidden),
        "n_selected": len(selected),
    }


def run_repo(
    repo: str,
    insts: list[dict],
    out: Path,
    mode: str,
    budget: int,
    timeout_s: int,
    extra_env: dict[str, str] | None = None,
    tau: float | None = None,
) -> list[dict]:
    wt = ensure_worktree(repo, out)
    rows = []
    for inst in insts:
        if not checkout(wt, inst["commit"]):
            rows.append({"instance_id": inst["instance_id"], "repo": repo, "status": "checkout_failed"})
            continue
        r = run_one(wt, inst["commit"], mode, budget, timeout_s, extra_env, tau)
        row = {
            "instance_id": inst["instance_id"],
            "repo": repo,
            "mode": mode,
            "budget": budget,
            **r,
            **score(inst, r),
        }
        rows.append(row)
        with (out / f"{repo}.jsonl").open("a") as fh:
            fh.write(json.dumps(row) + "\n")
    return rows


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", required=True, choices=["ego", "ppr", "bm25", "rrf", "pit"])
    ap.add_argument("--out", required=True)
    ap.add_argument("--budget", type=int, default=8000)
    ap.add_argument("--timeout", type=int, default=120)
    ap.add_argument("--workers", type=int, default=5)
    ap.add_argument("--repo", action="append", help="restrict to these repos")
    ap.add_argument(
        "--env",
        action="append",
        default=[],
        metavar="KEY=VAL",
        help="extra environment for the binary (e.g. DIFFCTX_BM25_DISCOVERY_TOP_K=20); recorded per row",
    )
    ap.add_argument("--tau", type=float, default=None, help="override the stopping threshold (shipped default when omitted)")
    args = ap.parse_args()

    extra_env: dict[str, str] = {}
    for kv in args.env:
        key, val = kv.split("=", 1)
        extra_env[key] = val

    out = PROJECT_ROOT / args.out
    out.mkdir(parents=True, exist_ok=True)

    insts = load_instances()
    if args.repo:
        insts = [i for i in insts if i["repo"] in set(args.repo)]
    by_repo: dict[str, list[dict]] = {}
    for i in insts:
        by_repo.setdefault(i["repo"], []).append(i)

    # One worktree per repo, so a repo is the unit of parallelism: two workers
    # on the same repo would fight over the same checkout.
    done = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.workers) as ex:
        futs = {
            ex.submit(run_repo, repo, rows, out, args.mode, args.budget, args.timeout, extra_env, args.tau): repo
            for repo, rows in by_repo.items()
        }
        for fut in concurrent.futures.as_completed(futs):
            repo = futs[fut]
            rows = fut.result()
            done += len(rows)
            produced = sum(1 for r in rows if r.get("status") == "produced")
            print(f"[{args.mode}] {repo}: {produced}/{len(rows)} produced ({done}/{len(insts)} total)", flush=True)


if __name__ == "__main__":
    main()
