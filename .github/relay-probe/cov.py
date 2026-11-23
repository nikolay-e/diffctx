"""cov.py <lcov.info>: product lines (crates/*/src) no test executes, per file, most first; the full list as path:ranges."""
import collections, pathlib, sys

hits = collections.defaultdict(dict)
cur = None
for line in pathlib.Path(sys.argv[1]).read_text().splitlines():
    if line.startswith("SF:"):
        cur = line[3:]
    elif line.startswith("DA:") and cur:
        n, c = line[3:].split(",")[:2]
        hits[cur][int(n)] = max(hits[cur].get(int(n), 0), int(c))
rows, total, missed = [], 0, 0
for path, lines in hits.items():
    rel = path.split("/crates/", 1)[-1]
    if "/src/" not in rel or "/tests/" in rel:
        continue
    zero = sorted(n for n, c in lines.items() if c == 0)
    total += len(lines); missed += len(zero)
    if zero:
        spans, start = [], zero[0]
        for x, y in zip(zero, zero[1:] + [None]):
            if y != x + 1:
                spans.append(f"{start}" if start == x else f"{start}-{x}"); start = y
        rows.append((len(zero), len(lines), "crates/" + rel, spans))
rows.sort(reverse=True)
pathlib.Path("uncovered.txt").write_text("".join(f"{p}: {','.join(s)}\n" for _, _, p, s in rows))
print(f"product lines instrumented {total}, never executed {missed} ({missed / max(total, 1):.1%}) in {len(rows)} files\n")
print("| file | never executed | of |\n|---|---:|---:|")
for z, t, p, _ in rows[:60]:
    print(f"| {p} | {z} | {t} |")
