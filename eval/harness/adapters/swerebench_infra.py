"""SWE-rebench V2 infra slice: instances whose fix touches both code and infrastructure.

Why this slice exists: the cross-artifact links diffctx is meant to draw (an
env name read in code and set in a manifest, an image built by CI and consumed
by a chart) have no oracle in the other corpora. A real fix that edits a code
file and an infra file together is a co-change record: weak for precision (a
version bump rides along with a bug fix), but real, at scale, and not written
by us.

## Rule (frozen; the manifest records it and the build reproduces it)

An instance qualifies when its gold `patch` *modifies* (the file exists at the
base commit) at least one code file and at least one infra file. Two directions
are scored separately, each using only one side's hunks as the change:

- `code_to_infra`: seed = every code-side block applied as one commit, hidden
  gold = the modified infra files.
- `infra_to_code`: seed = every infra-side block, hidden gold = the modified
  code files.

Blocks that are neither (docs, tests, lock files, data) are applied on neither
side and are not gold. Hidden gold counts only files that exist at base, so no
retrieval could be asked for a file the repository does not hold yet.

## Licence

SWE-rebench V2 is CC-BY-4.0. The committed manifest holds instance ids, the
pinned dataset revision, the rule, counts and our own labels; no patch text.
"""

from __future__ import annotations

import hashlib
import re
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

HF_PATH = "nebius/SWE-rebench-V2"
DATASET_REVISION = "10483de0f50fe5da545942705a76c6150171af7f"  # pragma: allowlist secret
PARQUET_PATH = "data/train-00000-of-00001.parquet"
RULE_VERSION = 1
MAX_SAMPLE = 200
MAX_PER_REPO = 4
MAX_SEED_FILES = 25
LABEL_SAMPLE = 30

CODE_SUFFIXES = frozenset(
    {
        ".py", ".pyi", ".go", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".rs", ".java", ".php", ".kt", ".kts",
        ".jl", ".ex", ".exs", ".scala", ".swift", ".dart", ".c", ".h", ".cc", ".cpp", ".hpp", ".cxx", ".cs", ".r",
        ".clj", ".cljs", ".ml", ".mli", ".lua", ".rb", ".vue", ".svelte",
    }
)  # fmt: skip
_TEST_SEGMENTS = frozenset({"test", "tests", "__tests__", "spec", "specs", "testdata", "fixtures", "e2e"})
_K8S_SEGMENTS = frozenset({"k8s", "kubernetes", "manifests", "deploy", "deployment", "deployments", "kustomize", "overlays"})
_HELM_SEGMENTS = frozenset({"charts", "helm"})
_YAML_SUFFIXES = (".yaml", ".yml")
_ENV_TEMPLATE = re.compile(
    r"^(\.env(\..+)?|.+\.env(\.(example|sample|template|dist))?|env\.(example|sample|template))$", re.IGNORECASE
)
_COMPOSE = re.compile(r"^(docker-)?compose([.-].+)?\.ya?ml$", re.IGNORECASE)
_CI_NAMES = frozenset(
    {".gitlab-ci.yml", ".travis.yml", "jenkinsfile", "azure-pipelines.yml", "cloudbuild.yaml", "cloudbuild.yml"}
)
_K8S_CONTENT = re.compile(r"^[ +\-]?\s*apiVersion:\s*\S+", re.MULTILINE)
_K8S_KIND = re.compile(r"^[ +\-]?\s*kind:\s*\S+", re.MULTILINE)

_BLOCK_SPLIT = re.compile(r"(?=^diff --git )", re.MULTILINE)
_HEADER_PATHS = re.compile(r"^diff --git a/(.+?) b/(.+)$")


@dataclass(frozen=True)
class Block:
    old_path: str
    new_path: str
    status: str  # "modified" | "added" | "deleted" | "renamed"
    text: str

    @property
    def path(self) -> str:
        return self.old_path if self.status == "deleted" else self.new_path


