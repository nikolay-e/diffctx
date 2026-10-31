"""V8 function coverage of a TypeScript or JavaScript repository's own tests (T14 runtime evidence).

    python trace/ts_coverage.py --root REPO --out functions.jsonl -- npm test

runs the command with `NODE_V8_COVERAGE` set and writes one JSON line per function V8 saw under REPO:
`{"path", "name", "line", "count"}`; `count` above zero means the function ran. Coverage says a function
executed, not who called it, so it backs "this definition is reached at run time" and cannot confirm an
edge. Code a test runner transpiles is reported against the script V8 ran; mapping it back to the source
needs its source maps and is not done here. Run it inside the isolation the indexers use.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import unquote, urlparse


def utf16_line_starts(text: str) -> list[int]:
    """Offsets, in UTF-16 code units as V8 counts them, at which each line of `text` starts."""
    starts = [0]
    unit = 0
    for char in text:
        unit += 2 if ord(char) > 0xFFFF else 1
        if char == "\n":
            starts.append(unit)
    return starts


def line_of(starts: list[int], offset: int) -> int:
    low, high = 0, len(starts)
    while low < high:
        middle = (low + high) // 2
        if starts[middle] <= offset:
            low = middle + 1
        else:
            high = middle
    return low


def read_coverage(directory: Path, root: Path) -> list[dict]:
    root = root.resolve()
    rows: list[dict] = []
    for dump in sorted(directory.glob("coverage-*.json")):
        for script in json.loads(dump.read_text(encoding="utf-8")).get("result", []):
            url = urlparse(script.get("url", ""))
            if url.scheme != "file":
                continue
            path = Path(unquote(url.path))
            try:
                relative = path.resolve().relative_to(root)
            except ValueError:
                continue
            if "node_modules" in relative.parts or not path.is_file():
                continue
            starts = utf16_line_starts(path.read_text(encoding="utf-8", errors="replace"))
            for function in script.get("functions", []):
                ranges = function.get("ranges") or []
                if ranges and function.get("functionName"):
                    rows.append(
                        {
                            "path": relative.as_posix(),
                            "name": function["functionName"],
                            "line": line_of(starts, ranges[0]["startOffset"]),
                            "count": ranges[0]["count"],
                        }
                    )
    return sorted(rows, key=lambda r: (r["path"], r["line"], r["name"]))


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="ts_coverage", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    options = parser.parse_args(argv)
    command = options.command[1:] if options.command[:1] == ["--"] else options.command
    if not command:
        parser.error("give the test command after --")
    with tempfile.TemporaryDirectory(prefix="oracle-v8-") as scratch:
        env = {**os.environ, "NODE_V8_COVERAGE": scratch}
        status = subprocess.run(command, cwd=options.root, env=env, check=False).returncode
        rows = read_coverage(Path(scratch), options.root)
    options.out.write_text("".join(json.dumps(r, sort_keys=True) + "\n" for r in rows), encoding="utf-8")
    print(f"{len(rows)} functions, tests exited {status}", file=sys.stderr)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
