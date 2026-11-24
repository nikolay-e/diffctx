"""profile.py <perf-dir> <gitcount-dir>: where 1.20.0's and the head's instructions and git processes go, per cell.

Reads perf.sh's callgrind files (<cell>-base.cg = 1.20.0, <cell>-step.cg = head) and gitcount.sh's strace files
(<cell>-old.st, <cell>-new.st). Prints the top functions by inclusive instructions for both binaries and the git
subcommands each one starts. Never fails: it explains a gate, it is not one."""

import collections
import pathlib
import re
import subprocess
import sys

perf, gitc = (pathlib.Path(p) for p in sys.argv[1:3])
ROW = re.compile(r"^\s*([\d,]+) \(\s*[\d.]+%\)\s+(.*)$")
SHORT = re.compile(r"\s*\[[^\]]*\]$")


def top(cg, inclusive, n):
    out = subprocess.run(
        [
            "callgrind_annotate",
            f"--inclusive={'yes' if inclusive else 'no'}",
            "--threshold=100",
            str(cg),
        ],
        capture_output=True,
        text=True,
    ).stdout
    rows = [m for m in map(ROW.match, out.splitlines()) if m]
    return [f"{int(m.group(1).replace(',', '')):>16,}  {SHORT.sub('', m.group(2))[:150]}" for m in rows[:n]]


def subcommands(st):
    counts = collections.Counter()
    for line in st.read_text(errors="replace").splitlines():
        m = re.search(r'execve\("[^"]*/git", \[(.*?)\]', line)
        if not m or not line.rstrip().endswith("= 0"):
            continue
        args = re.findall(r'"((?:[^"\\]|\\.)*)"', m.group(1))[1:]
        sub, skip = "?", False
        for a in args:
            if skip:
                skip = False
            elif a in ("-C", "-c", "--git-dir", "--work-tree"):
                skip = True
            elif not a.startswith("-"):
                sub = a
                break
        counts[sub] += 1
    return counts


for cell in ("impact-narrow", "locate-ppr-narrow", "symbol", "pack-ego-wide"):
    base, head = perf / f"{cell}-base.cg", perf / f"{cell}-step.cg"
    if not (base.exists() and head.exists()):
        print(f"### {cell}: no callgrind files\n")
        continue
    print(f"### {cell}: top functions by inclusive instructions\n")
    for tag, cg in (("1.20.0", base), ("head", head)):
        print(f"{tag}:\n```")
        print("\n".join(top(cg, True, 40)))
        print("```")
    print("head, exclusive (self) instructions:\n```")
    print("\n".join(top(head, False, 20)))
    print("```")
    old, new = gitc / f"{cell}-old.st", gitc / f"{cell}-new.st"
    if old.exists() and new.exists():
        o, h = subcommands(old), subcommands(new)
        print(f"\ngit subcommands ({cell}):\n\n| subcommand | 1.20.0 | head |\n|---|---:|---:|")
        for s in sorted(set(o) | set(h), key=lambda s: -(h[s] + o[s])):
            print(f"| {s} | {o[s]} | {h[s]} |")
    print()
