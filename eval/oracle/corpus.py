"""The corpus, the sampling parameters and the pinned indexers."""

from __future__ import annotations

import json
import platform
import tomllib
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parent
LANGUAGES = ("python", "typescript", "rust", "kotlin", "go")
EXTENSIONS = {
    "python": (".py",),
    "typescript": (".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs"),
    "rust": (".rs",),
    "kotlin": (".kt",),  # `.kts` are build scripts: present, never indexed as sources
    "go": (".go",),
}
INDEXERS = {"python": "@sourcegraph/scip-python", "typescript": "@sourcegraph/scip-typescript"}  # npm packages
INDEXER_TOOLS = {"rust": "rust-analyzer", "kotlin": "scip-java", "go": "scip-go"}  # release binaries, see [tools]
TRIPLES = {
    ("Darwin", "arm64"): "aarch64-apple-darwin",
    ("Darwin", "x86_64"): "x86_64-apple-darwin",
    ("Linux", "aarch64"): "aarch64-unknown-linux-gnu",
    ("Linux", "arm64"): "aarch64-unknown-linux-gnu",
    ("Linux", "x86_64"): "x86_64-unknown-linux-gnu",
}


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
class Tool:
    """A release binary pinned by version and a SHA-256 per platform."""

    name: str
    version: str
    url: str
    platforms: str  # os-arch | triple | any: how the release names its builds
    archive: str  # tar.gz | gz | file
    sha256: dict[str, str]

    def build_name(self) -> str:
        """How this release names the build for this machine; the key into `sha256`."""
        if self.platforms == "any":
            return "any"
        if self.platforms == "triple":
            triple = TRIPLES.get((platform.system(), platform.machine()))
            if triple is None:
                raise RuntimeError(f"no {self.name} build for {platform.system()} {platform.machine()}")
            return triple
        return scip_platform()

    def download_url(self) -> str:
        return self.url.format(version=self.version, platform=self.build_name())


@dataclass(frozen=True)
class Corpus:
    repos: tuple[Repo, ...]
    sampling: Sampling
    min_file_ratio: float
    index_seconds: int
    diffctx_seconds: int
    tools: dict[str, Tool]

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
            raise ValueError(f"{repo.name}: language {repo.language!r} has no indexer")
        if len(repo.sha) != 40:
            raise ValueError(f"{repo.name}: sha must be a full 40-character commit id")
    s = raw["sampling"]
    tools = {name: Tool(name=name, **entry) for name, entry in raw["tools"].items()}
    missing = [tool for tool in INDEXER_TOOLS.values() if tool not in tools]
    if missing:
        raise ValueError(f"corpus.toml pins no release for {missing}")
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
        tools=tools,
    )


def indexer_versions(package_json: Path = ROOT / "tools" / "package.json") -> dict[str, str]:
    """Exact versions of the npm indexers; a range is refused because the cache keys on the version."""
    deps = json.loads(package_json.read_text(encoding="utf-8"))["dependencies"]
    for name, version in deps.items():
        if not version[:1].isdigit():
            raise ValueError(f"{name} is pinned as {version!r}; an exact version is required")
    return deps


def indexer_pin(corpus: Corpus, language: str) -> tuple[str, str]:
    """(name, exact version) of the indexer for a language: what the index cache keys on."""
    if language in INDEXERS:
        package = INDEXERS[language]
        return package.rsplit("/", 1)[-1], indexer_versions()[package]
    tool = corpus.tools[INDEXER_TOOLS[language]]
    return tool.name, tool.version


def scip_platform() -> str:
    system = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system())
    machine = {"arm64": "arm64", "aarch64": "arm64", "x86_64": "amd64", "AMD64": "amd64"}.get(platform.machine())
    if system is None or machine is None:
        raise RuntimeError(f"no release build for {platform.system()} {platform.machine()}")
    return f"{system}-{machine}"


def is_code_path(language: str, path: str) -> bool:
    return path.endswith(EXTENSIONS[language])
