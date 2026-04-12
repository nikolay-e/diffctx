from __future__ import annotations

import logging
import re
import subprocess
from pathlib import Path
from types import TracebackType

from .types import DiffHunk

logger = logging.getLogger(__name__)

_HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
_RANGE_RE = re.compile(r"^\s*(\S+?)(\.\.\.?)(\S*?)\s*$")  # NOSONAR(S5852)
_SAFE_RANGE_RE = re.compile(r"^[a-zA-Z0-9_.^~/@{}\-]+(\.\.\.?[a-zA-Z0-9_.^~/@{}\-]*)?$")
_SAFE_DIFF_FLAGS = ["--no-textconv", "--no-ext-diff"]


class GitError(Exception):
    pass


def _validate_diff_range(diff_range: str) -> None:
    if not _SAFE_RANGE_RE.match(diff_range.strip()):
        raise GitError(f"Invalid diff range: {diff_range!r}")


def run_git(repo_root: Path, args: list[str]) -> str:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo_root), *args],
            capture_output=True,
            encoding="utf-8",
            errors="replace",
            check=True,
            timeout=60,
        )
        return result.stdout
    except subprocess.CalledProcessError as e:
        raise GitError(f"git {' '.join(args)} failed: {e.stderr.strip()}") from e
    except subprocess.TimeoutExpired as e:
        raise GitError(f"git {' '.join(args)} timed out after 60s") from e
    except FileNotFoundError as e:
        raise GitError("git is not installed or not in PATH") from e


def is_git_repo(path: Path) -> bool:
    try:
        run_git(path, ["rev-parse", "--git-dir"])
        return True
    except GitError:
        return False


def get_diff_text(repo_root: Path, diff_range: str) -> str:
    _validate_diff_range(diff_range)
    return run_git(repo_root, ["diff", *_SAFE_DIFF_FLAGS, diff_range])


def _parse_hunk_header(match: re.Match[str], path: Path) -> DiffHunk:
    old_start = int(match.group(1))
    old_len_str = match.group(2)
    old_len = int(old_len_str) if old_len_str else 1
    new_start = int(match.group(3))
    new_len_str = match.group(4)
    new_len = int(new_len_str) if new_len_str else 1

    return DiffHunk(
        path=path,
        new_start=new_start,
        new_len=new_len,
        old_start=old_start,
        old_len=old_len,
    )


_C_ESCAPE_MAP = {"t": "\t", "n": "\n", "r": "\r", "b": "\b", "f": "\f", "v": "\v", "a": "\a", "\\": "\\", '"': '"'}


def _unquote_c_style(quoted: str) -> str:
    if not (quoted.startswith('"') and quoted.endswith('"')):
        return quoted
    raw = quoted[1:-1]
    chars: list[str] = []
    i = 0
    while i < len(raw):
        if raw[i] == "\\" and i + 1 < len(raw):
            nxt = raw[i + 1]
            if nxt in _C_ESCAPE_MAP:
                chars.append(_C_ESCAPE_MAP[nxt])
                i += 2
            elif nxt in "01234567" and i + 3 <= len(raw) and all(c in "01234567" for c in raw[i + 1 : i + 4]):
                chars.append(chr(int(raw[i + 1 : i + 4], 8)))
                i += 4
            else:
                chars.append("\\")
                i += 1
        else:
            chars.append(raw[i])
            i += 1
    result = "".join(chars)
    try:
        return result.encode("latin-1").decode("utf-8")
    except (UnicodeDecodeError, UnicodeEncodeError):
        return result


def _parse_path_line(line: str, repo_root: Path) -> tuple[str, Path | None]:
    if line.startswith("--- /dev/null"):
        return "old", None
    if line.startswith("+++ /dev/null"):
        return "new", None
    if line.startswith("--- a/"):
        rel_path = line.removeprefix("--- a/").strip()
        resolved = (repo_root / rel_path).resolve()
        if not resolved.is_relative_to(repo_root.resolve()):
            return "", None
        return "old", repo_root / rel_path
    if line.startswith("+++ b/"):
        rel_path = line.removeprefix("+++ b/").strip()
        resolved = (repo_root / rel_path).resolve()
        if not resolved.is_relative_to(repo_root.resolve()):
            return "", None
        return "new", repo_root / rel_path
    if line.startswith('--- "a/'):
        rel_path = _unquote_c_style(line.removeprefix("--- ").strip()).removeprefix("a/")
        resolved = (repo_root / rel_path).resolve()
        if not resolved.is_relative_to(repo_root.resolve()):
            return "", None
        return "old", repo_root / rel_path
    if line.startswith('+++ "b/'):
        rel_path = _unquote_c_style(line.removeprefix("+++ ").strip()).removeprefix("b/")
        resolved = (repo_root / rel_path).resolve()
        if not resolved.is_relative_to(repo_root.resolve()):
            return "", None
        return "new", repo_root / rel_path
    return "", None


