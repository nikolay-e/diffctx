from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

import pygit2


class GitError(Exception):
    """Raised when a git operation fails (test-side mirror of the Rust GitError)."""


@dataclass(frozen=True)
class DiffHunk:
    path: Path
    new_start: int
    new_len: int
    old_start: int = 0
    old_len: int = 0


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
    return "", None


_HUNK_RE = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")

_repo_cache: dict[str, pygit2.Repository] = {}

_SIGNATURE = pygit2.Signature("Test", "test@test.com")


def _get_repo(repo_root: Path) -> pygit2.Repository:
    key = str(repo_root)
    if key not in _repo_cache:
        _repo_cache[key] = pygit2.Repository(str(repo_root))
    return _repo_cache[key]


def clear_repo_cache() -> None:
    _repo_cache.clear()


def _resolve_commit(repo: pygit2.Repository, rev: str) -> pygit2.Commit:
    obj = repo.revparse_single(rev)
    if isinstance(obj, pygit2.Tag):
        obj = obj.peel(pygit2.Commit)
    if isinstance(obj, pygit2.Commit):
        return obj
    raise GitError(f"Cannot resolve {rev} to a commit")


def _is_working_tree_diff(diff_range: str) -> bool:
    return ".." not in diff_range


def _resolve_range(repo: pygit2.Repository, diff_range: str) -> tuple[pygit2.Commit, pygit2.Commit | None]:
    if _is_working_tree_diff(diff_range):
        return _resolve_commit(repo, diff_range), None

    parts = diff_range.split("...")
    if len(parts) == 2:
        base = _resolve_commit(repo, parts[0])
        head = _resolve_commit(repo, parts[1])
        return base, head

    parts = diff_range.split("..")
    if len(parts) == 2:
        base = _resolve_commit(repo, parts[0])
        head = _resolve_commit(repo, parts[1])
        return base, head

    raise GitError(f"Cannot parse diff range: {diff_range}")


def _get_diff(repo: pygit2.Repository, diff_range: str, context_lines: int = 3) -> pygit2.Diff:
    base, head = _resolve_range(repo, diff_range)
    flags = pygit2.GIT_DIFF_PATIENCE
    if _is_working_tree_diff(diff_range):
        repo.index.read()
        diff_index = repo.index.diff_to_tree(base.tree)
        diff_workdir = repo.diff(a=base.tree, flags=flags, context_lines=context_lines)
        diff_index.merge(diff_workdir)
        diff_index.find_similar()
        return diff_index
    else:
        diff = repo.diff(a=base.tree, b=head.tree, flags=flags, context_lines=context_lines)  # type: ignore[arg-type]
        diff.find_similar()
        return diff


def parse_diff(repo_root: Path, diff_range: str) -> list[DiffHunk]:
    repo = _get_repo(repo_root)
    diff = _get_diff(repo, diff_range, context_lines=0)
    patch_text = diff.patch or ""

    hunks: list[DiffHunk] = []
    old_path: Path | None = None
    new_path: Path | None = None

    for line in patch_text.splitlines():
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


def get_diff_text(repo_root: Path, diff_range: str) -> str:
    repo = _get_repo(repo_root)
    diff = _get_diff(repo, diff_range)
    return diff.patch or ""


def get_changed_files(repo_root: Path, diff_range: str) -> list[Path]:
    repo = _get_repo(repo_root)
    diff = _get_diff(repo, diff_range)
    paths: list[Path] = []
    for patch in diff:
        delta = patch.delta
        if delta.new_file.path:
            paths.append(repo_root / delta.new_file.path)
    return paths


def get_deleted_files(repo_root: Path, diff_range: str) -> set[Path]:
    repo = _get_repo(repo_root)
    diff = _get_diff(repo, diff_range)
    result: set[Path] = set()
    for patch in diff:
        delta = patch.delta
        if delta.status == pygit2.GIT_DELTA_DELETED:
            result.add((repo_root / delta.old_file.path).resolve())
    return result


def get_renamed_paths(repo_root: Path, diff_range: str, min_similarity: int = 95) -> tuple[set[Path], set[Path]]:
    repo = _get_repo(repo_root)
    diff = _get_diff(repo, diff_range)
    old_paths: set[Path] = set()
    pure_new_paths: set[Path] = set()
    for patch in diff:
        delta = patch.delta
        if delta.status == pygit2.GIT_DELTA_RENAMED:
            old_paths.add((repo_root / delta.old_file.path).resolve())
            if delta.similarity >= min_similarity:
                pure_new_paths.add((repo_root / delta.new_file.path).resolve())
    return old_paths, pure_new_paths


def get_untracked_files(repo_root: Path) -> list[Path]:
    repo = _get_repo(repo_root)
    result: list[Path] = []
    for filepath, flags in repo.status().items():
        if flags & pygit2.GIT_STATUS_WT_NEW:
            result.append(repo_root / filepath)
    return result


def show_file_at_revision(repo_root: Path, rev: str, rel_path: Path) -> str:
    repo = _get_repo(repo_root)
    commit = _resolve_commit(repo, rev)
    try:
        entry = commit.tree[rel_path.as_posix()]
    except KeyError:
        raise GitError(f"Path {rel_path} not found at revision {rev}")
    blob = repo.get(entry.id)
    if blob is None or not isinstance(blob, pygit2.Blob):
        raise GitError(f"Not a blob: {rel_path} at {rev}")
    return blob.data.decode("utf-8", errors="replace")


def is_git_repo(path: Path) -> bool:
    try:
        pygit2.Repository(str(path))
        return True
    except pygit2.GitError:
        return False


def run_git(repo_root: Path, args: list[str]) -> str:
    raise GitError(
        f"run_git called with args {args} — all git operations should be handled by pygit2 backend. "
        "This indicates a missing pygit2 replacement."
    )


class Pygit2Repo:
    def __init__(self, path: Path) -> None:
        self.path = path
        path.mkdir(parents=True, exist_ok=True)
        self._repo = pygit2.init_repository(str(path))
        self._repo.config["user.name"] = "Test"
        self._repo.config["user.email"] = "test@test.com"
        _repo_cache[str(path)] = self._repo

    def add_file(self, rel_path: str, content: str) -> Path:
        file_path = self.path / rel_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_text(content, encoding="utf-8")
        return file_path

    def add_file_binary(self, rel_path: str, data: bytes) -> Path:
        file_path = self.path / rel_path
        file_path.parent.mkdir(parents=True, exist_ok=True)
        file_path.write_bytes(data)
        return file_path

    def remove_file(self, rel_path: str) -> None:
        file_path = self.path / rel_path
        if file_path.exists():
            file_path.unlink()

    def stage_file(self, rel_path: str) -> None:
        self._repo.index.read()
        self._repo.index.add(rel_path)
        self._repo.index.write()

    def commit(self, message: str) -> str:
        self._repo.index.read()
        self._repo.index.add_all()
        self._repo.index.write()
        tree_oid = self._repo.index.write_tree()

        try:
            parent = self._repo.head.peel(pygit2.Commit)
            parents = [parent.id]
        except pygit2.GitError:
            parents = []

        oid = self._repo.create_commit(
            "refs/heads/main" if not parents else "HEAD",
            _SIGNATURE,
            _SIGNATURE,
            message,
            tree_oid,
            parents,
        )

        if not parents:
            self._repo.set_head(self._repo.references["refs/heads/main"].target)

        return str(oid)
