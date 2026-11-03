"""What each indexer needs before and while it runs, and the least trust that lets it run.

Python and TypeScript only: no `pip` call, no lifecycle scripts, nothing of the repository's own code runs.
Rust, Go and Kotlin plans are gone with their repositories; each returns with its cell (wave 5).
"""

from __future__ import annotations

import json
import re
import subprocess
from dataclasses import dataclass
from pathlib import Path

import corpus as corpus_config

TOOLS = corpus_config.ROOT / "tools"


@dataclass
class Plan:
    command: list[str] | None
    env: dict[str, str]
    blocked: str = ""


def _succeeds(command: list[str], cwd: Path, env: dict[str, str], timeout: int) -> bool:
    try:
        return subprocess.run(command, cwd=cwd, env=env, capture_output=True, timeout=timeout, check=False).returncode == 0
    except subprocess.TimeoutExpired:
        return False


def plan_python(ctx: Context) -> Plan:
    # A repository's own `[tool.pyright] include` narrows what scip-python reads (click's old pins keep 24 of 70 files);
    # a pyrightconfig.json outranks it, so the oracle indexes every file.
    (ctx.tree / "pyrightconfig.json").write_text(json.dumps({"include": ["."]}), encoding="utf-8")
    environment = ctx.home / "environment.json"
    environment.write_text("[]", encoding="utf-8")  # no pip call: nothing of the repository's environment runs
    command = [
        str(TOOLS / "node_modules" / ".bin" / "scip-python"),
        "index",
        ".",
        f"--project-name={ctx.repo.name}",
        f"--project-version={ctx.sha}",
        f"--output={ctx.out}",
        f"--environment={environment}",
        "--quiet",
    ]
    return Plan(command, ctx.env)


COVERING_OPTIONS = {"allowJs": True, "noEmit": True, "skipLibCheck": True}
RESOLUTION_OPTIONS = {"module": "esnext", "moduleResolution": "bundler"}
REPO_CONFIG = "tsconfig.repo.json"


def _read_config(config: Path) -> dict:
    """The tsconfig as JSON; comments on their own lines are tolerated, anything else unreadable is empty."""
    try:
        return json.loads(re.sub(r"^\s*//.*$", "", config.read_text(encoding="utf-8"), flags=re.MULTILINE))
    except (OSError, ValueError):
        return {}


def _is_solution_style(data: dict) -> bool:
    """A tsconfig that only lists `references` compiles no file itself: scip-typescript then reads just the projects it names."""
    return data.get("files") == [] and "include" not in data and bool(data.get("references"))


def cover_everything(tree: Path) -> None:
    """Index every script file: put a root tsconfig over the repository's that includes them all.

    The repository's own `include`, `exclude` and `references` decide what scip-typescript reads, and they leave
    out benchmarks, workspace packages and configs (hono: a third of its files). `--infer-tsconfig` does not help:
    it picks the JavaScript-aware config only when no `.ts` file turns up in its first thousand paths, and the
    installed `node_modules` always holds `.d.ts` files, so a JavaScript repository was indexed with a config
    that ignores JavaScript (express: "no files got indexed"). The oracle's config extends the repository's, so
    module resolution, `paths` and `jsx` stay, and the repository's file is kept as tsconfig.repo.json.
    """
    config = tree / "tsconfig.json"
    options = dict(COVERING_OPTIONS)
    body: dict = {"oracleCovering": True, "include": ["**/*"], "exclude": ["**/node_modules"]}
    if config.exists():
        existing = _read_config(config)
        if existing.get("oracleCovering"):
            return
        config.rename(tree / REPO_CONFIG)
        if _is_solution_style(existing):
            options.update(RESOLUTION_OPTIONS)
        else:
            body["extends"] = f"./{REPO_CONFIG}"
    else:
        options.update(RESOLUTION_OPTIONS)
    body["compilerOptions"] = options
    config.write_text(json.dumps(body), encoding="utf-8")


def plan_typescript(ctx: Context) -> Plan:
    if ctx.install and (ctx.tree / "package.json").exists():
        flags = ["--ignore-scripts", "--no-audit", "--no-fund", "--no-package-lock"]
        _succeeds(["npm", "install", *flags], ctx.tree, ctx.env, ctx.timeout)
    cover_everything(ctx.tree)
    return Plan([str(TOOLS / "node_modules" / ".bin" / "scip-typescript"), "index", f"--output={ctx.out}"], ctx.env)


@dataclass
class Context:
    corpus: corpus_config.Corpus
    repo: corpus_config.Repo
    sha: str
    tree: Path
    out: Path
    home: Path
    env: dict[str, str]
    install: bool
    timeout: int


PLANNERS = {
    "python": plan_python,
    "typescript": plan_typescript,
}