def parse_diff(repo_root: Path, diff_range: str) -> list[DiffHunk]:
    _validate_diff_range(diff_range)
    output = run_git(repo_root, ["diff", *_SAFE_DIFF_FLAGS, "--unified=0", "-M", diff_range])
    hunks: list[DiffHunk] = []
    old_path: Path | None = None
    new_path: Path | None = None

    for line in output.splitlines():
        path_type, path = _parse_path_line(line, repo_root)
        if path_type == "old":
            old_path = path
            continue
        if path_type == "new":
            new_path = path
            continue

        match = _HUNK_RE.match(line)
        if match:
            current_path = new_path if new_path else old_path
            if current_path:
                hunks.append(_parse_hunk_header(match, current_path))

    return hunks


def _run_git_z(repo_root: Path, args: list[str]) -> list[str]:
    output = run_git(repo_root, args)
    return [p for p in output.split("\0") if p]


def get_changed_files(repo_root: Path, diff_range: str) -> list[Path]:
    _validate_diff_range(diff_range)
    return [repo_root / p for p in _run_git_z(repo_root, ["diff", *_SAFE_DIFF_FLAGS, "--name-only", "-M", "-z", diff_range])]


def split_diff_range(diff_range: str) -> tuple[str | None, str | None]:
    m = _RANGE_RE.match(diff_range)
    if not m:
        return (None, None)
    base = m.group(1).strip()
    head = m.group(3).strip()
    return (base or None, head or None)


def get_untracked_files(repo_root: Path) -> list[Path]:
    return [repo_root / p for p in _run_git_z(repo_root, ["ls-files", "--others", "--exclude-standard", "-z"])]


def get_deleted_files(repo_root: Path, diff_range: str) -> set[Path]:
    _validate_diff_range(diff_range)
    return {
        (repo_root / p).resolve()
        for p in _run_git_z(repo_root, ["diff", *_SAFE_DIFF_FLAGS, "--diff-filter=D", "--name-only", "-M", "-z", diff_range])
    }


def get_renamed_paths(repo_root: Path, diff_range: str, min_similarity: int = 95) -> tuple[set[Path], set[Path]]:
    _validate_diff_range(diff_range)
    output = run_git(repo_root, ["diff", *_SAFE_DIFF_FLAGS, "--diff-filter=R", "--name-status", "-M", "-z", diff_range])
    parts = output.split("\0")
    old_paths: set[Path] = set()
    pure_new_paths: set[Path] = set()
    i = 0
    while i < len(parts):
        if parts[i].startswith("R"):
            try:
                sim = int(parts[i][1:])
            except ValueError:
                sim = 0
            if i + 1 < len(parts) and parts[i + 1]:
                old_paths.add((repo_root / parts[i + 1]).resolve())
            if sim >= min_similarity and i + 2 < len(parts) and parts[i + 2]:
                pure_new_paths.add((repo_root / parts[i + 2]).resolve())
            i += 3
        else:
            i += 1
    return old_paths, pure_new_paths


def show_file_at_revision(repo_root: Path, rev: str, rel_path: Path) -> str:
    spec = f"{rev}:{rel_path.as_posix()}"
    return run_git(repo_root, ["show", spec])


class CatFileBatch:
    def __init__(self, repo_root: Path) -> None:
        self._proc: subprocess.Popen[bytes] | None = None
        self._repo_root = repo_root

    def _ensure_started(self) -> subprocess.Popen[bytes]:
        if self._proc is None or self._proc.poll() is not None:
            self._proc = subprocess.Popen(
                ["git", "-C", str(self._repo_root), "cat-file", "--batch"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
        return self._proc

    def get(self, rev: str, rel_path: Path) -> str:
        spec = f"{rev}:{rel_path.as_posix()}\n"
        proc = self._ensure_started()
        assert proc.stdin is not None and proc.stdout is not None
        proc.stdin.write(spec.encode())
        proc.stdin.flush()

        header = proc.stdout.readline()
        if not header:
            raise GitError(f"cat-file: unexpected EOF for {spec.strip()}")

        header_str = header.decode("utf-8", errors="replace").strip()
        if header_str.endswith("missing"):
            raise GitError(f"Path not found: {spec.strip()}")

        parts = header_str.split()
        if len(parts) < 3:
            raise GitError(f"cat-file: malformed header: {header_str}")

        size = int(parts[2])
        content = proc.stdout.read(size)
        proc.stdout.read(1)  # trailing LF

        return content.decode("utf-8", errors="replace")

    def close(self) -> None:
        if self._proc is not None and self._proc.poll() is None:
            assert self._proc.stdin is not None
            self._proc.stdin.close()
            try:
                self._proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._proc.kill()
                self._proc.wait()
            self._proc = None

    def __enter__(self) -> CatFileBatch:
        return self

    def __exit__(
        self,
        _exc_type: type[BaseException] | None,
        _exc_val: BaseException | None,
        _exc_tb: TracebackType | None,
    ) -> None:
        self.close()
