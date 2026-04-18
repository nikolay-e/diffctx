#!/usr/bin/env python3
"""Convert v1 YAML test cases to v3 schema."""

from __future__ import annotations

import re
import sys
from pathlib import Path

import yaml

CASES_DIR = Path(__file__).parent.parent / "tests" / "cases" / "diff"

_V1_KNOWN_KEYS = frozenset(
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


class _LiteralStr(str):
    pass


_YAML_STR_TAG = "tag:yaml.org,2002:str"


def _literal_representer(dumper, data):
    if "\n" in data:
        return dumper.represent_scalar(_YAML_STR_TAG, data, style="|")
    return dumper.represent_scalar(_YAML_STR_TAG, data)


def _none_representer(dumper, _data):
    return dumper.represent_scalar("tag:yaml.org,2002:null", "null")


class _V3Dumper(yaml.Dumper):
    pass


_V3Dumper.add_representer(str, _literal_representer)
_V3Dumper.add_representer(type(None), _none_representer)
_V3Dumper.add_representer(
    _LiteralStr,
    lambda d, s: d.represent_scalar("tag:yaml.org,2002:str", s, style="|"),
)


def _normalize_snippet(val) -> str:
    if isinstance(val, str):
        return val.rstrip("\n")
    if isinstance(val, bool):
        return "true" if val else "false"
    if val is None:
        return "null"
    if isinstance(val, (int, float)):
        return str(val)
    if isinstance(val, dict) and len(val) == 1:
        k, v = next(iter(val.items()))
        return f"{_normalize_snippet(k)}: {_normalize_snippet(v)}"
    return str(val)


def _normalize_snippets(vals) -> list[str]:
    if vals is None:
        return []
    if isinstance(vals, list):
        return [_normalize_snippet(v) for v in vals]
    return [_normalize_snippet(vals)]


def _sanitize_id(text: str) -> str:
    stem = Path(text).stem if "/" in text or "." in text else text
    sanitized = re.sub(r"[^a-zA-Z0-9]+", "_", stem).strip("_").lower()
    return sanitized[:40] if sanitized else "frag"


def _unique(base: str, used: set[str]) -> str:
    if base not in used:
        used.add(base)
        return base
    i = 1
    while f"{base}_{i}" in used:
        i += 1
    result = f"{base}_{i}"
    used.add(result)
    return result


def _parse_v1(data: dict) -> dict:
    assertions = data.get("assertions") or {}
    options = data.get("options") or {}

    initial_files = data.get("initial") or data.get("initial_files") or {}
    changed_files = data.get("changed") or data.get("changed_files") or {}

    must_include = _normalize_snippets(assertions.get("must_include", data.get("must_include", [])))
    must_include_files = list(assertions.get("must_include_files", data.get("must_include_files") or []))
    must_include_content = _normalize_snippets(assertions.get("must_include_content", data.get("must_include_content", [])))
    raw_cf = assertions.get("must_include_content_from", data.get("must_include_content_from")) or {}
    must_include_content_from = {str(p): _normalize_snippets(v) for p, v in raw_cf.items()}

    must_not_include = _normalize_snippets(assertions.get("must_not_include", data.get("must_not_include", [])))
    must_not_include_files = list(assertions.get("must_not_include_files", data.get("must_not_include_files") or []))
    raw_ncf = assertions.get("must_not_include_content_from", data.get("must_not_include_content_from")) or {}
    must_not_include_content_from = {str(p): _normalize_snippets(v) for p, v in raw_ncf.items()}

    commit_message = options.get("commit_message", data.get("commit_message", "Update files"))
    add_garbage = options.get("add_garbage", data.get("add_garbage_files", True))
    xfail = data.get("xfail")

    return {
        "name": data.get("name"),
        "initial_files": initial_files,
        "changed_files": changed_files,
        "must_include": must_include,
        "must_include_files": must_include_files,
        "must_include_content": must_include_content,
        "must_include_content_from": must_include_content_from,
        "must_not_include": must_not_include,
        "must_not_include_files": must_not_include_files,
        "must_not_include_content_from": must_not_include_content_from,
        "commit_message": commit_message,
        "add_garbage": bool(add_garbage),
        "xfail": xfail,
    }


def _build_required_fragments(v1: dict, used_ids: set[str], fragments: list) -> list[str]:
    required_ids: list[str] = []
    paths_with_req_anchors: set[str] = set()
    for path, snippets in v1["must_include_content_from"].items():
        paths_with_req_anchors.add(path)
        base = _sanitize_id(path)
        for snippet in snippets:
            fid = _unique(base, used_ids)
            fragments.append({"id": fid, "selector": {"path": path, "anchor": snippet}})
            required_ids.append(fid)
    for path in v1["must_include_files"]:
        if path in paths_with_req_anchors:
            continue
        base = _sanitize_id(path) + "_file"
        fid = _unique(base, used_ids)
        fragments.append({"id": fid, "selector": {"path": path}})
        required_ids.append(fid)
    for idx, snippet in enumerate(v1["must_include_content"]):
        fid = _unique(f"req_global_{idx}", used_ids)
        fragments.append({"id": fid, "selector": {"anchor": snippet}})
        required_ids.append(fid)
    for idx, pattern in enumerate(v1["must_include"]):
        fid = _unique(f"req_pattern_{idx}", used_ids)
        fragments.append({"id": fid, "selector": {"anchor": pattern}})
        required_ids.append(fid)
    return required_ids


def _build_forbidden_fragments(v1: dict, used_ids: set[str], fragments: list) -> list[str]:
    forbidden_ids: list[str] = []
    paths_with_fbd_anchors: set[str] = set()
    for path, snippets in v1["must_not_include_content_from"].items():
        paths_with_fbd_anchors.add(path)
        base = _sanitize_id(path)
        for snippet in snippets:
            fid = _unique("no_" + base, used_ids)
            fragments.append({"id": fid, "selector": {"path": path, "anchor": snippet}})
            forbidden_ids.append(fid)
    for path in v1["must_not_include_files"]:
        if path in paths_with_fbd_anchors:
            continue
        base = "no_" + _sanitize_id(path)
        fid = _unique(base, used_ids)
        fragments.append({"id": fid, "selector": {"path": path}})
        forbidden_ids.append(fid)
    for idx, marker in enumerate(v1["must_not_include"]):
        fid = _unique(f"no_global_{idx}", used_ids)
        fragments.append({"id": fid, "selector": {"anchor": marker}})
        forbidden_ids.append(fid)
    return forbidden_ids


def _convert_to_v3(v1: dict, source_file: Path) -> dict:
    name = v1["name"] or source_file.stem
    fragments: list = []
    used_ids: set[str] = set()

    required_ids = _build_required_fragments(v1, used_ids, fragments)
    forbidden_ids = _build_forbidden_fragments(v1, used_ids, fragments)

    # Build v3 structure
    v3: dict = {"name": name}

    v3["repo"] = {
        "initial_files": _make_file_dict(v1["initial_files"]),
        "changed_files": _make_file_dict(v1["changed_files"]),
        "commit_message": v1["commit_message"],
    }

    fixtures: dict = {}
    if v1["add_garbage"]:
        fixtures["auto_garbage"] = True
    if fixtures:
        v3["fixtures"] = fixtures

    if fragments:
        v3["fragments"] = _serialize_fragments(fragments)

    v3["oracle"] = {
        "required": required_ids,
        "allowed": [],
        "forbidden": forbidden_ids,
    }

    xfail_val = v1["xfail"]
    if xfail_val:
        v3["xfail"] = {
            "category": None,
            "reason": str(xfail_val),
            "issue": None,
        }

    return v3


def _make_file_dict(files: dict) -> dict:
    return {k: _LiteralStr(v) if "\n" in str(v) else str(v) for k, v in files.items()}


def _serialize_fragments(fragments: list[dict]) -> list[dict]:
    result = []
    for frag in fragments:
        sel_raw = frag["selector"]
        sel: dict = {}
        for key in ("path", "symbol", "kind", "anchor"):
            if sel_raw.get(key) is not None:
                sel[key] = sel_raw[key]
        if sel_raw.get("any_of"):
            sel["any_of"] = sel_raw["any_of"]
        result.append({"id": frag["id"], "selector": sel})
    return result


def _dump_v3(v3: dict) -> str:
    return yaml.dump(
        v3,
        Dumper=_V3Dumper,
        default_flow_style=False,
        allow_unicode=True,
        sort_keys=False,
        width=120,
    )


def _load_yaml(path: Path) -> list[dict]:
    with path.open("r", encoding="utf-8") as f:
        content = yaml.safe_load(f)
    if content is None:
        return []
    if isinstance(content, list):
        return content
    if isinstance(content, dict):
        if "tests" in content:
            return content["tests"]
        return [content]
    return []


def migrate_file(yaml_path: Path, dry_run: bool = False) -> bool:
    try:
        raw_cases = _load_yaml(yaml_path)
        if not raw_cases:
            return False

        v3_cases = []
        for raw in raw_cases:
            if not isinstance(raw, dict):
                continue
            v1 = _parse_v1(raw)
            v3 = _convert_to_v3(v1, yaml_path)
            v3_cases.append(v3)

        if not v3_cases:
            return False

        if len(v3_cases) == 1:
            output = _dump_v3(v3_cases[0])
        else:
            output = "tests:\n"
            for case in v3_cases:
                dumped = _dump_v3(case)
                indented = "\n".join("  " + line if line else "" for line in dumped.splitlines())
                output += "  - " + indented.lstrip() + "\n"

        if not dry_run:
            yaml_path.write_text(output, encoding="utf-8")
        return True

    except Exception as e:
        print(f"  ERROR {yaml_path.name}: {e}", file=sys.stderr)
        return False


def main() -> None:
    dry_run = "--dry-run" in sys.argv
    files = sorted(CASES_DIR.glob("**/*.yaml"))
    # Exclude SCHEMA.md which is markdown
    files = [f for f in files if f.suffix == ".yaml"]

    ok = err = 0
    for f in files:
        success = migrate_file(f, dry_run=dry_run)
        if success:
            ok += 1
        else:
            err += 1

    action = "Would migrate" if dry_run else "Migrated"
    print(f"{action}: {ok} files, {err} errors")


if __name__ == "__main__":
    main()
