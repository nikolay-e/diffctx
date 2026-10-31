"""The corpus, the sampling parameters and the pinned indexers."""

from __future__ import annotations

import json
import platform
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
LANGUAGES = ("python", "typescript")
EXTENSIONS = {
    "python": (".py",),
    "typescript": (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"),
}
INDEXERS = {"python": "@sourcegraph/scip-python", "typescript": "@sourcegraph/scip-typescript"}


@dataclass(frozen=True)
class Repo:
    name: str
    url: str
    language: str
    sha: str
    license: str
    fast: bool = False
    flavor: str = ""


@dataclass(frozen=True)
class Sampling:
    seed: int
    commits: int
    fast_commits: int
    history: int
    max_code_files: int
    definitions_per_repo: int


@dataclass(frozen=True)
class Corpus:
    repos: tuple[Repo, ...]
    sampling: Sampling
    min_file_ratio: float
    index_seconds: int
    diffctx_seconds: int
    scip_version: str
    scip_url: str
    scip_sha256: dict[str, str]
    slots: dict[str, dict]

    def select(self, names: list[str] | None = None, fast: bool = False) -> list[Repo]:
        chosen = [r for r in self.repos if not names or r.name in names]
        if names and len(chosen) != len(set(names)):
            known = ", ".join(r.name for r in self.repos)
            raise SystemExit(f"unknown repository in {names}; the corpus has: {known}")
        return [r for r in chosen if r.fast] if fast and not names else chosen


def load_corpus(path: Path = ROOT / "corpus.toml") -> Corpus:
    raw = tomllib.loads(path.read_text(encoding="utf-8"))
    repos = tuple(Repo(**entry) for entry in raw["repo"])
    for repo in repos:
        if repo.language not in LANGUAGES:
            raise ValueError(f"{repo.name}: language {repo.language!r} has no indexer yet; use a [slots] entry")
        if len(repo.sha) != 40:
            raise ValueError(f"{repo.name}: sha must be a full 40-character commit id")
    s = raw["sampling"]
    scip = raw["scip_cli"]
    return Corpus(
        repos=repos,
        sampling=Sampling(
            seed=s["seed"],
            commits=s["commits"],
            fast_commits=s["fast_commits"],
            history=s["history"],
            max_code_files=s["max_code_files"],
            definitions_per_repo=s["definitions_per_repo"],
        ),
        min_file_ratio=raw["completeness"]["min_file_ratio"],
        index_seconds=raw["timeouts"]["index_seconds"],
        diffctx_seconds=raw["timeouts"]["diffctx_seconds"],
        scip_version=scip["version"],
        scip_url=scip["url"],
        scip_sha256=scip["sha256"],
        slots=raw.get("slots", {}),
    )


def indexer_versions(package_json: Path = ROOT / "tools" / "package.json") -> dict[str, str]:
    """Exact versions of the npm indexers; a range is refused because the cache keys on the version."""
    deps = json.loads(package_json.read_text(encoding="utf-8"))["dependencies"]
    for name, version in deps.items():
        if not version[:1].isdigit():
            raise ValueError(f"{name} is pinned as {version!r}; an exact version is required")
    return deps


def scip_platform() -> str:
    system = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system())
    machine = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "amd64", "AMD64": "amd64"}.get(platform.machine())
    if system is None or machine is None:
        raise RuntimeError(f"no scip CLI build for {platform.system()} {platform.machine()}")
    return f"{system}-{machine}"


def is_code_path(language: str, path: str) -> bool:
    return path.endswith(EXTENSIONS[language])
