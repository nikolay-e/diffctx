"""Second half of progressive disclosure: bodies for ids a locate call ranked.

`mode="locate"` returns a navigation list costing a few hundred tokens; this
resolves a chosen subset of it to source. The split is the point — an agent
reads the ranking, decides what it actually needs, and pays for that alone
instead of for a whole pack.

Ids are `path:start-end` exactly as locate emits `path` and `lines`, so the
second call is a copy of fields from the first with no id table to maintain.

Two invariants this module exists to hold:

- **The revision matches the ranking.** Line numbers came from the diff's end
  revision, so bodies are read there too (`git show <rev>:<path>`). Reading the
  working tree instead would silently mis-slice every fragment whenever the
  range is historical or the tree is dirty — the worst kind of wrong, because
  the output still looks like source.
- **The ignore contract is not a suggestion.** `.gitignore` and
  `.diffctx/ignore` are enforced here as they are in every other tool. An id is
  attacker-influenceable input (it can arrive from anything the model read), so
  a fetch must not become the one door that opens what the repo withholds.
"""

from __future__ import annotations

import subprocess
from dataclasses import dataclass
from pathlib import Path

# A fetch is meant to be a targeted follow-up. Past this many ids the caller is
# re-implementing the pack, one round trip at a time, and should ask for the
# pack instead.
MAX_FETCH_IDS = 40

_GIT_TIMEOUT_SECONDS = 30


@dataclass(frozen=True)
class FragmentRef:
    path: str
    start_line: int | None
    end_line: int | None


def parse_fragment_id(raw: str) -> FragmentRef | None:
    """`src/a.py:12-40`, `src/a.py:12`, or `src/a.py` (whole file).

    Returns `None` for anything unparseable rather than raising: one malformed
    id in a batch of twenty should cost that id, not the call.
    """
    text = raw.strip()
    if not text:
        return None
    path, sep, span = text.rpartition(":")
    if not sep:
        return FragmentRef(text, None, None)
    if not path:
        return None
    lo, dash, hi = span.partition("-")
    try:
        start = int(lo)
        end = int(hi) if dash and hi else start
    except ValueError:
        # A colon that was part of the path, not a line span.
        return FragmentRef(text, None, None)
    if start < 1 or end < start:
        return None
    return FragmentRef(path, start, end)


def end_revision(diff_ref: str) -> str | None:
    """The revision a locate ranking's line numbers refer to.

    `None` means the working tree: that is what `git diff` with no range
    compares against, and reading committed blobs for it would report content
    the caller cannot see in their editor.
    """
    ref = diff_ref.strip()
    if not ref:
        return None
    for sep in ("...", ".."):
        if sep in ref:
            _, _, right = ref.partition(sep)
            right = right.strip()
            # `A..` means "A to the working tree" in git's own reading.
            return right or None
    return ref


def _blob_at(repo: Path, rev: str, rel_path: str) -> str | None:
    try:
        proc = subprocess.run(
            ["git", "show", f"{rev}:{rel_path}"],
            cwd=repo,
            capture_output=True,
            timeout=_GIT_TIMEOUT_SECONDS,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout.decode("utf-8", errors="replace")


def _worktree_text(repo: Path, rel_path: str) -> str | None:
    target = repo / rel_path
    try:
        if not target.is_file():
            return None
        return target.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None


def _slice(text: str, ref: FragmentRef) -> tuple[str, str]:
    lines = text.splitlines()
    if ref.start_line is None or ref.end_line is None:
        return text, f"1-{len(lines)}"
    start = min(ref.start_line, len(lines)) if lines else 1
    end = min(ref.end_line, len(lines))
    if end < start:
        return "", f"{ref.start_line}-{ref.end_line}"
    return "\n".join(lines[start - 1 : end]), f"{start}-{end}"


def _is_admissible(repo: Path, rel_path: str) -> bool:
    """Containment plus the repo's own ignore contract.

    Containment is checked lexically on the joined path — the file need not
    exist in the working tree, since it may only exist in a historical
    revision, so `resolve()` on a missing path is not a usable test here.
    """
    from diffctx.ignore import get_ignore_specs

    if not rel_path or rel_path.startswith("/") or Path(rel_path).is_absolute():
        return False
    if ".." in Path(rel_path).parts:
        return False
    spec = get_ignore_specs(repo, None, False, None)
    return not spec.match_file(rel_path)


def fetch_fragments(repo: Path, diff_ref: str, fragment_ids: list[str], max_file_bytes: int) -> str:
    """Markdown bodies for `fragment_ids`, one section per id.

    Every id is accounted for in the output — resolved, or named with the
    reason it was not. A silently dropped id would read as "this fragment is
    empty", which is a different claim than "this fragment was refused".
    """
    if len(fragment_ids) > MAX_FETCH_IDS:
        raise ValueError(
            f"fragment_ids: {len(fragment_ids)} ids exceeds the {MAX_FETCH_IDS} per-call limit. "
            f'Fetch the ones you need, or ask for mode="pack" if you need most of the selection.'
        )

    rev = end_revision(diff_ref)
    rev_label = rev or "working tree"
    parts = [f"# {len(fragment_ids)} fragments at {rev_label}\n"]
    for raw in fragment_ids:
        ref = parse_fragment_id(raw)
        if ref is None:
            parts.append(f"## {raw}\n*Unparseable id — expected `path:start-end`, `path:line`, or `path`.*\n")
            continue
        if not _is_admissible(repo, ref.path):
            # Deliberately one message for "outside the repo" and "ignored":
            # distinguishing them tells the caller whether a path they cannot
            # read nonetheless exists.
            parts.append(f"## {ref.path}\n*Not available: outside the repository or excluded by its ignore rules.*\n")
            continue
        text = _blob_at(repo, rev, ref.path) if rev else None
        if text is None:
            # Created after `rev`, or the range ends at the working tree.
            text = _worktree_text(repo, ref.path)
        if text is None:
            parts.append(f"## {ref.path}\n*Not found at {rev_label}.*\n")
            continue
        if len(text.encode("utf-8", errors="replace")) > max_file_bytes:
            parts.append(f"## {ref.path}\n*Skipped: file exceeds {max_file_bytes:,} bytes.*\n")
            continue
        body, span = _slice(text, ref)
        suffix = Path(ref.path).suffix.lstrip(".")
        parts.append(f"## {ref.path}:{span}\n```{suffix}\n{body}\n```\n")
    return "\n".join(parts)
