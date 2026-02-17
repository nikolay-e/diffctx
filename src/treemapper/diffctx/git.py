from __future__ import annotations

import re
import subprocess
from pathlib import Path

from .types import DiffHunk

_HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")
_RANGE_RE = re.compile(r"^\s*(\S+?)(\.\.\.?)(\S+?)\s*$")  # NOSONAR(S5852)


class GitError(Exception):
    pass


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
    except FileNotFoundError as e:
        raise GitError("git is not installed or not in PATH") from e


def is_git_repo(path: Path) -> bool:
    try:
        run_git(path, ["rev-parse", "--git-dir"])
        return True
    except GitError:
        return False


def get_diff_text(repo_root: Path, diff_range: str) -> str:
    return run_git(repo_root, ["diff", diff_range])


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


def _parse_path_line(line: str, repo_root: Path) -> tuple[str, Path | None]:
    if line.startswith("--- a/"):
        return "old", repo_root / line.removeprefix("--- a/").strip()
    if line.startswith("--- /dev/null"):
        return "old", None
    if line.startswith("+++ b/"):
        return "new", repo_root / line.removeprefix("+++ b/").strip()
    if line.startswith("+++ /dev/null"):
        return "new", None
    return "", None


def parse_diff(repo_root: Path, diff_range: str) -> list[DiffHunk]:
    output = run_git(repo_root, ["diff", "--unified=0", diff_range])
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


def get_changed_files(repo_root: Path, diff_range: str) -> list[Path]:
    output = run_git(repo_root, ["diff", "--name-only", diff_range])
    files: list[Path] = []
    for line in output.splitlines():
        line = line.strip()
        if line:
            files.append(repo_root / line)
    return files


def split_diff_range(diff_range: str) -> tuple[str | None, str | None]:
    m = _RANGE_RE.match(diff_range)
    if not m:
        return (None, None)
    base = m.group(1).strip()
    head = m.group(3).strip()
    return (base or None, head or None)


def get_untracked_files(repo_root: Path) -> list[Path]:
    output = run_git(repo_root, ["ls-files", "--others", "--exclude-standard"])
    return [repo_root / line.strip() for line in output.splitlines() if line.strip()]


def show_file_at_revision(repo_root: Path, rev: str, rel_path: Path) -> str:
    spec = f"{rev}:{rel_path.as_posix()}"
    return run_git(repo_root, ["show", spec])
