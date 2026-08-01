"""Re-run the 109-commit real-world benchmark against the current build.

The shipped snapshot in `datasets/real-world-diff/v1/` was taken on 1.10.2 under
a 30-second wall-clock cap and reads: 72 hang, 34 over_dump, 3 ok, mean
precision 0.176 on the cases that produced output. Two thirds of it is a
liveness result, not a quality one, so the quality numbers describe the 37 cases
that survived rather than the benchmark.

This re-runs the same 109 commits so #149's gate ("over-dump rate < 10%,
precision >= 0.35") and #121's timeout class can be read off the same set.

Elapsed is recorded per case, so the run stays comparable to the 30s snapshot
without inheriting its cap: a case that finishes in 40s is a hang at 30s and a
success at 180s, and only the recorded seconds can tell those apart afterwards.

Repositories are checked out in detached worktrees under the output directory,
so the shared clones in `test-repos/` are never moved.

CLI:
    python -m eval.workflows.realworld_rerun --out results/realworld_<sha>
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import time
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[2]
BENCH = PROJECT_ROOT / "datasets/real-world-diff/v1/diffctx_realworld_bench.jsonl"
CLONES = PROJECT_ROOT / "test-repos"

# The snapshot's own threshold for `over_dump`. Kept as-is so the two runs are
# read on the same scale; the dataset's concern logs argue it over-fires on
# legitimately large diffs, which is a finding to carry, not to silently retune
# mid-comparison.
OVER_DUMP_TOKENS = 15_000


def load_cases() -> list[dict]:
    return [json.loads(line) for line in BENCH.read_text().splitlines() if line.strip()]


def ensure_worktree(repo: str, root: Path) -> Path:
    """A detached worktree per repo, reused across that repo's commits."""
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
        timeout=600,
    )
    return r.returncode == 0


def run_case(wt: Path, sha: str, timeout_s: int, binary: str, budget: int = 0) -> dict:
    """One diffctx run over `<sha>^..<sha>`, timed.

    Markdown, not JSON: `md_tokens` is what the snapshot recorded and what the
    over-dump threshold is expressed in, so counting the rendered output
    directly keeps the two runs on one scale. It also halves wall clock — a
    second invocation purely to obtain the token count would double a run whose
    dominant cost is already the 180s ceiling.
    """
    started = time.monotonic()
    try:
        proc = subprocess.run(
            [binary, str(wt), "--diff", f"{sha}^..{sha}", "-f", "md", "-q", "--timeout", str(timeout_s)]
            + (["--budget", str(budget)] if budget else []),
            capture_output=True,
            text=True,
            timeout=timeout_s + 30,
        )
    except subprocess.TimeoutExpired:
        return {"status": "hang", "elapsed_s": round(time.monotonic() - started, 1)}
    elapsed = round(time.monotonic() - started, 1)

    if proc.returncode != 0 or not proc.stdout.strip():
        # Exit 124 is diffctx's own deadline; anything else with no output is
        # recorded distinctly so a crash is not filed as a timeout.
        kind = "hang" if proc.returncode in (124, 143) else "no_output"
        return {"status": kind, "elapsed_s": elapsed, "rc": proc.returncode}

    md = proc.stdout
    # `## \`path:lines\` ...` is the fragment heading; a trailing `**changed**`
    # marks the ones carrying the diff.
    heads = re.findall(r"^## `([^`:]+):[^`]*`(.*)$", md, re.M)
    return {
        "status": "produced",
        "elapsed_s": elapsed,
        "md_tokens": token_count(md),
        "ctx_files": len({h[0] for h in heads}),
        "n_frags": len(heads),
        "changed_frags": sum(1 for h in heads if "**changed**" in h[1]),
        "selected_files": sorted({h[0] for h in heads}),
    }


def token_count(text: str) -> int:
    import tiktoken

    return len(tiktoken.get_encoding("o200k_base").encode(text))


