from __future__ import annotations

import re
from pathlib import Path

import yaml

from tests.framework.types import YamlTestCase


def _parse_yaml_test(data: dict, source_file: Path | None = None) -> YamlTestCase:
    initial = data.get("initial", data.get("initial_files", {}))
    changed = data.get("changed", data.get("changed_files", {}))

    assertions = data.get("assertions", {})
    must_include = assertions.get("must_include", data.get("must_include", []))
    must_include_files = assertions.get("must_include_files", data.get("must_include_files", []))
    must_include_content = assertions.get("must_include_content", data.get("must_include_content", []))
    must_not_include = assertions.get("must_not_include", data.get("must_not_include", []))

    options = data.get("options", {})

    return YamlTestCase(
        name=data["name"],
        initial_files=initial,
        changed_files=changed,
        must_include=must_include,
        must_include_files=must_include_files,
        must_include_content=must_include_content,
        must_not_include=must_not_include,
        commit_message=options.get("commit_message", data.get("commit_message", "Update files")),
        overhead_ratio=options.get("overhead_ratio", data.get("overhead_ratio", 0.3)),
        min_budget=options.get("min_budget", data.get("min_budget")),
        add_garbage_files=options.get("add_garbage", data.get("add_garbage_files", True)),
        skip_garbage_check=options.get("skip_garbage_check", data.get("skip_garbage_check", False)),
        source_file=source_file,
    )


def _load_triplet(before_path: Path) -> YamlTestCase | None:
    stem = before_path.stem
    base_name = re.sub(r"_before$", "", stem)
    parent = before_path.parent

    after_path = parent / f"{base_name}_after.yaml"
    diffctx_path = parent / f"{base_name}_diffctx.yaml"

    if not after_path.exists() or not diffctx_path.exists():
        return None

    with before_path.open("r", encoding="utf-8") as f:
        initial_files = yaml.safe_load(f) or {}

    with after_path.open("r", encoding="utf-8") as f:
        changed_files = yaml.safe_load(f) or {}

    with diffctx_path.open("r", encoding="utf-8") as f:
        diffctx = yaml.safe_load(f) or {}

    must_include = diffctx.get("must_include", [])
    must_include_files = diffctx.get("must_include_files", [])
    must_include_content = [s.rstrip("\n") for s in diffctx.get("must_include_content", [])]
    must_not_include = diffctx.get("must_not_include", [])

    return YamlTestCase(
        name=base_name,
        initial_files=initial_files,
        changed_files=changed_files,
        must_include=must_include,
        must_include_files=must_include_files,
        must_include_content=must_include_content,
        must_not_include=must_not_include,
        add_garbage_files=False,
        skip_garbage_check=True,
        source_file=diffctx_path,
    )


def load_test_cases(yaml_path: Path) -> list[YamlTestCase]:
    with yaml_path.open("r", encoding="utf-8") as f:
        content = yaml.safe_load(f)

    if content is None:
        return []

    if isinstance(content, list):
        return [_parse_yaml_test(item, yaml_path) for item in content]

    if isinstance(content, dict):
        if "tests" in content:
            return [_parse_yaml_test(item, yaml_path) for item in content["tests"]]
        if "name" in content:
            return [_parse_yaml_test(content, yaml_path)]

    return []


def _is_triplet_part(yaml_file: Path, seen: set[Path]) -> bool:
    if yaml_file in seen:
        return True
    return yaml_file.stem.endswith(("_after", "_diffctx"))


def load_test_cases_from_dir(cases_dir: Path, pattern: str = "**/*.yaml") -> list[YamlTestCase]:
    cases: list[YamlTestCase] = []
    seen_triplet_bases: set[Path] = set()

    for yaml_file in sorted(cases_dir.glob(pattern)):
        if not yaml_file.stem.endswith("_before"):
            continue
        triplet_case = _load_triplet(yaml_file)
        if triplet_case:
            seen_triplet_bases.add(yaml_file)
            base_name = re.sub(r"_before$", "", yaml_file.stem)
            seen_triplet_bases.add(yaml_file.parent / f"{base_name}_after.yaml")
            seen_triplet_bases.add(yaml_file.parent / f"{base_name}_diffctx.yaml")
            cases.append(triplet_case)

    for yaml_file in sorted(cases_dir.glob(pattern)):
        if not _is_triplet_part(yaml_file, seen_triplet_bases):
            cases.extend(load_test_cases(yaml_file))

    yml_pattern = pattern.replace(".yaml", ".yml")
    for yaml_file in sorted(cases_dir.glob(yml_pattern)):
        if yaml_file in seen_triplet_bases:
            continue
        if yaml_file.stem.endswith(("_before", "_after", "_diffctx")):
            continue
        cases.extend(load_test_cases(yaml_file))

    return cases
