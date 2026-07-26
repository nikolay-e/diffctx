"""First-party dcbench adapter for the versioned in-repository corpus.

Fully offline: patches and annotations live in-tree, repositories resolve to
local pinned clones under test-repos/.
"""

from __future__ import annotations

import subprocess
from collections.abc import Iterator
from pathlib import Path

import yaml

from eval.harness.adapters.base import BenchmarkAdapter, BenchmarkInstance, extract_patch_files

REPO_ROOT = Path(__file__).resolve().parents[3]
DCBENCH_ROOT = REPO_ROOT / "datasets" / "dcbench" / "v1"

_LANG_FROM_EXTENSION = {
    "py": "python",
    "rs": "rust",
    "go": "go",
    "js": "javascript",
    "jsx": "javascript",
    "ts": "typescript",
    "tsx": "typescript",
    "java": "java",
    "kt": "kotlin",
    "scala": "scala",
    "rb": "ruby",
    "php": "php",
    "ex": "elixir",
    "exs": "elixir",
    "erl": "erlang",
    "c": "c",
    "h": "c",
    "cc": "cpp",
    "cpp": "cpp",
    "hpp": "cpp",
    "m": "objc",
    "mm": "objc",
    "swift": "swift",
    "lua": "lua",
    "pl": "perl",
    "hs": "haskell",
    "zig": "zig",
    "clj": "clojure",
    "dart": "dart",
    "sql": "sql",
    "tf": "hcl",
    "groovy": "groovy",
}


def _infer_language(files: frozenset[str]) -> str:
    counts: dict[str, int] = {}
    for f in files:
        ext = f.rsplit(".", 1)[-1].lower() if "." in f else ""
        lang = _LANG_FROM_EXTENSION.get(ext)
        if lang:
            counts[lang] = counts.get(lang, 0) + 1
    return max(counts, key=lambda k: counts[k]) if counts else "unknown"


class DcbenchAdapter(BenchmarkAdapter):
    name = "dcbench"

    def __init__(self, root: Path = DCBENCH_ROOT, repos_root: Path | None = None, annotated_only: bool = True) -> None:
        self.root = root
        self.repos_root = repos_root or REPO_ROOT / "test-repos"
        self.annotated_only = annotated_only
        self._repos = yaml.safe_load((root / "repos.yaml").read_text())

    def dataset_revision(self) -> str:
        r = subprocess.run(
            ["git", "-C", str(self.root), "log", "-1", "--format=%H", "--", "instances"],
            capture_output=True,
            text=True,
        )
        sha = r.stdout.strip() if r.returncode == 0 else ""
        return f"dcbench@{sha[:12] or 'worktree'}"

    def _load_raw(self) -> Iterator[dict]:
        for inst_dir in sorted((self.root / "instances").iterdir()):
            ann_path = inst_dir / "annotation.yaml"
            if not ann_path.exists():
                continue
            row = yaml.safe_load(ann_path.read_text())
            row["_dir"] = inst_dir
            yield row

    def _patch_text(self, row: dict) -> str | None:
        patch_file = row["_dir"] / "patch.diff"
        if patch_file.exists():
            return patch_file.read_text(errors="replace")
        repo = self.repos_root / row["repo"]
        r = subprocess.run(
            ["git", "-C", str(repo), "format-patch", "-1", "--stdout", "--no-signature", row["commit"]],
            capture_output=True,
            text=True,
        )
        return r.stdout if r.returncode == 0 and r.stdout else None

    def _normalize(self, row: dict) -> BenchmarkInstance | None:
        annotated = row.get("annotator") not in (None, "pending")
        if self.annotated_only and not annotated:
            return None
        patch = self._patch_text(row)
        if not patch or not patch.strip():
            return None
        patch_files = extract_patch_files(patch)
        gold_paths = frozenset(g["path"] for g in row.get("gold") or [])
        gold_files = gold_paths if annotated and gold_paths else patch_files
        if not gold_files:
            return None
        repo_dir = self.repos_root / row["repo"]
        return BenchmarkInstance(
            instance_id=f"dcbench::{row['_dir'].name}",
            source_benchmark=self.name,
            repo=f"dcbench/{row['repo']}",
            base_commit=row["base_commit"],
            gold_patch=patch,
            gold_files=gold_files,
            language=_infer_language(patch_files),
            gold_fragments=None,
            edit_scope=len(patch_files),
            extra={
                "repo_url": str(repo_dir.resolve()),
                "annotated": annotated,
                "nontrivial_gold_count": row.get("nontrivial_gold_count", 0),
                "forbidden_files": sorted(f["path"] for f in row.get("forbidden") or []),
            },
        )
