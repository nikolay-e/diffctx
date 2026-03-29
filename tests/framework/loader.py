from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

from tests.framework.types import YamlTestCase

_VALID_TOP_LEVEL_KEYS = frozenset(
    {
        "name",
        "initial",
        "initial_files",
        "changed",
        "changed_files",
        "assertions",
        "options",
        "must_include",
        "must_include_files",
        "must_include_content",
        "must_not_include",
        "must_include_content_from",
        "must_not_include_files",
        "must_not_include_content_from",
        "max_fragments",
        "max_files",
        "max_fragments_per_file",
        "max_enrichment",
        "min_recall",
        "max_noise_rate",
        "max_context_tokens",
        "commit_message",
        "min_budget",
        "add_garbage_files",
        "skip_garbage_check",
        "xfail",
        "min_score",
        "strict",
        "tests",
    }
)

_VALID_ASSERTION_KEYS = frozenset(
    {
        "must_include",
        "must_include_files",
        "must_include_content",
        "must_not_include",
        "must_include_content_from",
        "must_not_include_files",
        "must_not_include_content_from",
    }
)

_VALID_OPTION_KEYS = frozenset(
    {
        "max_fragments",
        "max_files",
        "max_fragments_per_file",
        "max_enrichment",
        "min_recall",
        "max_noise_rate",
        "max_context_tokens",
        "commit_message",
        "min_budget",
        "add_garbage",
        "skip_garbage_check",
        "min_score",
    }
)


def _validate_keys(data: dict, source_file: Path | None) -> None:
    unknown = set(data.keys()) - _VALID_TOP_LEVEL_KEYS
    if unknown:
        location = str(source_file) if source_file else "unknown"
        raise ValueError(f"Unknown keys in test case ({location}): {sorted(unknown)}")

    assertions = data.get("assertions", {})
    if isinstance(assertions, dict):
        unknown_a = set(assertions.keys()) - _VALID_ASSERTION_KEYS
        if unknown_a:
            location = str(source_file) if source_file else "unknown"
            raise ValueError(f"Unknown assertion keys ({location}): {sorted(unknown_a)}")

    options = data.get("options", {})
    if isinstance(options, dict):
        unknown_o = set(options.keys()) - _VALID_OPTION_KEYS
        if unknown_o:
            location = str(source_file) if source_file else "unknown"
            raise ValueError(f"Unknown option keys ({location}): {sorted(unknown_o)}")


def _normalize_snippet(snippet: Any) -> str:
    if isinstance(snippet, str):
        return snippet.rstrip("\n")
    if isinstance(snippet, bool):
        return ("true" if snippet else "false").rstrip("\n")
    if snippet is None:
        return "null"
    if isinstance(snippet, (int, float)):
        return str(snippet).rstrip("\n")
    if isinstance(snippet, dict) and len(snippet) == 1:
        key, value = next(iter(snippet.items()))
        return f"{_normalize_snippet(key)}: {_normalize_snippet(value)}".rstrip("\n")
    raise TypeError(f"Unsupported assertion snippet type: {type(snippet).__name__}")


def _normalize_snippet_list(values: Any) -> list[str]:
    if values is None:
        return []
    if isinstance(values, list):
        raw_items = values
    else:
        raw_items = [values]
    return [_normalize_snippet(item) for item in raw_items]


def _normalize_content_from(value: Any) -> dict[str, list[str]]:
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise TypeError(f"must_include_content_from must be a mapping, got {type(value).__name__}")
    return {str(path): _normalize_snippet_list(snippets) for path, snippets in value.items()}


def _parse_yaml_test(data: dict, source_file: Path | None = None) -> YamlTestCase:
    _validate_keys(data, source_file)
    name = data.get("name", source_file.stem if source_file else "unnamed")
    initial_files = data.get("initial", data.get("initial_files", {}))
    changed_files = data.get("changed", data.get("changed_files", {}))

    assertions = data.get("assertions", {})

    must_include = assertions.get("must_include", data.get("must_include", []))
    must_include_files = assertions.get("must_include_files", data.get("must_include_files", []))
    must_not_include = assertions.get("must_not_include", data.get("must_not_include", []))

    raw_content = assertions.get("must_include_content", data.get("must_include_content", []))
    must_include_content = _normalize_snippet_list(raw_content)

    raw_content_from = assertions.get("must_include_content_from", data.get("must_include_content_from", {}))
    must_include_content_from = _normalize_content_from(raw_content_from)

    must_not_include_files = assertions.get("must_not_include_files", data.get("must_not_include_files", []))

    raw_not_content_from = assertions.get("must_not_include_content_from", data.get("must_not_include_content_from", {}))
    must_not_include_content_from = _normalize_content_from(raw_not_content_from)

    options = data.get("options", {})

    return YamlTestCase(
        name=name,
        initial_files=initial_files,
        changed_files=changed_files,
        must_include=must_include,
        must_include_files=must_include_files,
        must_include_content=must_include_content,
        must_not_include=must_not_include,
        must_include_content_from=must_include_content_from,
        must_not_include_files=must_not_include_files,
        must_not_include_content_from=must_not_include_content_from,
        max_fragments=options.get("max_fragments", data.get("max_fragments")),
        max_files=options.get("max_files", data.get("max_files")),
        max_fragments_per_file=options.get("max_fragments_per_file", data.get("max_fragments_per_file")),
        max_enrichment=options.get("max_enrichment", data.get("max_enrichment")),
        min_recall=options.get("min_recall", data.get("min_recall")),
        max_noise_rate=options.get("max_noise_rate", data.get("max_noise_rate")),
        max_context_tokens=options.get("max_context_tokens", data.get("max_context_tokens")),
        commit_message=options.get("commit_message", data.get("commit_message", "Update files")),
        min_budget=options.get("min_budget", data.get("min_budget")),
        add_garbage_files=options.get("add_garbage", data.get("add_garbage_files", True)),
        skip_garbage_check=options.get("skip_garbage_check", data.get("skip_garbage_check", False)),
        xfail=data.get("xfail"),
        min_score=options.get("min_score", data.get("min_score")),
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
        if "name" in content or "initial" in content:
            return [_parse_yaml_test(content, yaml_path)]

    return []


def load_test_cases_from_dir(cases_dir: Path, pattern: str = "**/*.yaml") -> list[YamlTestCase]:
    cases: list[YamlTestCase] = []
    for yaml_file in sorted(cases_dir.glob(pattern)):
        cases.extend(load_test_cases(yaml_file))
    return cases
