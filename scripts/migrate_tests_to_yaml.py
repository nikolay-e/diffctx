#!/usr/bin/env python3
from __future__ import annotations

import ast
import re
import sys
from pathlib import Path

import yaml


def extract_string_value(node: ast.expr) -> str | None:
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.JoinedStr):
        parts = []
        for value in node.values:
            if isinstance(value, ast.Constant):
                parts.append(str(value.value))
            elif isinstance(value, ast.FormattedValue):
                parts.append("{...}")
        return "".join(parts)
    return None


def extract_list_value(node: ast.expr) -> list[str]:
    if not isinstance(node, ast.List):
        return []
    result = []
    for elt in node.elts:
        val = extract_string_value(elt)
        if val:
            result.append(val)
    return result


def extract_dict_value(node: ast.expr) -> dict[str, str]:
    if not isinstance(node, ast.Dict):
        return {}
    result = {}
    for key, value in zip(node.keys, node.values):
        if key is None:
            continue
        key_str = extract_string_value(key)
        val_str = extract_string_value(value)
        if key_str and val_str is not None:
            result[key_str] = val_str
    return result


def _set_option(case: dict, key: str, value: ast.expr) -> None:
    if isinstance(value, ast.Constant):
        if value.value is not None:
            case.setdefault("options", {})[key] = value.value


def _process_keyword(case: dict, kw: ast.keyword) -> None:
    handlers = {
        "name": lambda: case.__setitem__("name", extract_string_value(kw.value)),
        "initial_files": lambda: case.__setitem__("initial", extract_dict_value(kw.value)),
        "changed_files": lambda: case.__setitem__("changed", extract_dict_value(kw.value)),
        "must_include": lambda: case.setdefault("assertions", {}).__setitem__("must_include", extract_list_value(kw.value)),
        "must_not_include": lambda: case.setdefault("assertions", {}).__setitem__(
            "must_not_include", extract_list_value(kw.value)
        ),
        "commit_message": lambda: case.setdefault("options", {}).__setitem__("commit_message", extract_string_value(kw.value)),
        "add_garbage_files": lambda: _set_option(case, "add_garbage", kw.value),
        "skip_garbage_check": lambda: _set_option(case, "skip_garbage_check", kw.value),
        "overhead_ratio": lambda: _set_option(case, "overhead_ratio", kw.value),
        "min_budget": lambda: _set_option(case, "min_budget", kw.value),
    }
    handler = handlers.get(kw.arg)
    if handler:
        handler()


def extract_diff_test_case(call_node: ast.Call) -> dict | None:
    case: dict = {}
    for kw in call_node.keywords:
        _process_keyword(case, kw)
    return case if case.get("name") else None


def find_diff_test_cases(tree: ast.Module) -> list[dict]:
    cases = []

    class Visitor(ast.NodeVisitor):
        def visit_Call(self, node: ast.Call):
            is_direct = isinstance(node.func, ast.Name) and node.func.id == "DiffTestCase"
            is_attr = isinstance(node.func, ast.Attribute) and node.func.attr == "DiffTestCase"
            if is_direct or is_attr:
                case = extract_diff_test_case(node)
                if case:
                    cases.append(case)
            self.generic_visit(node)

    Visitor().visit(tree)
    return cases


def convert_file(input_path: Path, output_dir: Path) -> int:
    with input_path.open("r", encoding="utf-8") as f:
        source = f.read()

    try:
        tree = ast.parse(source)
    except SyntaxError as e:
        print(f"Syntax error in {input_path}: {e}")
        return 0

    cases = find_diff_test_cases(tree)
    if not cases:
        print(f"No DiffTestCase found in {input_path}")
        return 0

    match = re.search(r"test_diff_(\w+)\.py", input_path.name)
    lang = match.group(1) if match else "misc"

    lang_dir = output_dir / lang
    lang_dir.mkdir(parents=True, exist_ok=True)

    for case in cases:
        name = case["name"]
        yaml_path = lang_dir / f"{name}.yaml"

        case_literal = convert_to_literal(case)
        yaml_content = yaml.dump(case_literal, default_flow_style=False, allow_unicode=True, sort_keys=False, width=120)

        with yaml_path.open("w", encoding="utf-8") as f:
            f.write(yaml_content)

    print(f"Converted {len(cases)} cases from {input_path.name} -> {lang_dir}")
    return len(cases)


class LiteralStr(str):
    pass


def literal_str_representer(dumper, data):
    style = "|" if "\n" in data else None
    return dumper.represent_scalar("tag:yaml.org,2002:str", data, style=style)


yaml.add_representer(LiteralStr, literal_str_representer)


def convert_to_literal(obj):
    if isinstance(obj, dict):
        return {k: convert_to_literal(v) for k, v in obj.items()}
    if isinstance(obj, list):
        return [convert_to_literal(v) for v in obj]
    if isinstance(obj, str) and "\n" in obj:
        return LiteralStr(obj)
    return obj


def main():
    tests_dir = Path(__file__).parent.parent / "tests"
    output_dir = tests_dir / "cases" / "diff"

    if len(sys.argv) > 1:
        input_files = [Path(p) for p in sys.argv[1:]]
    else:
        input_files = list(tests_dir.glob("test_diff_*.py"))

    total = 0
    for input_path in sorted(input_files):
        if not input_path.exists():
            print(f"File not found: {input_path}")
            continue
        total += convert_file(input_path, output_dir)

    print(f"\nTotal: {total} test cases converted")


if __name__ == "__main__":
    main()
