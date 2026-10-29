#!/usr/bin/env python3
"""CfgNet-500: does diffctx's file-level graph link the files of a labelled config dependency?

    python -m eval cfgnet500 run --out DIR [--projects piggymetrics,pig] [--csv P] [--workdir D]
    python -m eval cfgnet500 summarize --rows DIR/rows.json

The 500 candidates are two options in two files of ten Spring Boot projects at
the versions the paper lists (`datasets/xlang/cfgnet500_pins.json`), each
labelled true or false by hand (199 true, 301 false: plain value equality is
40% precise on them). The labels live in a repository with no licence, so the
CSV is fetched at its pinned commit when the run starts, checked against the
pinned digest, and never committed; rows written here carry file paths, the
technology pair and the label, not option names or values.

A candidate is *decided* when its two files are distinct nodes of the project's
file-level graph. It is predicted positive when an edge of category `config`,
`config_generic` or `key_match` joins them in either direction. Same-file
candidates cannot be decided at file level and are counted, not scored.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import subprocess
import sys
import urllib.request
from collections import defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

PINS = Path(__file__).resolve().parents[2] / "datasets" / "xlang" / "cfgnet500_pins.json"
PREDICT_CATEGORIES = frozenset({"config", "config_generic", "key_match"})
CSV_URL = "https://raw.githubusercontent.com/{repo}/{commit}/{path}"
FETCH_TIMEOUT_S = 120


@dataclass(frozen=True)
class Candidate:
    project: str
    index: int
    file_a: str
    file_b: str
    tech_a: str
    tech_b: str
    label: bool

    @property
    def tech_pair(self) -> str:
        return "+".join(sorted({self.tech_a, self.tech_b}))


def load_pins() -> dict[str, Any]:
    return json.loads(PINS.read_text())


def fetch_csv(dest_dir: Path, pins: dict[str, Any]) -> Path:
    """The labels at their pinned commit, verified against the pinned digest."""
    src = pins["labels_source"]
    dest = dest_dir / "all_dependencies.csv"
    if not dest.exists():
        dest_dir.mkdir(parents=True, exist_ok=True)
        url = CSV_URL.format(repo=src["repo"], commit=src["commit"], path=src["path"])
        with urllib.request.urlopen(url, timeout=FETCH_TIMEOUT_S) as resp:
            dest.write_bytes(resp.read())
    digest = hashlib.sha256(dest.read_bytes()).hexdigest()
    if digest != src["sha256"]:
        raise RuntimeError(f"label file digest {digest} differs from the pinned {src['sha256']}")
    return dest


def load_candidates(csv_path: Path, pins: dict[str, Any]) -> list[Candidate]:
    with csv_path.open(newline="", encoding="utf-8") as f:
        rows = list(csv.DictReader(f))
    src = pins["labels_source"]
    out = [
        Candidate(
            project=r["project"],
            index=int(r["index"]),
            file_a=r["option_file"],
            file_b=r["dependent_option_file"],
            tech_a=r["option_technology"],
            tech_b=r["dependent_option_technology"],
            label=r["final_rating"].strip().upper() == "TRUE",
        )
        for r in rows
    ]
    true_rows = sum(c.label for c in out)
    if (len(out), true_rows) != (src["rows"], src["true_rows"]):
        raise RuntimeError(f"labels read as {len(out)} rows / {true_rows} true, pinned {src['rows']} / {src['true_rows']}")
    unknown = {c.project for c in out} - set(pins["projects"])
    if unknown:
        raise RuntimeError(f"projects without a pinned version: {sorted(unknown)}")
    return out


def _git(args: list[str], cwd: Path | None = None, timeout: int = 1800) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True, timeout=timeout)


def checkout_project(workdir: Path, project: str, pins: dict[str, Any]) -> Path:
    """Clone with history but no blobs, then check out the pinned version."""
    spec = pins["projects"][project]
    dest = workdir / project
    if not dest.exists():
        workdir.mkdir(parents=True, exist_ok=True)
        _git(["clone", "--quiet", "--filter=blob:none", "--no-checkout", f"https://github.com/{spec['repo']}.git", str(dest)])
    # A pinned commit can be reachable only through a pull-request ref a clone does not fetch;
    # GitHub serves it by its full hash, which the pin carries when that is so.
    present = subprocess.run(["git", "cat-file", "-e", f"{spec['sha']}^{{commit}}"], cwd=dest, capture_output=True)
    if present.returncode != 0:
        _git(["fetch", "--quiet", "--filter=blob:none", "origin", spec.get("full_sha", spec["sha"])], cwd=dest)
    _git(["checkout", "--quiet", "--force", spec["sha"]], cwd=dest)
    return dest


def file_links(repo_dir: Path) -> tuple[set[str], dict[frozenset[str], set[str]]]:
    """File-level nodes and, per unordered file pair, the categories of the edges joining them."""
    from diffctx._native.graph_export import graph_to_json_string
    from diffctx._native.project_graph import build_project_graph

    document = json.loads(graph_to_json_string(build_project_graph(repo_dir), "file"))
    nodes = {n["path"] for n in document["nodes"]}
    links: dict[frozenset[str], set[str]] = defaultdict(set)
    for e in document["edges"]:
        if e["source"] != e["target"]:
            links[frozenset((e["source"], e["target"]))].add(e["category"])
    return nodes, links


def judge(candidates: list[Candidate], nodes: set[str], links: dict[frozenset[str], set[str]]) -> list[dict[str, Any]]:
    rows = []
    for c in candidates:
        row: dict[str, Any] = {
            "project": c.project,
            "index": c.index,
            "files": [c.file_a, c.file_b],
            "tech_pair": c.tech_pair,
            "label": c.label,
            "predicted": False,
            "linked_any_category": False,
        }
        if c.file_a == c.file_b:
            row["status"] = "same_file"
        elif c.file_a not in nodes or c.file_b not in nodes:
            row["status"] = "file_not_in_graph"
        else:
            categories = links.get(frozenset((c.file_a, c.file_b)), set())
            row["status"] = "decided"
            row["link_categories"] = sorted(categories)
            row["predicted"] = bool(categories & PREDICT_CATEGORIES)
            row["linked_any_category"] = bool(categories)
        rows.append(row)
    return rows


def wilson(successes: int, n: int, z: float = 1.96) -> list[float] | None:
    if n == 0:
        return None
    p = successes / n
    denom = 1 + z * z / n
    centre = (p + z * z / (2 * n)) / denom
    half = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / denom
    return [round(max(0.0, centre - half), 4), round(min(1.0, centre + half), 4)]


def _ratio(num: int, den: int) -> float | None:
    return round(num / den, 4) if den else None


def _score(rows: list[dict[str, Any]], predicted_key: str) -> dict[str, Any]:
    decided = [r for r in rows if r["status"] == "decided"]
    true_all = sum(1 for r in rows if r["label"])
    tp = sum(1 for r in decided if r[predicted_key] and r["label"])
    fp = sum(1 for r in decided if r[predicted_key] and not r["label"])
    true_decided = sum(1 for r in decided if r["label"])
    return {
        "candidates": len(rows),
        "true": true_all,
        "decided": len(decided),
        "coverage": _ratio(len(decided), len(rows)),
        "predicted_positive": tp + fp,
        "tp": tp,
        "fp": fp,
        "precision": _ratio(tp, tp + fp),
        "precision_wilson95": wilson(tp, tp + fp),
        "recall_of_all_true": _ratio(tp, true_all),
        "recall_of_decided_true": _ratio(tp, true_decided),
    }


def summarize(rows: list[dict[str, Any]]) -> dict[str, Any]:
    by_status: dict[str, int] = defaultdict(int)
    for r in rows:
        by_status[r["status"]] += 1
    same_file = [r for r in rows if r["status"] == "same_file"]
    out: dict[str, Any] = {
        "protocol": "decided = two distinct file nodes; predicted positive = config/config_generic/key_match edge between them",
        "status": dict(sorted(by_status.items())),
        "same_file_true": sum(1 for r in same_file if r["label"]),
        "overall": _score(rows, "predicted"),
        "any_edge_category": _score(rows, "linked_any_category"),
    }
    for key in ("tech_pair", "project"):
        groups: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for r in rows:
            groups[r[key]].append(r)
        out[f"by_{key}"] = {k: _score(v, "predicted") for k, v in sorted(groups.items())}
    return out


def cmd_run(args: argparse.Namespace) -> int:
    pins = load_pins()
    workdir = Path(args.workdir)
    csv_path = Path(args.csv) if args.csv else fetch_csv(workdir / "labels", pins)
    candidates = load_candidates(csv_path, pins)
    wanted = args.projects.split(",") if args.projects else list(pins["projects"])
    rows: list[dict[str, Any]] = []
    for project in wanted:
        print(f"[{project}] checkout {pins['projects'][project]['sha']}", flush=True)
        repo_dir = checkout_project(workdir / "repos", project, pins)
        nodes, links = file_links(repo_dir)
        print(f"[{project}] {len(nodes)} files, {len(links)} linked pairs", flush=True)
        rows.extend(judge([c for c in candidates if c.project == project], nodes, links))
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "rows.json").write_text(json.dumps(rows, indent=1) + "\n")
    summary = summarize(rows)
    summary["projects"] = wanted
    summary["labels_commit"] = pins["labels_source"]["commit"]
    (out / "summary.json").write_text(json.dumps(summary, indent=1) + "\n")
    print(json.dumps(summary, indent=1))
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(prog="eval cfgnet500")
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="score the file-level graph on the labelled candidates")
    r.add_argument("--out", required=True)
    r.add_argument("--workdir", default=str(Path.home() / ".cache" / "cfgnet500"))
    r.add_argument("--projects", help="comma separated subset of the ten projects")
    r.add_argument("--csv", help="a local copy of the pinned label file (still digest-checked)")
    s = sub.add_parser("summarize", help="re-summarize rows.json files")
    s.add_argument("--rows", nargs="+", required=True)
    args = ap.parse_args()
    if args.cmd == "run":
        return cmd_run(args)
    rows: list[dict[str, Any]] = []
    for path in args.rows:
        rows.extend(json.loads(Path(path).read_text()))
    print(json.dumps(summarize(rows), indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
