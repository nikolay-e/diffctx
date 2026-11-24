#!/usr/bin/env python3
"""Materialize impact/ and symbol/ corpus cases and run diffctx binaries on them.

Mirrors crates/diffctx-native/tests/common/oracle/{case,engine}.rs (schema v1):
initial commit, then the change (renames, changed_files, deletions) committed
or left in the work tree, then untracked files; impact asks
`--diff <base>..HEAD|HEAD --mode impact -f json -q [--unfold] <query.args>`,
symbol asks `--symbol NAME -f json -q [--unfold] <query.args>`.

One result file per case and binary: <out>/<bin>/<mode>/<lang>/<stem>.json
(raw stdout) plus <stem>.err on a nonzero exit (rc and stderr). A case is
skipped when its result exists, unless --force. No build, no cargo: only the
binaries named on the command line run.

  run.py --cases-root DIR --out DIR --bin BASE=PATH --bin MID=PATH --bin HEAD=PATH
         [--filter SUBSTR ...] [--list FILE] [--shard K/N] [--jobs 4] [--force]
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import yaml

SCRATCH = Path(tempfile.gettempdir()) / "hedge-scratch"
GIT_ENV = {
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_AUTHOR_NAME": "test",
    "GIT_AUTHOR_EMAIL": "test@example.com",
    "GIT_COMMITTER_NAME": "test",
    "GIT_COMMITTER_EMAIL": "test@example.com",
}
NICE = []


def git(root, *args):
    out = subprocess.run(
        [*NICE, "git", *args],
        cwd=root,
        env={**os.environ, **GIT_ENV},
        capture_output=True,
        text=True,
    )
    if out.returncode:
        raise RuntimeError(f"git {' '.join(args)} failed: {out.stderr.strip()}")
    return out.stdout.strip()


def safe_relative(rel):
    if not rel or rel.startswith("/") or ".." in rel.split("/") or "\\" in rel:
        raise ValueError(f"`{rel}` is not a path inside the repository")


def write_files(root, files):
    for rel, content in (files or {}).items():
        safe_relative(rel)
        full = root / rel
        full.parent.mkdir(parents=True, exist_ok=True)
        full.write_text(content, encoding="utf-8")


def materialise(mode, repo, root):
    """Returns the base commit. Same order as engine.rs::materialise."""
    git(root, "init", "--quiet", "-b", "main")
    git(root, "config", "user.email", "test@example.com")
    git(root, "config", "user.name", "test")
    git(root, "config", "commit.gpgsign", "false")
    write_files(root, repo.get("initial_files"))
    git(root, "add", "-A")
    git(root, "commit", "--quiet", "-m", "Initial commit", "--allow-empty")
    base = git(root, "rev-parse", "HEAD")
    changed = repo.get("changed_files") or {}
    deleted = repo.get("deleted_files") or []
    renamed = repo.get("renamed_files") or {}
    if changed or deleted or renamed or mode == "impact":
        for src, dst in renamed.items():
            safe_relative(src)
            safe_relative(dst)
            (root / dst).parent.mkdir(parents=True, exist_ok=True)
            (root / src).rename(root / dst)
        write_files(root, changed)
        for rel in deleted:
            safe_relative(rel)
            (root / rel).unlink()
        if not repo.get("worktree"):
            git(root, "add", "-A")
            git(
                root,
                "commit",
                "--quiet",
                "-m",
                repo.get("commit_message") or "Update files",
                "--allow-empty",
            )
    write_files(root, repo.get("untracked_files"))
    return base


_unfold = {}


def supports_unfold(binary):
    if binary not in _unfold:
        out = subprocess.run([binary, "--help"], capture_output=True, text=True)
        _unfold[binary] = "--unfold" in out.stdout
    return _unfold[binary]


def command(binary, mode, case, base):
    query = case.get("query") or {}
    args = []
    if mode == "impact":
        rng = "HEAD" if (case.get("repo") or {}).get("worktree") else f"{base}..HEAD"
        args += ["--diff", rng, "--mode", "impact"]
    else:
        args += ["--symbol", query.get("symbol") or ""]
    args += ["-f", "json", "-q"]
    if supports_unfold(binary):
        args.append("--unfold")
    args += list(query.get("args") or [])
    return args


def run_one(item, bins, out, cache_root, force):
    mode, lang, stem, path = item
    todo = {
        name: out / name / mode / lang / f"{stem}.json"
        for name in bins
        if force or not (out / name / mode / lang / f"{stem}.json").exists()
    }
    if not todo:
        return item, "skipped"
    case = yaml.safe_load(path.read_text(encoding="utf-8"))
    SCRATCH.mkdir(exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix=f"{mode}-{lang}-", dir=SCRATCH))
    try:
        base = materialise(mode, case.get("repo") or {}, work)
        env_case = case.get("query", {}).get("env") or {}
        for name, dest in todo.items():
            binary = bins[name]
            cache = cache_root / name
            cache.mkdir(parents=True, exist_ok=True)
            env = {**os.environ, **GIT_ENV, "DIFFCTX_CACHE_DIR": str(cache), **env_case}
            started = time.monotonic()
            proc = subprocess.run(
                [*NICE, binary, *command(binary, mode, case, base)],
                cwd=work,
                env=env,
                capture_output=True,
            )
            dest.parent.mkdir(parents=True, exist_ok=True)
            err = dest.with_suffix(".err")
            if proc.returncode == 0:
                dest.write_bytes(proc.stdout)
                err.unlink(missing_ok=True)
            else:
                dest.write_bytes(b"")
                err.write_text(f"rc={proc.returncode}\n" + proc.stderr.decode("utf-8", "replace"))
            dest.with_suffix(".sec").write_text(f"{time.monotonic() - started:.3f}\n")
        return item, "ran"
    except Exception as e:  # a case that does not build is reported, never dropped
        for name in todo:
            err = todo[name].with_suffix(".err")
            err.parent.mkdir(parents=True, exist_ok=True)
            err.write_text(f"materialise failed: {e}\n")
        return item, f"error: {e}"
    finally:
        shutil.rmtree(work, ignore_errors=True)


def discover(root, filters, list_file):
    wanted = None
    if list_file:
        wanted = {ln.strip() for ln in Path(list_file).read_text().splitlines() if ln.strip() and not ln.startswith("#")}
    items = []
    for mode in ("impact", "symbol"):
        for path in sorted((root / mode).glob("*/*.yaml")):
            key = f"{mode}/{path.parent.name}/{path.stem}"
            if wanted is not None and key not in wanted:
                continue
            if filters and not any(f in key for f in filters):
                continue
            items.append((mode, path.parent.name, path.stem, path))
    return items


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases-root", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--bin", action="append", default=[], metavar="NAME=PATH")
    ap.add_argument("--filter", action="append", default=[])
    ap.add_argument("--list", dest="list_file")
    ap.add_argument("--shard", default="1/1", help="K/N: every N-th case of the sorted list, from the K-th")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--force", action="store_true")
    a = ap.parse_args()
    bins = {n: str(Path(p).resolve()) for n, p in (b.split("=", 1) for b in a.bin)}
    if not bins:
        sys.exit("no --bin NAME=PATH")
    for name, path in bins.items():
        if not os.access(path, os.X_OK):
            sys.exit(f"{name}: {path} is not executable")
    out = Path(a.out)
    cache_root = SCRATCH / "cache"
    items = discover(Path(a.cases_root), a.filter, a.list_file)
    k, n = (int(x) for x in a.shard.split("/"))
    items = items[k - 1 :: n]
    print(f"{len(items)} cases x {len(bins)} binaries -> {out}", flush=True)
    done = errors = 0
    try:
        with ThreadPoolExecutor(max_workers=a.jobs) as pool:
            for item, state in pool.map(lambda i: run_one(i, bins, out, cache_root, a.force), items):
                done += 1
                if state.startswith("error"):
                    errors += 1
                    print(
                        f"[{done}/{len(items)}] {item[0]}/{item[1]}/{item[2]}: {state}",
                        flush=True,
                    )
                elif done % 25 == 0:
                    print(f"[{done}/{len(items)}]", flush=True)
    finally:
        shutil.rmtree(cache_root, ignore_errors=True)
    print(f"done {done} cases, {errors} materialise errors", flush=True)


if __name__ == "__main__":
    main()
