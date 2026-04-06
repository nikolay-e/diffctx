from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

from tests.framework.types import (
    Accept,
    DeclaredFragment,
    Fixtures,
    Oracle,
    Selector,
    XFailInfo,
    YamlTestCase,
)

_VALID_TOP_LEVEL_KEYS = frozenset(
    {
        "name",
        "tags",
        "repo",
        "fixtures",
        "fragments",
        "oracle",
        "accept",
        "xfail",
        "tests",
    }
)

_VALID_REPO_KEYS = frozenset({"initial_files", "changed_files", "commit_message"})
_VALID_FIXTURES_KEYS = frozenset({"auto_garbage", "distractors"})
_VALID_SELECTOR_KEYS = frozenset({"path", "symbol", "kind", "anchor", "any_of"})
_VALID_ORACLE_KEYS = frozenset({"required", "allowed", "forbidden"})
_VALID_ACCEPT_KEYS = frozenset({"symbol_match", "kind_must_match", "span_relation"})
_VALID_XFAIL_KEYS = frozenset({"category", "reason", "issue"})
_VALID_FRAGMENT_KEYS = frozenset({"id", "selector"})


def _validate_keys(data: dict, valid: frozenset, context: str) -> None:
    unknown = set(data.keys()) - valid
    if unknown:
        raise ValueError(f"Unknown keys in {context}: {sorted(unknown)}")


def _parse_selector(data: Any) -> Selector:
    if data is None:
        return Selector()
    if not isinstance(data, dict):
        raise TypeError(f"selector must be a dict, got {type(data).__name__}")
    _validate_keys(data, _VALID_SELECTOR_KEYS, "selector")
    any_of_raw = data.get("any_of")
    any_of = [_parse_selector(s) for s in any_of_raw] if any_of_raw else None
    return Selector(
        path=data.get("path"),
        symbol=data.get("symbol"),
        kind=data.get("kind"),
        anchor=str(data["anchor"]) if data.get("anchor") is not None else None,
        any_of=any_of,
    )


def _parse_fragment(data: Any) -> DeclaredFragment:
    if not isinstance(data, dict):
        raise TypeError(f"fragment must be a dict, got {type(data).__name__}")
    _validate_keys(data, _VALID_FRAGMENT_KEYS, "fragment")
    if "id" not in data:
        raise ValueError("Fragment missing required 'id' field")
    return DeclaredFragment(
        id=str(data["id"]),
        selector=_parse_selector(data.get("selector") or {}),
    )


def _parse_oracle(data: Any) -> Oracle:
    if data is None:
        return Oracle()
    if not isinstance(data, dict):
        raise TypeError(f"oracle must be a dict, got {type(data).__name__}")
    _validate_keys(data, _VALID_ORACLE_KEYS, "oracle")
    return Oracle(
        required=[str(x) for x in (data.get("required") or [])],
        allowed=[str(x) for x in (data.get("allowed") or [])],
        forbidden=[str(x) for x in (data.get("forbidden") or [])],
    )


def _parse_accept(data: Any) -> Accept:
    if data is None:
        return Accept()
    if not isinstance(data, dict):
        raise TypeError(f"accept must be a dict, got {type(data).__name__}")
    _validate_keys(data, _VALID_ACCEPT_KEYS, "accept")
    return Accept(
        symbol_match=str(data.get("symbol_match", "exact")),
        kind_must_match=bool(data.get("kind_must_match", False)),
        span_relation=str(data.get("span_relation", "exact_or_enclosing")),
    )


def _parse_fixtures(data: Any) -> Fixtures:
    if data is None:
        return Fixtures()
    if not isinstance(data, dict):
        raise TypeError(f"fixtures must be a dict, got {type(data).__name__}")
    _validate_keys(data, _VALID_FIXTURES_KEYS, "fixtures")
    return Fixtures(
        auto_garbage=bool(data.get("auto_garbage", False)),
        distractors=dict(data.get("distractors") or {}),
    )


def _parse_xfail(data: Any) -> XFailInfo:
    if data is None:
        return XFailInfo()
    if isinstance(data, str):
        return XFailInfo(reason=data if data else None)
    if not isinstance(data, dict):
        raise TypeError(f"xfail must be a dict or string, got {type(data).__name__}")
    _validate_keys(data, _VALID_XFAIL_KEYS, "xfail")
    category = data.get("category")
    reason = data.get("reason")
    issue = data.get("issue")
    return XFailInfo(
        category=str(category) if category is not None else None,
        reason=str(reason) if reason is not None else None,
        issue=str(issue) if issue is not None else None,
    )


def _parse_yaml_test(data: dict, source_file: Path | None = None) -> YamlTestCase:
    _validate_keys(data, _VALID_TOP_LEVEL_KEYS, f"test case ({source_file or 'unknown'})")

    name = data.get("name", source_file.stem if source_file else "unnamed")
    tags = [str(t) for t in (data.get("tags") or [])]

    repo = data.get("repo") or {}
    if repo:
        _validate_keys(repo, _VALID_REPO_KEYS, "repo")
    initial_files = dict(repo.get("initial_files") or {})
    changed_files = dict(repo.get("changed_files") or {})
    commit_message = str(repo.get("commit_message", "Update files"))

    fragments_raw = data.get("fragments") or []
    fragments = [_parse_fragment(f) for f in fragments_raw]

    return YamlTestCase(
        name=name,
        initial_files=initial_files,
        changed_files=changed_files,
        fragments=fragments,
        oracle=_parse_oracle(data.get("oracle")),
        tags=tags,
        commit_message=commit_message,
        fixtures=_parse_fixtures(data.get("fixtures")),
        accept=_parse_accept(data.get("accept")),
        xfail=_parse_xfail(data.get("xfail")),
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
    cases: list[YamlTestCase] = []
    for yaml_file in sorted(cases_dir.glob(pattern)):
        cases.extend(load_test_cases(yaml_file))
    return cases