def split_blocks(patch: str) -> list[Block]:
    out: list[Block] = []
    for raw in _BLOCK_SPLIT.split(patch):
        if not raw.startswith("diff --git "):
            continue
        head = raw.split("\n", 1)[0]
        m = _HEADER_PATHS.match(head)
        if not m:
            continue
        old, new = m.group(1), m.group(2)
        if "\nnew file mode" in raw:
            status = "added"
        elif "\ndeleted file mode" in raw:
            status = "deleted"
        elif "\nrename from " in raw or old != new:
            status = "renamed"
        else:
            status = "modified"
        out.append(Block(old, new, status, raw))
    return out


def classify_block(block: Block) -> str:
    """`infra`, `code` or `other` for one patch block."""
    path = block.path
    parts = path.split("/")
    name = parts[-1]
    lower = name.lower()
    dirs = {p.lower() for p in parts[:-1]}

    if lower.startswith("dockerfile") or lower.endswith(".dockerfile") or lower == "containerfile":
        return "infra"
    if lower.endswith((".tf", ".tfvars", ".hcl")):
        return "infra"
    if _ENV_TEMPLATE.match(name):
        return "infra"
    if lower in _CI_NAMES or (lower == "config.yml" and ".circleci" in parts):
        return "infra"
    if ".github" in parts and "workflows" in parts and lower.endswith(_YAML_SUFFIXES):
        return "infra"
    if lower.endswith(_YAML_SUFFIXES):
        if _COMPOSE.match(name):
            return "infra"
        if lower == "chart.yaml" or (dirs & _HELM_SEGMENTS):
            return "infra"
        if dirs & _K8S_SEGMENTS or (_K8S_CONTENT.search(block.text) and _K8S_KIND.search(block.text)):
            return "infra"
        return "other"
    if lower.endswith(".tpl") and "templates" in dirs:
        return "infra"

    suffix = "." + lower.rsplit(".", 1)[-1] if "." in lower else ""
    if suffix in CODE_SUFFIXES:
        if (
            dirs & _TEST_SEGMENTS
            or lower.startswith("test_")
            or re.search(r"[._-](test|spec)\.", lower)
            or lower.endswith("_test.go")
        ):
            return "other"
        return "code"
    return "other"


def infra_kind(path: str) -> str:
    """Coarse label of an infra file, for the manifest's counts."""
    lower = path.lower().rsplit("/", 1)[-1]
    if lower.startswith("dockerfile") or lower.endswith(".dockerfile") or lower == "containerfile":
        return "dockerfile"
    if lower.endswith((".tf", ".tfvars", ".hcl")):
        return "terraform"
    if _ENV_TEMPLATE.match(path.rsplit("/", 1)[-1]):
        return "env_template"
    if ".github/workflows/" in path or lower in _CI_NAMES or ".circleci/" in path or lower == "jenkinsfile":
        return "ci"
    if _COMPOSE.match(path.rsplit("/", 1)[-1]):
        return "compose"
    if lower == "chart.yaml" or "/charts/" in "/" + path or "/helm/" in "/" + path or lower.endswith(".tpl"):
        return "helm"
    return "k8s"


@dataclass(frozen=True)
class SliceEntry:
    instance_id: str
    repo: str
    base_commit: str
    language: str
    code_seed: tuple[str, ...]  # code-side files in the patch (any status)
    infra_seed: tuple[str, ...]
    code_hidden: tuple[str, ...]  # modified (exist at base) code files
    infra_hidden: tuple[str, ...]
    infra_kinds: tuple[str, ...]

    def as_manifest_row(self) -> dict[str, Any]:
        return {
            "instance_id": self.instance_id,
            "repo": self.repo,
            "base_commit": self.base_commit,
            "language": self.language,
            "code_files": list(self.code_hidden),
            "infra_files": list(self.infra_hidden),
            "infra_kinds": list(self.infra_kinds),
        }