def score(case: dict, result: dict) -> dict:
    """Precision and recall against the hand-labelled include/exclude sets.

    Both are file-level and both are *partial* labels: `gold_include` lists what
    a correct tool must surface, `gold_exclude` what it must not. Precision here
    is therefore over the labelled files only — an unlabelled selected file is
    neither credited nor penalised, which is what the snapshot's 0.176 also
    measured.
    """
    if result.get("status") != "produced":
        return {}
    selected = set(result["selected_files"])
    inc = set(case.get("gold_include") or [])
    exc = set(case.get("gold_exclude") or [])

    def hit(gold: set[str]) -> int:
        """Gold entries surfaced. Suffix-matched, because the labels are written
        repo-relative while the renderer emits paths relative to the worktree —
        exact equality alone would score every case zero."""
        return sum(1 for g in gold if any(s == g or s.endswith("/" + g) for s in selected))

    required_hits = hit(inc)
    forbidden_hits = hit(exc)
    recall = required_hits / len(inc) if inc else None
    forbidden = forbidden_hits / len(exc) if exc else 0.0
    labelled_total = required_hits + forbidden_hits
    precision = required_hits / labelled_total if labelled_total else None
    return {
        "recall": None if recall is None else round(recall, 3),
        "forbidden_rate": round(forbidden, 3),
        "precision_labelled": None if precision is None else round(precision, 3),
    }


def classify(result: dict, md_tokens: int | None) -> str:
    if result.get("status") != "produced":
        return result.get("status", "unknown")
    if md_tokens is not None and md_tokens > OVER_DUMP_TOKENS:
        return "over_dump"
    return "ok"


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--timeout", type=int, default=180)
    # The Python CLI, not the native binary: markdown is a Python-side format
    # (the native  accepts yaml|json only), and  is the unit the
    # snapshot and the over-dump threshold are both expressed in.
    ap.add_argument("--binary", default=str(PROJECT_ROOT / ".venv/bin/diffctx"))
    ap.add_argument("--limit", type=int, default=0)
    # One process per repo. Each repo has its own worktree, so the three never
    # contend on a checkout, and the wall clock collapses from the sum of the
    # repos to the slowest one — which matters when most cases sit at the
    # timeout ceiling rather than finishing.
    ap.add_argument("--repo", default="")
    # Explicit budget instead of auto. Output tracks the budget almost exactly
    # (8000 -> 8823 md tokens, 48000 -> 48117 on a react-native case), and auto
    # saturates at `auto_max` = 48000 on these diffs — three times the
    # benchmark's own over-dump threshold. Whether the extra 40k buys any recall
    # is #167, and this flag is how it gets measured.
    ap.add_argument("--budget", type=int, default=0)
    args = ap.parse_args(argv)

    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    sink = out / "results.jsonl"
    done = set()
    if sink.exists():
        done = {json.loads(line)["commit"] for line in sink.read_text().splitlines() if line.strip()}
        print(f"resuming: {len(done)} cases already recorded")

    cases = load_cases()
    if args.repo:
        cases = [c for c in cases if c["repo"] == args.repo]
    if args.limit:
        cases = cases[: args.limit]

    with sink.open("a") as fh:
        for i, case in enumerate(cases, 1):
            if case["commit"] in done:
                continue
            wt = ensure_worktree(case["repo"], out)
            if not checkout(wt, case["sha"]):
                row = {"commit": case["commit"], "repo": case["repo"], "status": "checkout_fail"}
                fh.write(json.dumps(row) + "\n")
                fh.flush()
                continue

            result = run_case(wt, case["sha"], args.timeout, args.binary, args.budget)
            md = result.get("md_tokens")
            row = {
                "commit": case["commit"],
                "repo": case["repo"],
                "sha": case["sha"],
                "baseline_status": case.get("status"),
                "baseline_md_tokens": case.get("md_tokens"),
                "baseline_ctx_files": case.get("ctx_files"),
                "md_tokens": md,
                "new_status": classify(result, md),
                **result,
                **score(case, result),
            }
            row.pop("selected_files", None)
            fh.write(json.dumps(row) + "\n")
            fh.flush()
            print(
                f"[{i}/{len(cases)}] {case['commit']:<28} {row['new_status']:<12} " f"{result.get('elapsed_s')}s  tokens={md}",
                flush=True,
            )
    print(f"\nwrote {sink}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
