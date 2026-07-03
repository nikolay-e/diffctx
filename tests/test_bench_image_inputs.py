"""Guards against the failure mode that silently killed every full sweep for
seven weeks: files referenced by Dockerfile.bench / build_bench_image.sh were
deleted (a9e2ad83) and the image reference was renamed without republishing,
so provisioning died inside cloud-init with no CI signal.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
STDLIB_OR_LOCAL = {
    "benchmarks",
    "diffctx",
    "aider",  # launched via `uv tool run`, not pip-installed
    "select",
    "the",  # false positive from doc lines
}
IMPORT_TO_DIST = {
    "rank_bm25": "rank-bm25",
    "huggingface_hub": "huggingface-hub",
}


def _dockerfile() -> str:
    path = REPO_ROOT / "Dockerfile.bench"
    assert path.exists(), "Dockerfile.bench referenced by scripts/build_bench_image.sh and bench-sweep.yml"
    return path.read_text()


def test_dockerfile_bench_copy_sources_exist():
    missing = []
    for line in _dockerfile().splitlines():
        parts = line.split()
        if len(parts) < 3 or parts[0] != "COPY" or parts[1].startswith("--from"):
            continue
        for src in parts[1:-1]:
            if not (REPO_ROOT / src).exists():
                missing.append(src)
    assert not missing, f"Dockerfile.bench COPY sources missing from repo: {missing}"


def test_requirements_bench_covers_benchmark_imports():
    import sys

    reqs_path = REPO_ROOT / "requirements-bench.txt"
    assert reqs_path.exists(), "requirements-bench.txt referenced by Dockerfile.bench"
    declared = {
        re.split(r"[><=\[;]", line.strip())[0].lower().replace("-", "_")
        for line in reqs_path.read_text().splitlines()
        if line.strip() and not line.startswith("#")
    }

    import_re = re.compile(r"^\s*(?:import|from)\s+(\w+)", re.MULTILINE)
    stdlib = set(sys.stdlib_module_names)
    needed = set()
    for py in (REPO_ROOT / "benchmarks").rglob("*.py"):
        for m in import_re.finditer(py.read_text()):
            mod = m.group(1)
            if mod in stdlib or mod in STDLIB_OR_LOCAL:
                continue
            needed.add(IMPORT_TO_DIST.get(mod, mod).replace("-", "_"))

    missing = needed - declared
    assert not missing, f"benchmarks/ imports not covered by requirements-bench.txt: {sorted(missing)}"


def test_bench_sweep_workflow_references_existing_bake_script():
    workflow = (REPO_ROOT / ".github" / "workflows" / "bench-sweep.yml").read_text()
    for script in re.findall(r"/app/(scripts/\S+\.py)", workflow):
        assert (REPO_ROOT / script).exists(), f"bench-sweep.yml runs {script} inside the image but it is not in the repo"
