"""Git access for the oracle: bare mirrors, exported trees, hunks. Hooks and filters never run."""

from __future__ import annotations

import os
import re
import subprocess
import tarfile
from dataclasses import dataclass
from pathlib import Path

HARDENING = ("-c", "core.hooksPath=/dev/null", "-c", "protocol.allow=never", "-c", "protocol.https.allow=always")
HUNK_HEADER = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def git_env() -> dict[str, str]:
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": "/nonexistent",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_SYSTEM": "/dev/null",
        "GIT_TERMINAL_PROMPT": "0",
        "LC_ALL": "C",
    }


def git(*args: str, cwd: Path | None = None, git_dir: Path | None = None, local: bool = False) -> str:
    base = ["git", *HARDENING, *(("-c", "protocol.file.allow=always") if local else ())]
    if git_dir is not None:
        base += ["--git-dir", str(git_dir)]
    result = subprocess.run([*base, *args], cwd=cwd, env=git_env(), capture_output=True, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:3])} failed: {result.stderr.decode(errors='replace').strip()}")
    return result.stdout.decode(errors="replace")


def mirror(url: str, dest: Path, sha: str) -> Path:
    """A bare clone holding `sha`; fetched again only when the commit is missing."""
    if not dest.exists():
        dest.parent.mkdir(parents=True, exist_ok=True)
        git("clone", "--bare", "--quiet", url, str(dest))
    try:
        git("cat-file", "-e", f"{sha}^{{commit}}", git_dir=dest)
    except RuntimeError:
        git("fetch", "--quiet", "origin", sha, git_dir=dest)
    return dest


def export_tree(git_dir: Path, sha: str, dest: Path) -> Path:
    """The files of `sha` without a checkout: no hooks, no filters, no symlink out of `dest`."""
    if dest.exists():
        return dest
    partial = dest.with_name(dest.name + ".partial")
    partial.mkdir(parents=True, exist_ok=True)
    with subprocess.Popen(
        ["git", *HARDENING, "--git-dir", str(git_dir), "archive", "--format=tar", sha],
        env=git_env(),
        stdout=subprocess.PIPE,
    ) as proc:
        assert proc.stdout is not None
        with tarfile.open(fileobj=proc.stdout, mode="r|") as tar:
            tar.extractall(partial, filter="data")
        if proc.wait() != 0:
            raise RuntimeError(f"git archive {sha} failed")
    partial.rename(dest)
    return dest


def checkout(work: Path, git_dir: Path, sha: str) -> Path:
    """A non-bare clone of the mirror at `sha`, for running the tool under test."""
    if not work.exists():
        git("clone", "--quiet", "--no-checkout", str(git_dir), str(work), local=True)
    git("checkout", "--quiet", "--force", "--detach", sha, cwd=work)
    git("clean", "--quiet", "-fdx", cwd=work)
    return work


def parent_of(git_dir: Path, sha: str) -> str | None:
    parents = git("rev-list", "--parents", "-n", "1", sha, git_dir=git_dir).split()[1:]
    return parents[0] if len(parents) == 1 else None


@dataclass(frozen=True)
class Hunk:
    old_start: int
    old_count: int
    new_start: int
    new_count: int

    @property
    def old_lines(self) -> range:
        return range(self.old_start, self.old_start + self.old_count)

    @property
    def new_lines(self) -> range:
        return range(self.new_start, self.new_start + self.new_count)


@dataclass(frozen=True)
class FileChange:
    old_path: str | None
    new_path: str | None
    hunks: tuple[Hunk, ...]

    @property
    def status(self) -> str:
        if self.old_path is None:
            return "A"
        if self.new_path is None:
            return "D"
        return "M" if self.old_path == self.new_path else "R"


def diff_changes(git_dir: Path, base: str, head: str) -> list[FileChange]:
    text = git("-c", "core.quotePath=false", "diff", "-U0", "-M", "--no-color", "--no-ext-diff", base, head, git_dir=git_dir)
    return parse_diff(text)


def parse_diff(text: str) -> list[FileChange]:
    changes: list[FileChange] = []
    old: str | None = None
    new: str | None = None
    hunks: list[Hunk] = []
    in_hunks = False

    def flush() -> None:
        if old is not None or new is not None:
            changes.append(FileChange(old, new, tuple(hunks)))

    for line in text.splitlines():
        if line.startswith("diff --git "):
            flush()
            old, new, hunks, in_hunks = None, None, [], False
        elif line.startswith("@@"):
            in_hunks = True
            match = HUNK_HEADER.match(line)
            if match:
                a, b, c, d = match.groups()
                hunks.append(Hunk(int(a), 1 if b is None else int(b), int(c), 1 if d is None else int(d)))
        elif in_hunks:
            continue
        elif line.startswith("rename from "):
            old = line[len("rename from ") :]
        elif line.startswith("rename to "):
            new = line[len("rename to ") :]
        elif line.startswith("--- "):
            old = None if line[4:] == "/dev/null" else _header_path(line[4:], "a/")
        elif line.startswith("+++ "):
            new = None if line[4:] == "/dev/null" else _header_path(line[4:], "b/")
    flush()
    return changes


def _header_path(raw: str, prefix: str) -> str:
    raw = raw.split("\t")[0]
    return raw[len(prefix) :] if raw.startswith(prefix) else raw


def commits_with_paths(git_dir: Path, sha: str, history: int) -> list[tuple[str, list[str]]]:
    """Single-parent commits back from `sha`, newest first, with the paths each one touches."""
    args = ("-c", "core.quotePath=false", "log", "--no-merges", "-M", "--name-only", "--format=@%H %P")
    text = git(*args, f"--max-count={history}", sha, git_dir=git_dir)
    commits: list[tuple[str, list[str]]] = []
    current: tuple[str, list[str]] | None = None
    for line in text.splitlines():
        if line.startswith("@"):
            fields = line[1:].split()
            current = (fields[0], []) if len(fields) == 2 else None
            if current:
                commits.append(current)
        elif line and current:
            current[1].append(line)
    return commits
