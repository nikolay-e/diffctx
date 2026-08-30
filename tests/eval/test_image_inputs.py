"""Guards against the failure mode that silently killed every full sweep for
seven weeks: files referenced by Dockerfile.eval / build_eval_image.sh were
deleted (a9e2ad83) and the image reference was renamed without republishing,
so provisioning died inside cloud-init with no CI signal.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
STDLIB_OR_LOCAL = {
    "eval",
    "diffctx",
    "aider",  # launched via `uv tool run`, not pip-installed
    "select",
    "the",  # false positive from doc lines
}
IMPORT_TO_DIST = {
    "rank_bm25": "rank-bm25",
    "huggingface_hub": "huggingface-hub",
    "yaml": "pyyaml",
}


def _dockerfile() -> str:
    path = REPO_ROOT / "Dockerfile.eval"
    assert path.exists(), "Dockerfile.eval referenced by scripts/build_eval_image.sh and eval-sweep.yml"
    return path.read_text()


def test_dockerfile_eval_copy_sources_exist():
    missing = []
    for line in _dockerfile().splitlines():
        parts = line.split()
        if len(parts) < 3 or parts[0] != "COPY" or parts[1].startswith("--from"):
            continue
        for src in parts[1:-1]:
            if not (REPO_ROOT / src).exists():
                missing.append(src)
    assert not missing, f"Dockerfile.eval COPY sources missing from repo: {missing}"


def _eval_group_names() -> set[str]:
    text = (REPO_ROOT / "pyproject.toml").read_text()
    block = text.split("[dependency-groups]", 1)[1].split("\n[", 1)[0]
    body = block.split("eval = [", 1)[1].split("]", 1)[0]
    return {
        re.split(r"[><=\[;]", item.strip().strip('",'))[0].lower().replace("-", "_")
        for item in body.splitlines()
        if item.strip().startswith('"')
    }


def test_eval_dependency_group_covers_evaluation_imports():
    import sys

    declared = _eval_group_names()
    assert declared, "the `eval` dependency group is what Dockerfile.eval installs"

    import_re = re.compile(r"^\s*(?:import|from)\s+(\w+)", re.MULTILINE)
    stdlib = set(sys.stdlib_module_names)
    needed = set()
    for py in (REPO_ROOT / "eval").rglob("*.py"):
        for m in import_re.finditer(py.read_text()):
            mod = m.group(1)
            if mod in stdlib or mod in STDLIB_OR_LOCAL:
                continue
            needed.add(IMPORT_TO_DIST.get(mod, mod).replace("-", "_"))

    missing = needed - declared
    assert not missing, f"eval/ imports not covered by the `eval` dependency group: {sorted(missing)}"


def test_eval_sweep_workflow_references_existing_bake_script():
    workflow = (REPO_ROOT / ".github" / "workflows" / "eval-sweep.yml").read_text()
    for script in re.findall(r"/app/(scripts/\S+\.py)", workflow):
        assert (REPO_ROOT / script).exists(), f"eval-sweep.yml runs {script} inside the image but it is not in the repo"