def qualify(row: dict[str, Any]) -> SliceEntry | None:
    """The slice rule applied to one dataset row; None when it does not qualify."""
    patch = row.get("patch") or ""
    code_seed: list[str] = []
    infra_seed: list[str] = []
    code_mod: list[str] = []
    infra_mod: list[str] = []
    for block in split_blocks(patch):
        side = classify_block(block)
        if side == "other":
            continue
        (code_seed if side == "code" else infra_seed).append(block.path)
        if block.status == "modified":
            (code_mod if side == "code" else infra_mod).append(block.path)
    if not code_mod or not infra_mod:
        return None
    if len(code_seed) > MAX_SEED_FILES or len(infra_seed) > MAX_SEED_FILES:
        return None
    return SliceEntry(
        instance_id=row["instance_id"],
        repo=row["repo"],
        base_commit=row["base_commit"],
        language=row["language"],
        code_seed=tuple(sorted(code_seed)),
        infra_seed=tuple(sorted(infra_seed)),
        code_hidden=tuple(sorted(code_mod)),
        infra_hidden=tuple(sorted(infra_mod)),
        infra_kinds=tuple(sorted({infra_kind(p) for p in infra_mod})),
    )


def seed_patch(patch: str, side: str) -> str:
    """The patch restricted to the blocks of one side (`code` or `infra`)."""
    return "".join(b.text if b.text.endswith("\n") else b.text + "\n" for b in split_blocks(patch) if classify_block(b) == side)


def _rank_key(instance_id: str, salt: str) -> str:
    return hashlib.sha256(f"{salt}:{instance_id}".encode()).hexdigest()


_CLASS_PRIORITY = ("terraform", "env_template", "compose", "dockerfile", "helm", "k8s", "ci")
CI_POOL_CAP = 50


def infra_class(entry: SliceEntry) -> str:
    """The rarest infra kind among the instance's modified infra files.

    CI workflow edits are 80% of the slice and the weakest evidence of a
    key link (a version bump rides along with any fix), so an instance that
    also touches a Dockerfile, chart, manifest, compose or env file is drawn
    under that kind and CI-only instances are capped as one stratum.
    """
    return next(k for k in _CLASS_PRIORITY if k in entry.infra_kinds)


def _language_round_robin(items: list[SliceEntry], max_per_repo: int) -> list[SliceEntry]:
    by_lang: dict[str, list[SliceEntry]] = defaultdict(list)
    seen: Counter[str] = Counter()
    for item in sorted(items, key=lambda e: _rank_key(e.instance_id, "sample")):
        if seen[item.repo] < max_per_repo:
            seen[item.repo] += 1
            by_lang[item.language].append(item)
    order = sorted(by_lang, key=lambda lg: (-len(by_lang[lg]), lg))
    out: list[SliceEntry] = []
    while any(by_lang.values()):
        for lang in order:
            if by_lang[lang]:
                out.append(by_lang[lang].pop(0))
    return out


def draw_sample(entries: list[SliceEntry], limit: int = MAX_SAMPLE, max_per_repo: int = MAX_PER_REPO) -> list[SliceEntry]:
    """Deterministic stratified sample: infra-class strata, languages spread inside.

    A stratum is read in language round-robin over sha256 order, at most
    `max_per_repo` per repository (also across strata) so one monorepo cannot be a whole stratum;
    the CI-only stratum is cut to `CI_POOL_CAP`. The quota is three per stratum
    plus the rest proportional to what each holds (largest remainder).
    """
    by_class: dict[str, list[SliceEntry]] = defaultdict(list)
    for entry in entries:
        by_class[infra_class(entry)].append(entry)
    pools = {k: _language_round_robin(v, max_per_repo) for k, v in by_class.items()}
    if "ci" in pools:
        pools["ci"] = pools["ci"][:CI_POOL_CAP]

    if sum(len(p) for p in pools.values()) <= limit:
        take = {k: len(p) for k, p in pools.items()}
    else:
        floor = {k: min(len(p), 3) for k, p in pools.items()}
        room = {k: len(p) - floor[k] for k, p in pools.items()}
        share = {k: (limit - sum(floor.values())) * room[k] / sum(room.values()) for k in pools}
        take = {k: floor[k] + int(share[k]) for k in pools}
        for k in sorted(pools, key=lambda key: (-(share[key] - int(share[key])), key)):
            if sum(take.values()) >= limit:
                break
            take[k] += 1
    # The repo cap holds across strata: rarest stratum first, skipping a repo that is full.
    chosen: list[SliceEntry] = []
    per_repo: Counter[str] = Counter()
    for k in sorted(pools, key=_CLASS_PRIORITY.index):
        taken = 0
        for item in pools[k]:
            if taken == take[k]:
                break
            if per_repo[item.repo] < max_per_repo:
                per_repo[item.repo] += 1
                chosen.append(item)
                taken += 1
    return sorted(chosen, key=lambda e: e.instance_id)


