"""Agent Retrieval Bench edit2ripple: fetch the pinned release, rank with diffctx.

Two subcommands; scoring is a separate step because it must run in an
environment that holds the benchmark's own evaluator and nothing of ours
(`.github/scripts/arb_score.py`, run with an interpreter that has the pinned
evaluator installed).

    python -m eval arb-edit2ripple fetch --out DIR
    python -m eval arb-edit2ripple run --data-root DIR --out DIR [--limit N]

`run` materializes each sample's repository at its base commit, applies the
anchor diff as one commit, asks `diffctx locate` for the files that change
reaches, and writes one row per sample to `rankings.jsonl`. Gold never enters
this step.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

from eval.external.sharding import parse_shard, shard_items
from eval.harness.adapters.arb_edit2ripple import ArbEdit2RippleAdapter, load_pins, repair_anchor_diff

LOCATE_BUDGET = 8000
RELEASE = "v2_edit2ripple"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def fetch(out: Path) -> Path:
    """Download the pinned release anonymously, verify its hash, extract to `out/data`."""
    pins = load_pins()["dataset"]
    url = f"https://huggingface.co/datasets/{pins['hf_repo']}/resolve/{pins['revision']}/{pins['archive']}"
    out.mkdir(parents=True, exist_ok=True)
    archive = out / "release.tar.zst"
    if not archive.exists() or sha256_of(archive) != pins["archive_sha256"]:
        print(f"downloading {url}", flush=True)
        with urllib.request.urlopen(url, timeout=120) as resp, archive.open("wb") as sink:
            while block := resp.read(1 << 20):
                sink.write(block)
    actual = sha256_of(archive)
    if actual != pins["archive_sha256"]:
        raise SystemExit(f"archive hash {actual} does not match the pin {pins['archive_sha256']}")
    data = out / "data"
    data.mkdir(exist_ok=True)
    unzstd = subprocess.Popen(["zstd", "-dc", str(archive)], stdout=subprocess.PIPE)
    subprocess.run(["tar", "-x", "-C", str(data)], stdin=unzstd.stdout, check=True)
    if unzstd.wait() != 0:
        raise SystemExit("zstd failed to decompress the archive")
    samples = data / "benchmark" / RELEASE / "samples.jsonl"
    if sha256_of(samples) != pins["samples_sha256"]:
        raise SystemExit("samples.jsonl does not match the pinned hash")
    print(f"release extracted to {data}")
    return data


def ranking_from_locate(doc: dict, anchor_file: str) -> dict:
    """Files by their best fragment score, anchor dropped.

    `items` is the 8k selection, `overflow` the capped list of admitted
    candidates the budget left behind; both carry the relevance score the
    selection used, so the union is one ranking.
    """
    best: dict[str, float] = {}
    for entry in [*doc.get("items", []), *doc.get("overflow", [])]:
        path = entry["path"]
        best[path] = max(best.get(path, float("-inf")), float(entry.get("score") or 0.0))
    ranked = sorted(((p, s) for p, s in best.items() if p != anchor_file), key=lambda ps: (-ps[1], ps[0]))
    provenance = doc.get("provenance") or {}
    return {
        "ranked": [[p, s] for p, s in ranked],
        "selected_files": sorted({e["path"] for e in doc.get("items", []) if e["path"] != anchor_file}),
        "overflow_count": doc.get("overflow_count", 0),
        "engine_version": (provenance.get("engine") or {}).get("version"),
        "effective_config_hash": provenance.get("effective_config_hash"),
        "tau": (provenance.get("selection") or {}).get("tau"),
    }


def locate_ranking(repo_dir: Path, anchor_file: str, budget: int, timeout: int) -> dict:
    from diffctx._native.pipeline import build_locate

    raw = build_locate(repo_dir, "HEAD~1..HEAD", budget_tokens=budget, timeout=timeout)
    return ranking_from_locate(json.loads(raw), anchor_file)


def rank_instance(inst, worktrees: Path, budget: int, timeout: int) -> dict:
    from eval.harness.common import apply_gold_patch, ensure_repo, reset_to_parent, run_cmd

    row = {
        "sample_id": inst.extra["sample_id"],
        "repo": inst.repo,
        "base_commit": inst.base_commit,
        "anchor_file": inst.extra["anchor_file"],
        "seeding": inst.extra["seeding"],
        "seed_repair": inst.extra["seed_repair"],
    }
    started = time.time()
    repo_dir = ensure_repo(inst.extra["repo_url"], inst.repo, inst.base_commit, worktrees)
    if repo_dir is None:
        return {**row, "status": "clone_fail"}
    outcome = apply_gold_patch(repo_dir, inst.gold_patch)
    if not outcome:
        run_cmd(["git", "-C", str(repo_dir), "checkout", "--force", inst.base_commit], check=False, timeout=60)
        return {**row, "status": "apply_fail"}
    engine_started = time.perf_counter()
    try:
        ranking = locate_ranking(repo_dir, inst.extra["anchor_file"], budget, timeout)
        # engine_s is the engine call alone; elapsed_s also holds the clone, which is not the engine's cost
        engine_s = round(time.perf_counter() - engine_started, 3)
    except Exception as exc:  # the engine reports a deadline or a parse failure as an exception
        message = f"{type(exc).__name__}: {exc}"
        status = "timeout" if "timeout" in message.lower() or "deadline" in message.lower() else "diffctx_fail"
        return {**row, "status": status, "error": message[:300], "apply_mode": outcome.mode}
    finally:
        reset_to_parent(repo_dir)
    return {
        **row,
        "status": "ok",
        "apply_mode": outcome.mode,
        "elapsed_s": round(time.time() - started, 1),
        "engine_s": engine_s,
        **ranking,
    }


def _done_ids(path: Path) -> set[str]:
    if not path.exists():
        return set()
    return {json.loads(line)["sample_id"] for line in path.read_text().splitlines() if line.strip()}


def run(args: argparse.Namespace) -> int:
    from eval.harness.common import repos_dir

    pins = load_pins()["dataset"]
    samples = args.data_root / "benchmark" / RELEASE / "samples.jsonl"
    if sha256_of(samples) != pins["samples_sha256"]:
        print("samples.jsonl does not match the pinned hash", file=sys.stderr)
        return 2

    cache = args.repos_dir or repos_dir("ARB_REPOS_DIR")
    cache.mkdir(parents=True, exist_ok=True)
    # The bare-clone cache is read from this variable on every call.
    os.environ["CB_REPOS_DIR"] = str(cache)
    worktrees = cache / "worktrees" / f"w{os.getpid()}"
    worktrees.mkdir(parents=True, exist_ok=True)

    adapter = ArbEdit2RippleAdapter(samples, args.repo_url_template)
    instances = sorted(adapter.load(), key=lambda i: (i.repo, i.base_commit, i.instance_id))
    whole_release = not (args.sample_id or args.limit)
    if args.shard:
        index, total = parse_shard(args.shard)
        instances = shard_items(instances, lambda i: i.repo, total, index)
    if args.sample_id:
        wanted = set(args.sample_id)
        instances = [i for i in instances if i.extra["sample_id"] in wanted]
    if args.limit:
        instances = instances[: args.limit]

    args.out.mkdir(parents=True, exist_ok=True)
    rankings = args.out / "rankings.jsonl"
    done = _done_ids(rankings)
    print(f"{len(instances)} samples, {len(done)} already ranked", flush=True)
    with rankings.open("a", encoding="utf-8") as sink:
        if whole_release and (not args.shard or parse_shard(args.shard)[0] == 0):
            for sample_id in adapter.unseedable():
                if sample_id not in done:
                    sink.write(json.dumps({"sample_id": sample_id, "status": "unseedable"}, sort_keys=True) + "\n")
        for position, inst in enumerate(instances, start=1):
            if inst.extra["sample_id"] in done:
                continue
            row = rank_instance(inst, worktrees, args.budget, args.timeout)
            sink.write(json.dumps(row, sort_keys=True) + "\n")
            sink.flush()
            print(f"[{position}/{len(instances)}] {inst.repo} {row['sample_id']} {row['status']}", flush=True)
    return 0


def baseline_inputs(args: argparse.Namespace) -> int:
    """Two sample files for the benchmark's own baselines: as shipped, and diff-only.

    The diff-only query is what diffctx receives: the anchor file and its anchor
    diff as repaired for the seed, the intent text dropped. `ok_ids.json` names
    the samples diffctx ranked, so every comparison is over the same samples.
    """
    samples_path = args.data_root / "benchmark" / RELEASE / "samples.jsonl"
    rows = [json.loads(line) for line in samples_path.read_text(encoding="utf-8").splitlines() if line.strip()]
    ranked_ok: set[str] = set()
    for path in args.rankings:
        for line in path.read_text(encoding="utf-8").splitlines():
            row = json.loads(line) if line.strip() else {}
            if row.get("status") == "ok":
                ranked_ok.add(str(row["sample_id"]))
    args.out.mkdir(parents=True, exist_ok=True)
    diff_only = []
    for row in rows:
        repaired, _ = repair_anchor_diff(row["query"]["anchor_diff"])
        query = {"anchor_file": row["query"]["anchor_file"], "anchor_diff": repaired}
        diff_only.append({**row, "query": query})
    (args.out / "samples_full.jsonl").write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in rows), encoding="utf-8")
    (args.out / "samples_diffonly.jsonl").write_text(
        "".join(json.dumps(r, sort_keys=True) + "\n" for r in diff_only), encoding="utf-8"
    )
    (args.out / "ok_ids.json").write_text(json.dumps(sorted(ranked_ok)), encoding="utf-8")
    print(f"{len(rows)} samples, {len(ranked_ok)} ranked by diffctx")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    fetch_p = sub.add_parser("fetch", help="download and verify the pinned release")
    fetch_p.add_argument("--out", type=Path, required=True)

    run_p = sub.add_parser("run", help="rank every sample with diffctx locate")
    run_p.add_argument("--data-root", type=Path, required=True, help="the `data` directory `fetch` produced")
    run_p.add_argument("--out", type=Path, required=True)
    run_p.add_argument(
        "--repos-dir", type=Path, default=None, help="bare-clone cache (default $ARB_REPOS_DIR or ~/.cache/contextbench_repos)"
    )
    run_p.add_argument("--shard", default="", help="I/N: this run's share, whole repositories per shard")
    run_p.add_argument("--sample-id", action="append", default=[])
    run_p.add_argument("--limit", type=int, default=0)
    run_p.add_argument("--budget", type=int, default=LOCATE_BUDGET)
    run_p.add_argument("--timeout", type=int, default=600, help="per-sample engine deadline in seconds")
    run_p.add_argument("--repo-url-template", default="https://github.com/{repo}.git")

    inputs_p = sub.add_parser(
        "baseline-inputs", help="write the as-shipped and diff-only sample files for the benchmark's baselines"
    )
    inputs_p.add_argument("--data-root", type=Path, required=True)
    inputs_p.add_argument("--rankings", type=Path, nargs="+", required=True)
    inputs_p.add_argument("--out", type=Path, required=True)

    args = parser.parse_args()
    if args.command == "baseline-inputs":
        return baseline_inputs(args)
    if args.command == "fetch":
        fetch(args.out)
        return 0
    return run(args)


if __name__ == "__main__":
    raise SystemExit(main())
