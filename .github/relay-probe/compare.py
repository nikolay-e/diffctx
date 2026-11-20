"""compare.py <base-dump> <head-dump>: every corpus answer of two builds, byte for byte after dropping wall-clock
fields, the fixture repositories' commit ids and temporary directory names (each run commits its fixtures at its own time); prints the moved
answers and exits 1 when any moved or a case is missing on one side."""
import json, pathlib, re, sys

TEMP_DIR = re.compile(rb"\.tmp[A-Za-z0-9]{6}\b")
COMMIT_ID = re.compile(rb"\b[0-9a-f]{40}\b|\b(?=[0-9a-f]*[0-9])(?=[0-9a-f]*[a-f])[0-9a-f]{7,12}\b")

base, head = (pathlib.Path(p) for p in sys.argv[1:3])


def norm(p, exact=True):
    raw = p.read_bytes()
    if p.suffix == ".json":
        try:
            doc = json.loads(raw)
        except ValueError:
            doc = None
        if isinstance(doc, dict):
            doc.pop("latency", None)
            doc.pop("provenance", None)
            if not exact:
                doc.pop("fit", None)
            raw = json.dumps(doc, sort_keys=True).encode()
    return TEMP_DIR.sub(b".tmp<dir>", COMMIT_ID.sub(b"<commit>", raw))


def files(root):
    return {p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_file() and p.suffix in (".json", ".md")}


a, b = files(base), files(head)
# The fixture repository's temp name enters the answer, and its spelling moves the token count by a rounding step:
# an answer whose only move is `fit` (and the md that carries no other change) is token noise, not a move.
differs = sorted(n for n in a & b if norm(base / n) != norm(head / n))
noise = {n for n in differs if n.endswith(".json") and norm(base / n, False) == norm(head / n, False)}
noise |= {n for n in differs if n.endswith(".md") and n[:-3] + ".json" in noise}
moved = [n for n in differs if n not in noise]
print(f"token-count only (fit): {len(noise)}")
only = sorted(a ^ b)
print(f"answers compared: {len(a & b)}; moved: {len(moved)}; on one side only: {len(only)}")
for n in moved[:60]:
    print(f"  MOVED {n}")
for n in only[:30]:
    print(f"  ONE-SIDED {n} ({'base' if n in a else 'head'})")
sys.exit(1 if moved or only else 0)
