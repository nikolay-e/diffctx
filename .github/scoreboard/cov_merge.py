#!/usr/bin/env python3
"""Two line-coverage reports of the same source, merged: which product lines each reached, and which neither did.

  cov_merge.py --rust rust.lcov --python python.lcov --out DIR [--sha SHA]

`--rust`   cargo llvm-cov over the Rust test suites (the CLI, hook and library doors, as the tests drive them)
`--python` pytest through the instrumented `_diffctx` extension and native binary (the Python, CLI and MCP doors)
Only product source counts: crates/<crate>/src/**. A line is reached when any report that instruments it
counted it; a line is in the dead-code candidate set when every report that instruments it counted it zero
times. The two builds differ (release against debug, features), so a line one build does not instrument is
judged by the other alone: that is stated in the summary, not hidden.

Writes into DIR: coverage.md (summary), files.tsv, functions.tsv, neither-lines.txt (path: line ranges).
"""

import argparse
import collections
import re
from pathlib import Path

PRODUCT = re.compile(r"(?:^|/)(crates/([^/]+)/src/.+\.rs)$")


def parse(path):
    """({file: {line: hits}}, {file: {(start, end, name): hits}})."""
    lines = collections.defaultdict(dict)
    funcs = collections.defaultdict(dict)
    spans = {}
    cur = None
    for raw in Path(path).read_text(errors="replace").splitlines():
        if raw.startswith("SF:"):
            m = PRODUCT.search(raw[3:])
            cur = m.group(1) if m else None
            spans = {}
        elif cur is None:
            continue
        elif raw.startswith("DA:"):
            n, c = raw[3:].split(",")[:2]
            lines[cur][int(n)] = max(lines[cur].get(int(n), 0), int(c))
        elif raw.startswith("FN:"):
            parts = raw[3:].split(",", 2)
            if len(parts) == 3 and parts[1].isdigit():
                spans[parts[2]] = (int(parts[0]), int(parts[1]))
            else:
                spans[parts[1]] = (int(parts[0]), int(parts[0]))
        elif raw.startswith("FNDA:"):
            count, name = raw[5:].split(",", 1)
            if name in spans:
                key = (*spans[name], demangle(name))
                funcs[cur][key] = funcs[cur].get(key, 0) + int(count)
    return lines, funcs


def demangle(name):
    """Legacy `_ZN<len><seg>...E` symbols; anything else (v0, C) is returned as it is."""
    if not name.startswith("_ZN"):
        return name
    rest, segments = name[3:], []
    while rest and rest[0].isdigit():
        m = re.match(r"(\d+)", rest)
        size = int(m.group(1))
        segments.append(rest[m.end() : m.end() + size])
        rest = rest[m.end() + size :]
    if segments and re.fullmatch(r"h[0-9a-f]{16}", segments[-1]):
        segments.pop()
    return "::".join(s.replace("$LT$", "<").replace("$GT$", ">").replace("$u20$", " ").replace("..", "::") for s in segments)


def ranges(numbers):
    spans, start = [], numbers[0]
    for x, y in zip(numbers, [*numbers[1:], None]):
        if y != x + 1:
            spans.append(f"{start}" if start == x else f"{start}-{x}")
            start = y
    return ",".join(spans)


def crate_of(path):
    return path.split("/")[1]


def tally(rl, pl):
    """Per file and per crate counts, the neither-lines per file, and the lines only one report instruments."""
    per_file, per_crate = [], collections.defaultdict(collections.Counter)
    neither_lines, only_one = {}, collections.Counter()
    for f in sorted(set(rl) | set(pl)):
        c, dead = collections.Counter(), []
        for n in sorted(set(rl.get(f, {})) | set(pl.get(f, {}))):
            r, p = rl.get(f, {}).get(n), pl.get(f, {}).get(n)
            c["lines"] += 1
            c["rust"] += bool(r)
            c["python"] += bool(p)
            c["both"] += bool(r) and bool(p)
            if not r and not p:
                c["neither"] += 1
                dead.append(n)
            if (r is None) != (p is None):
                only_one[crate_of(f)] += 1
        per_file.append((f, c))
        per_crate[crate_of(f)].update(c)
        if dead:
            neither_lines[f] = dead
    return per_file, per_crate, neither_lines, only_one


