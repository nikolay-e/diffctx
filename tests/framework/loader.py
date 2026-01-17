from __future__ import annotations

from pathlib import Path

import yaml

from tests.framework.types import YamlTestCase


def _parse_yaml_test(data: dict, source_file: Path | None = None) -> YamlTestCase:
    initial = data.get("initial", data.get("initial_files", {}))
    changed = data.get("changed", data.get("changed_files", {}))

    assertions = data.get("assertions", {})
    must_include = assertions.get("must_include", data.get("must_include", []))
    must_not_include = assertions.get("must_not_include", data.get("must_not_include", []))

    options = data.get("options", {})

    return YamlTestCase(
        name=data["name"],
        initial_files=initial,
        changed_files=changed,
        must_include=must_include,
        must_not_include=must_not_include,
        commit_message=options.get("commit_message", data.get("commit_message", "Update files")),
        overhead_ratio=options.get("overhead_ratio", data.get("overhead_ratio", 0.3)),
        min_budget=options.get("min_budget", data.get("min_budget")),
        add_garbage_files=options.get("add_garbage", data.get("add_garbage_files", True)),
        skip_garbage_check=options.get("skip_garbage_check", data.get("skip_garbage_check", False)),
        source_file=source_file,
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
        return [_parse_yaml_test(content, yaml_path)]

    return []


def load_test_cases_from_dir(cases_dir: Path, pattern: str = "**/*.yaml") -> list[YamlTestCase]:
    cases = []
    for yaml_file in sorted(cases_dir.glob(pattern)):
        cases.extend(load_test_cases(yaml_file))
    for yaml_file in sorted(cases_dir.glob(pattern.replace(".yaml", ".yml"))):
        cases.extend(load_test_cases(yaml_file))
    return cases
