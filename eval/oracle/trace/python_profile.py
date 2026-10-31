"""Observed call edges of a Python repository's own tests, by `sys.setprofile` (T14 runtime evidence).

    python trace/python_profile.py --root REPO --out edges.jsonl -- -x -q tests/

runs `pytest` with the arguments after `--` (`--module` picks another runnable module) inside this process
and writes one JSON line per distinct edge between two functions defined under REPO:
`{"from": {path, qualname, line}, "to": {...}, "count": n}`. The tests execute the repository's code, so run
this inside the same isolation as indexing: no secrets, no network the tests do not need.
"""

from __future__ import annotations

import argparse
import json
import os
import runpy
import sys
import threading
from collections import Counter
from collections.abc import Callable
from pathlib import Path
from types import FrameType

Edge = tuple[str, str, int, str, str, int]


class EdgeRecorder:
    def __init__(self, root: Path) -> None:
        self.root = str(root.resolve()) + os.sep
        self.edges: Counter[Edge] = Counter()
        self._relative: dict[str, str | None] = {}

    def _inside(self, filename: str) -> str | None:
        if filename not in self._relative:
            real = os.path.realpath(filename)
            self._relative[filename] = real[len(self.root) :] if real.startswith(self.root) else None
        return self._relative[filename]

    def __call__(self, frame: FrameType, event: str, arg: object) -> None:
        if event != "call" or frame.f_back is None:
            return
        callee_path = self._inside(frame.f_code.co_filename)
        caller_path = self._inside(frame.f_back.f_code.co_filename)
        if callee_path is None or caller_path is None:
            return
        caller = frame.f_back
        edge = (
            caller_path,
            _qualname(caller.f_code.co_qualname),
            caller.f_lineno,
            callee_path,
            _qualname(frame.f_code.co_qualname),
            frame.f_code.co_firstlineno,
        )
        self.edges[edge] += 1

    def rows(self) -> list[dict]:
        return [
            {"from": {"path": a, "qualname": b, "line": c}, "to": {"path": d, "qualname": e, "line": f}, "count": n}
            for (a, b, c, d, e, f), n in sorted(self.edges.items())
        ]


def _qualname(raw: str) -> str:
    """`outer.<locals>.inner` is named `outer.inner`, as the oracle's definitions name nested functions."""
    return raw.replace(".<locals>", "")


def record(root: Path, run: Callable[[], object]) -> EdgeRecorder:
    recorder = EdgeRecorder(root)
    sys.setprofile(recorder)
    threading.setprofile(recorder)
    try:
        run()
    finally:
        sys.setprofile(None)
        threading.setprofile(None)
    return recorder


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python_profile", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--module", default="pytest")
    parser.add_argument("args", nargs=argparse.REMAINDER)
    options = parser.parse_args(argv)
    arguments = options.args[1:] if options.args[:1] == ["--"] else options.args
    status = 0

    def run() -> None:
        nonlocal status
        sys.argv = [options.module, *arguments]
        sys.path.insert(0, str(options.root))
        try:
            runpy.run_module(options.module, run_name="__main__", alter_sys=True)
        except SystemExit as stop:
            status = stop.code if isinstance(stop.code, int) else 1

    recorder = record(options.root, run)
    options.out.write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in recorder.rows()), encoding="utf-8")
    print(f"{len(recorder.edges)} edges, tests exited {status}", file=sys.stderr)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
