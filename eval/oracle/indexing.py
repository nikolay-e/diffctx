"""Isolated SCIP indexing: scrubbed environment, no secrets, lifecycle scripts off, results cached.

The cache key is (repository, commit, indexer, indexer version). A failed or incomplete index is recorded,
counted and excluded; it is never read as an empty answer.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import tarfile
import tempfile
import time
import urllib.request
from dataclasses import asdict, dataclass, field
from pathlib import Path

import corpus as corpus_config
from scipindex import Index, read_index

CACHE = corpus_config.ROOT / "cache"
TOOLS = corpus_config.ROOT / "tools"
EXCLUDED_PARTS = ("node_modules", ".git", "dist", "build", "vendor", ".venv", "site-packages", "__pycache__")


@dataclass
class Completeness:
    files_present: int = 0
    files_indexed: int = 0
    definitions: int = 0
    references: int = 0
    references_to_indexed_definitions: int = 0
    lint_ok: bool | None = None

    @property
    def file_ratio(self) -> float:
        return 1.0 if self.files_present == 0 else self.files_indexed / self.files_present

    @property
    def resolved_ratio(self) -> float:
        return 1.0 if self.references == 0 else self.references_to_indexed_definitions / self.references


@dataclass
class IndexResult:
    ok: bool
    reason: str
    path: Path | None
    indexer: str
    version: str
    seconds: float = 0.0
    completeness: Completeness = field(default_factory=Completeness)

    def summary(self) -> dict:
        data = asdict(self)
        data["path"] = None if self.path is None else str(self.path)
        data["completeness"]["file_ratio"] = round(self.completeness.file_ratio, 4)
        data["completeness"]["resolved_ratio"] = round(self.completeness.resolved_ratio, 4)
        return data


def scrubbed_env(home: Path) -> dict[str, str]:
    """What an indexer or an installer sees: no tokens, no user config, a throwaway home."""
    (home / "tmp").mkdir(parents=True, exist_ok=True)
    for name in ("user.npmrc", "global.npmrc"):  # two files: npm refuses one path loaded as both levels
        (home / name).write_text("", encoding="utf-8")
    return {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(home),
        "TMPDIR": str(home / "tmp"),
        "LANG": "C.UTF-8",
        "CI": "1",
        "NO_COLOR": "1",
        "NODE_OPTIONS": "--max-old-space-size=8192",
        "npm_config_ignore_scripts": "true",
        "npm_config_audit": "false",
        "npm_config_fund": "false",
        "npm_config_update_notifier": "false",
        "npm_config_userconfig": str(home / "user.npmrc"),
        "npm_config_globalconfig": str(home / "global.npmrc"),
    }


def present_code_files(tree: Path, language: str) -> set[str]:
    found: set[str] = set()
    for path in tree.rglob("*"):
        relative = path.relative_to(tree)
        if any(part in EXCLUDED_PARTS for part in relative.parts) or not path.is_file() or path.is_symlink():
            continue
        if corpus_config.is_code_path(language, relative.as_posix()):
            found.add(relative.as_posix())
    return found


def measure_completeness(index: Index, tree: Path, language: str) -> Completeness:
    present = present_code_files(tree, language)
    indexed = [d for d in index.documents if d.path in present]
    defined = {o.symbol for d in index.documents for o in d.occurrences if o.is_definition}
    result = Completeness(files_present=len(present), files_indexed=len(indexed))
    for doc in indexed:
        for occ in doc.occurrences:
            if occ.symbol.startswith("local ") or not occ.symbol:
                continue
            if occ.is_definition:
                result.definitions += 1
            else:
                result.references += 1
                result.references_to_indexed_definitions += occ.symbol in defined
    return result


def _command(language: str, tree: Path, out: Path, sha: str, repo: str, home: Path) -> list[str]:
    bin_dir = TOOLS / "node_modules" / ".bin"
    if language == "python":
        environment = home / "environment.json"
        environment.write_text("[]", encoding="utf-8")  # no pip call: nothing of the repository's environment runs
        return [
            str(bin_dir / "scip-python"),
            "index",
            ".",
            f"--project-name={repo}",
            f"--project-version={sha}",
            f"--output={out}",
            f"--environment={environment}",
            "--quiet",
        ]
    command = [str(bin_dir / "scip-typescript"), "index", f"--output={out}"]
    if not (tree / "tsconfig.json").exists():
        command.append("--infer-tsconfig")
    return command


def _install_dependencies(tree: Path, env: dict[str, str], timeout: int) -> bool:
    if not (tree / "package.json").exists():
        return False
    flags = ["--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock"]
    done = subprocess.run(["npm", "install", *flags], cwd=tree, env=env, capture_output=True, timeout=timeout, check=False)
    return done.returncode == 0


def _scip_cli() -> Path | None:
    binary = CACHE / "bin" / "scip"
    return binary if binary.exists() else None


def _lint(index_path: Path, env: dict[str, str]) -> bool | None:
    scip = _scip_cli()
    if scip is None:
        return None
    return subprocess.run([str(scip), "lint", str(index_path)], env=env, capture_output=True, check=False).returncode == 0


def index_snapshot(
    corpus: corpus_config.Corpus,
    repo: corpus_config.Repo,
    sha: str,
    tree: Path,
    *,
    install: bool = True,
    retry_failed: bool = False,
) -> IndexResult:
    package = corpus_config.INDEXERS[repo.language]
    version = corpus_config.indexer_versions()[package]
    slot = CACHE / "index" / repo.name / sha / f"{package.rsplit('/', 1)[-1]}-{version}"
    meta = slot / "meta.json"
    if meta.exists():
        cached = json.loads(meta.read_text(encoding="utf-8"))
        if cached["ok"] or not retry_failed:
            return _from_meta(cached, slot, package, version)
    slot.mkdir(parents=True, exist_ok=True)
    out = slot / "index.scip"
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="oracle-home-") as home_dir:
        home = Path(home_dir)
        env = scrubbed_env(home)
        result = _run_indexer(corpus, repo, sha, tree, out, home, env, install)
        result.seconds = round(time.monotonic() - started, 1)
        if result.ok:
            index = read_index(out)
            result.completeness = measure_completeness(index, tree, repo.language)
            result.completeness.lint_ok = _lint(out, env)
            if result.completeness.file_ratio < corpus.min_file_ratio:
                result.ok, result.reason = False, "incomplete"
    result.indexer, result.version = package, version
    result.path = out if out.exists() else None
    meta.write_text(json.dumps(result.summary(), indent=2), encoding="utf-8")
    return result


def _run_indexer(
    corpus: corpus_config.Corpus,
    repo: corpus_config.Repo,
    sha: str,
    tree: Path,
    out: Path,
    home: Path,
    env: dict[str, str],
    install: bool,
) -> IndexResult:
    result = IndexResult(ok=False, reason="", path=None, indexer="", version="")
    if repo.language == "typescript" and install:
        _install_dependencies(tree, env, corpus.index_seconds)
    command = _command(repo.language, tree, out, sha, repo.name, home)
    try:
        done = subprocess.run(command, cwd=tree, env=env, capture_output=True, timeout=corpus.index_seconds, check=False)
    except subprocess.TimeoutExpired:
        result.reason = "timeout"
        return result
    if done.returncode != 0 or not out.exists():
        tail = done.stderr.decode(errors="replace")[-400:].strip().replace("\n", " | ")
        result.reason = f"indexer exited {done.returncode}: {tail}"
        return result
    result.ok, result.reason = True, "ok"
    return result


def _from_meta(meta: dict, slot: Path, package: str, version: str) -> IndexResult:
    stored = meta["completeness"]
    completeness = Completeness(**{k: stored[k] for k in Completeness.__dataclass_fields__})
    path = slot / "index.scip"
    return IndexResult(
        meta["ok"], meta["reason"], path if path.exists() else None, package, version, meta["seconds"], completeness
    )


def setup_tools(corpus: corpus_config.Corpus) -> None:
    """Install the pinned npm indexers without lifecycle scripts, and the scip CLI after its checksum matches."""
    with tempfile.TemporaryDirectory(prefix="oracle-home-") as home:
        env = scrubbed_env(Path(home))
        subprocess.run(["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund"], cwd=TOOLS, env=env, check=True)
    platform = corpus_config.scip_platform()
    expected = corpus.scip_sha256[platform]
    target = CACHE / "bin" / "scip"
    if target.exists():
        return
    url = corpus.scip_url.format(version=corpus.scip_version, platform=platform)
    if not url.startswith("https://"):
        raise ValueError(f"refusing a non-https download: {url}")
    with tempfile.TemporaryDirectory(prefix="oracle-scip-") as scratch:
        archive = Path(scratch) / "scip.tar.gz"
        with urllib.request.urlopen(url, timeout=120) as response:
            archive.write_bytes(response.read())
        actual = hashlib.sha256(archive.read_bytes()).hexdigest()
        if actual != expected:
            raise RuntimeError(f"scip CLI checksum mismatch for {platform}: expected {expected}, got {actual}")
        with tarfile.open(archive) as tar:
            tar.extractall(Path(scratch) / "out", filter="data")
        unpacked = next(p for p in (Path(scratch) / "out").rglob("scip") if p.is_file())
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.move(str(unpacked), target)
        target.chmod(0o755)