def function_rows(rl, pl, rf, pf):
    rows = []
    for f in sorted(set(rf) | set(pf)):
        # One row per source span: generic functions and closures are instantiated once per crate and
        # type, each under its own symbol, and the span is called when any instance was.
        merged, names = collections.defaultdict(lambda: [0, 0]), {}
        for idx, table in ((0, rf.get(f, {})), (1, pf.get(f, {}))):
            for (start, end, name), count in table.items():
                merged[start, end][idx] += count
                names.setdefault((start, end), name)
        for (start, end), (rc, pc) in sorted(merged.items()):
            name = names[start, end]
            span = [n for n in range(start, end + 1) if n in rl.get(f, {}) or n in pl.get(f, {})]
            if not span:
                continue
            rr = sum(bool(rl.get(f, {}).get(n)) for n in span)
            pp = sum(bool(pl.get(f, {}).get(n)) for n in span)
            nn = sum(not rl.get(f, {}).get(n) and not pl.get(f, {}).get(n) for n in span)
            rows.append((f, start, end, name, len(span), rr, pp, nn, rc, pc))
    return rows


def share(x, y):
    return f"{x / y:.1%}" if y else "-"


def summary(sha, per_file, per_crate, only_one, fn_rows, dead_fns):
    total = collections.Counter()
    for c in per_crate.values():
        total.update(c)
    lines = [
        f"# Product line coverage{sha}",
        "",
        "Lines of crates/*/src. `rust`: cargo llvm-cov over the Rust suites; `python`: pytest through the instrumented "
        "extension and native binary; `neither`: every report that instruments the line counted it zero times "
        "(the dead-code candidate set).",
        "",
        "| crate | lines | rust | python | both | neither | neither share | instrumented by one report only |",
        "|---|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for name, c in sorted(per_crate.items(), key=lambda kv: -kv[1]["neither"]):
        lines.append(
            f"| {name} | {c['lines']} | {c['rust']} | {c['python']} | {c['both']} | {c['neither']} | "
            f"{share(c['neither'], c['lines'])} | {only_one[name]} |"
        )
    lines.append(
        f"| **all** | {total['lines']} | {total['rust']} | {total['python']} | {total['both']} | {total['neither']} | "
        f"{share(total['neither'], total['lines'])} | {sum(only_one.values())} |"
    )
    lines += [
        "",
        f"functions in the merged reports: {len(fn_rows)}; never called and no line reached by either report: {len(dead_fns)}",
        "",
        "## Files with most lines neither report reached",
        "",
        "| file | neither | of |",
        "|---|---:|---:|",
    ]
    for f, c in sorted(per_file, key=lambda fc: -fc[1]["neither"])[:40]:
        if c["neither"]:
            lines.append(f"| {f} | {c['neither']} | {c['lines']} |")
    lines += [
        "",
        "## Functions never called by either, with most lines",
        "",
        "| function | file:start-end | lines |",
        "|---|---|---:|",
    ]
    for row in sorted(dead_fns, key=lambda r: -r[4])[:40]:
        lines.append(f"| {row[3]} | {row[0]}:{row[1]}-{row[2]} | {row[4]} |")
    return lines


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rust", required=True)
    ap.add_argument("--python", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--sha", default="")
    a = ap.parse_args()
    out = Path(a.out)
    out.mkdir(parents=True, exist_ok=True)
    (rl, rf), (pl, pf) = parse(a.rust), parse(a.python)
    per_file, per_crate, neither_lines, only_one = tally(rl, pl)
    with open(out / "files.tsv", "w") as fh:
        fh.write("file\tlines\trust_reached\tpython_reached\tboth\tneither\n")
        for f, c in per_file:
            fh.write(f"{f}\t{c['lines']}\t{c['rust']}\t{c['python']}\t{c['both']}\t{c['neither']}\n")
    with open(out / "neither-lines.txt", "w") as fh:
        for f, dead in sorted(neither_lines.items(), key=lambda kv: -len(kv[1])):
            fh.write(f"{f}: {ranges(dead)}\n")
    fn_rows = function_rows(rl, pl, rf, pf)
    with open(out / "functions.tsv", "w") as fh:
        fh.write("file\tstart\tend\tfunction\tlines\trust_lines\tpython_lines\tneither_lines\trust_calls\tpython_calls\n")
        for row in fn_rows:
            fh.write("\t".join(str(x) for x in row) + "\n")
    dead_fns = [r for r in fn_rows if r[8] == 0 and r[9] == 0 and r[5] == 0 and r[6] == 0]
    lines = summary(f" at {a.sha}" if a.sha else "", per_file, per_crate, only_one, fn_rows, dead_fns)
    (out / "coverage.md").write_text("\n".join(lines) + "\n")
    print("\n".join(lines[:20]))


if __name__ == "__main__":
    main()
