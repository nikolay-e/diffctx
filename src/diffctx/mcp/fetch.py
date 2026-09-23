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

from diffctx._diffctx import sanitize_text, withheld_paths

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
    for sep in ("...", ".."):
        if sep in ref:
            _, _, right = ref.partition(sep)
            right = right.strip()
            # git reads an empty right side as HEAD (`A..` is `A..HEAD`), and so does the engine.
            return right or "HEAD"
    # Anything without `..` — empty, `HEAD~3`, a duration window — is a base
    # that `git diff` compares against the working tree, so the ranking's line
    # numbers belong to the files on disk and the bodies must come from there.
    return None


def _blob_at(repo: Path, rev: str, rel_path: str) -> str | None:
    # `rev` is whatever followed `..` in the caller's diff_ref, and when
    # fragment_ids are given the engine never sees it — so nothing else has
    # validated it. A rev of `--output=x` would turn a read-only tool into a
    # file write; `--end-of-options` makes git read it as a revision only.
    if rev.startswith("-"):
        return None
    # `cat-file blob` rather than `show`: asked for a directory, `show` prints
    # the tree listing as if it were file content, and that listing names
    # entries the ignore rules may withhold. Only a blob can be a fragment body.
    try:
        proc = subprocess.run(
            ["git", "cat-file", "blob", "--end-of-options", f"{rev}:{rel_path}"],
            stdin=subprocess.DEVNULL,
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


def _slice(text: str, ref: FragmentRef) -> tuple[str, str] | None:
    """The requested lines and the span served, or None when the id points past
    the end of the file — a ranking whose line numbers do not fit the body is the
    revision/ranking divergence this module refuses to paper over."""
    lines = text.splitlines()
    if ref.start_line is None or ref.end_line is None:
        return text, f"1-{len(lines)}"
    if ref.start_line > len(lines):
        return None
    start = ref.start_line
    end = min(ref.end_line, len(lines))
    if end < start:
        return "", f"{ref.start_line}-{ref.end_line}"
    return "\n".join(lines[start - 1 : end]), f"{start}-{end}"


def _is_contained(repo: Path, rel_path: str) -> bool:
    """Containment only; whether the repo withholds the path is the engine's call.

    Containment is checked twice, because neither check alone is sufficient:

    - **Lexically**, which rejects `..` and absolute forms without touching the
      filesystem. This is the only check available for a path that exists solely
      in a historical revision.
    - **After resolution**, which is what catches a symlinked directory *inside*
      the repository pointing out of it. That path is lexically internal, so the
      first check passes it; a property fuzz over hostile path shapes found it
      reading a planted file one directory above the repo (#147).

    `resolve()` is non-strict, so it still follows the symlinked prefix of a path
    whose final component does not exist — which is exactly the escape shape.
    """
    if not rel_path or rel_path.startswith("/") or Path(rel_path).is_absolute():
        return False
    if ".." in Path(rel_path).parts:
        return False
    try:
        resolved = (repo / rel_path).resolve()
        if not resolved.is_relative_to(repo.resolve()):
            return False
    except (OSError, ValueError, RuntimeError):
        # Unresolvable (a symlink loop, a name the platform rejects): refuse.
        return False
    return True


def withheld_set(root: Path, rel_paths: list[str]) -> set[str]:
    """What the engine withholds, plus any name that cannot reach it.

    A filename that is not valid UTF-8 arrives as a surrogate-escaped `str`
    which the FFI boundary rejects; letting that raise would fail a whole call
    over one file nobody asked for by name, so it is simply not served.
    """
    clean, unservable = [], set()
    for rel in rel_paths:
        try:
            rel.encode("utf-8")
        except UnicodeEncodeError:
            unservable.add(rel)
        else:
            clean.append(rel)
    return set(withheld_paths(str(root), clean)) | unservable


@dataclass(frozen=True)
class FetchResult:
    markdown: str
    resolved: int


def fetch_fragments(repo: Path, diff_ref: str, fragment_ids: list[str], max_file_bytes: int) -> str:
    return fetch_result(repo, diff_ref, fragment_ids, max_file_bytes).markdown


def fetch_result(repo: Path, diff_ref: str, fragment_ids: list[str], max_file_bytes: int) -> FetchResult:
    # Every id is accounted for in the output — resolved, or named with the reason it was not. A
    # silently dropped id would read as "this fragment is empty", which is a different claim than
    # "this fragment was refused".
    if len(fragment_ids) > MAX_FETCH_IDS:
        raise ValueError(
            f"fragment_ids: {len(fragment_ids)} ids exceeds the {MAX_FETCH_IDS} per-call limit. "
            f'Fetch the ones you need, or ask for mode="pack" if you need most of the selection.'
        )

    rev = end_revision(diff_ref)
    rev_label = rev or "working tree"
    parts = [f"# {len(fragment_ids)} fragments at {rev_label}\n"]
    refs = [(raw, parse_fragment_id(raw)) for raw in fragment_ids]
    # One engine, one answer (#228): the same predicate that kept a path out
    # of the selection keeps it out of a fetch. Pathspec re-derivation here
    # both served `.netrc` and refused files the engine had ranked.
    contained = {ref.path for _, ref in refs if ref is not None and _is_contained(repo, ref.path)}
    withheld = withheld_set(repo, sorted(contained))
    resolved = 0
    for raw, ref in refs:
        section, body = _fetch_one(repo, rev, rev_label, raw, ref, contained, withheld, max_file_bytes)
        parts.append(section)
        resolved += body
    return FetchResult("\n".join(parts), resolved)


def _fetch_one(
    repo: Path,
    rev: str | None,
    rev_label: str,
    raw: str,
    ref: FragmentRef | None,
    contained: set[str],
    withheld: set[str],
    max_file_bytes: int,
) -> tuple[str, bool]:
    if ref is None:
        return f"## {raw}\n*Unparseable id — expected `path:start-end`, `path:line`, or `path`.*\n", False
    if ref.path not in contained or ref.path in withheld:
        # Deliberately one message for "outside the repo" and "ignored":
        # distinguishing them tells the caller whether a path they cannot
        # read nonetheless exists.
        return f"## {ref.path}\n*Not available: outside the repository or excluded by its ignore rules.*\n", False
    # Never the working tree under a revision's label: a file the revision
    # does not have (or a revision git cannot resolve) is reported, not
    # substituted with whatever is on disk.
    text = _blob_at(repo, rev, ref.path) if rev else _worktree_text(repo, ref.path)
    if text is None:
        return f"## {ref.path}\n*Not found at {rev_label}.*\n", False
    sliced = _slice(text, ref)
    if sliced is None:
        return (
            f"## {ref.path}\n*Not found: {ref.path} has {len(text.splitlines())} lines at {rev_label}; the id starts past the end.*\n",
            False,
        )
    body, span = sliced
    # The cap applies to what is served, not to the file it came from: the
    # engine ranks fragments of files up to its own (larger) limit, and every
    # id a ranking hands out must be fetchable.
    if len(body.encode("utf-8", errors="replace")) > max_file_bytes:
        return (
            f"## {ref.path}:{span}\n*Skipped: fragment exceeds {max_file_bytes:,} bytes; ask for a narrower line range.*\n",
            False,
        )
    body, redacted, categories = sanitize_text(body)
    suffix = Path(ref.path).suffix.lstrip(".")
    note = f"*{redacted} credential-shaped string(s) redacted: {', '.join(categories)}*\n" if redacted else ""
    return f"## {ref.path}:{span}\n{note}```{suffix}\n{body}\n```\n", True