def draw_label_sample(sample: list[SliceEntry], n: int = LABEL_SAMPLE) -> list[SliceEntry]:
    """Stratified 30 for the blind hand labels: round-robin over infra classes in hash order."""
    by_class: dict[str, list[SliceEntry]] = defaultdict(list)
    for entry in sorted(sample, key=lambda e: _rank_key(e.instance_id, "labels")):
        by_class[infra_class(entry)].append(entry)
    order = sorted(by_class, key=lambda k: (-len(by_class[k]), k))
    picked: list[SliceEntry] = []
    while len(picked) < n and any(by_class.values()):
        for k in order:
            if by_class[k] and len(picked) < n:
                picked.append(by_class[k].pop(0))
    return sorted(picked, key=lambda e: e.instance_id)


def load_slice(parquet: Path) -> list[SliceEntry]:
    """Read only the columns the rule needs, one row group at a time."""
    import pyarrow.parquet as pq

    pf = pq.ParquetFile(parquet)
    columns = ["instance_id", "repo", "base_commit", "language", "patch"]
    entries: list[SliceEntry] = []
    for i in range(pf.num_row_groups):
        for row in pf.read_row_group(i, columns=columns).to_pylist():
            entry = qualify(row)
            if entry is not None:
                entries.append(entry)
    return entries


def count_by(entries: list[SliceEntry]) -> dict[str, Any]:
    return {
        "total": len(entries),
        "by_language": dict(sorted(Counter(e.language for e in entries).items())),
        "by_infra_kind": dict(sorted(Counter(k for e in entries for k in e.infra_kinds).items())),
        "by_stratum": dict(sorted(Counter(infra_class(e) for e in entries).items())),
        "repos": len({e.repo for e in entries}),
    }


PUBLICATION_RULE = "instances whose paths trip the repository's public-content gate are replaced"


def build_manifest(parquet: Path, excluded: frozenset[str] | set[str] = frozenset()) -> dict[str, Any]:
    import pyarrow.parquet as pq

    total_rows = pq.ParquetFile(parquet).metadata.num_rows
    entries = load_slice(parquet)
    sample = draw_sample([e for e in entries if e.instance_id not in excluded])
    labelled = draw_label_sample(sample)
    return {
        "dataset": HF_PATH,
        "revision": DATASET_REVISION,
        "license": "CC-BY-4.0",
        "rule_version": RULE_VERSION,
        "rule": (
            "gold patch modifies (exists at base) >=1 code file and >=1 infra file; "
            f"<= {MAX_SEED_FILES} seed files per side; sample <= {MAX_SAMPLE}: infra-class strata "
            f"(rarest kind first), CI-only cut to {CI_POOL_CAP}, floor 3, proportional remainder, "
            f"language round-robin, <= {MAX_PER_REPO} per repo, sha256-ordered; {PUBLICATION_RULE}"
        ),
        "dataset_rows": total_rows,
        "qualifying": count_by(entries),
        "publication_exclusions": {"rule": PUBLICATION_RULE, "instance_ids": sorted(excluded)},
        "sample": count_by(sample),
        "label_sample_ids": [e.instance_id for e in labelled],
        "instances": [e.as_manifest_row() for e in sample],
    }
