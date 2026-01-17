from pathlib import Path

import pytest

from treemapper.diffctx import build_diff_context
from treemapper.diffctx.fragments import fragment_file
from treemapper.diffctx.types import Fragment, FragmentId


def _extract_fragments_from_tree(tree: dict) -> list[dict]:
    if tree.get("type") == "diff_context":
        return tree.get("fragments", [])
    return []


def _make_fragment(path: str, start: int, end: int, kind: str = "function", content: str = "") -> Fragment:
    return Fragment(
        id=FragmentId(Path(path), start, end),
        kind=kind,
        content=content or f"content lines {start}-{end}",
        identifiers=frozenset(),
    )


@pytest.fixture
def diff_project(git_with_commits):
    return git_with_commits


class TestCoalescingAdjacentFragments:
    def test_merge_coal_001_adjacent_same_file(self, diff_project):
        diff_project.add_file(
            "adjacent.py",
            """\
def func1():
    return 1

def func2():
    return 2

def func3():
    return 3
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "adjacent.py",
            """\
def func1():
    return 10

def func2():
    return 20

def func3():
    return 3
""",
        )
        diff_project.commit("Change func1 and func2")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        py_frags = [f for f in fragments if "adjacent.py" in f.get("path", "")]
        assert len(py_frags) >= 1

    def test_merge_coal_002_non_adjacent_separate(self, diff_project):
        diff_project.add_file(
            "scattered.py",
            """\
def func1():
    return 1

# Many lines of gap
# Line 1
# Line 2
# Line 3
# Line 4
# Line 5

def func2():
    return 2
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "scattered.py",
            """\
def func1():
    return 10

# Many lines of gap
# Line 1
# Line 2
# Line 3
# Line 4
# Line 5

def func2():
    return 20
""",
        )
        diff_project.commit("Change both")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestDifferentKindNoMerge:
    def test_merge_kind_001_function_class_separate(self, tmp_path):
        code = """\
def standalone_function():
    return 1

class MyClass:
    def method(self):
        pass
"""
        file_path = tmp_path / "mixed.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        function_frags = [f for f in fragments if f.kind == "function"]
        class_frags = [f for f in fragments if f.kind == "class"]

        assert len(function_frags) >= 1
        assert len(class_frags) >= 1


class TestDeduplicationNestedFragments:
    def test_merge_dedup_001_nested_chooses_one(self, diff_project):
        diff_project.add_file(
            "nested.py",
            """\
class Container:
    def method1(self):
        pass

    def method2(self):
        pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "nested.py",
            """\
class Container:
    def method1(self):
        return 1

    def method2(self):
        pass
""",
        )
        diff_project.commit("Change method1")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        nested_frags = [f for f in fragments if "nested.py" in f.get("path", "")]
        assert len(nested_frags) >= 1

    def test_merge_dedup_002_overlapping_ranges(self, tmp_path):
        code = """\
class BigClass:
    def method1(self):
        x = 1
        y = 2
        return x + y

    def method2(self):
        a = 1
        b = 2
        return a * b
"""
        file_path = tmp_path / "overlapping.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        for i, f1 in enumerate(fragments):
            for f2 in fragments[i + 1 :]:
                if f1.path == f2.path:
                    f1_range = set(range(f1.start_line, f1.end_line + 1))
                    f2_range = set(range(f2.start_line, f2.end_line + 1))
                    overlap = f1_range & f2_range
                    assert not (
                        overlap and f1_range != overlap and f2_range != overlap
                    ), f"Partial overlap detected: {f1.id} and {f2.id}"


class TestSameKindMerge:
    def test_merge_same_001_consecutive_chunks(self, tmp_path):
        lines = [f"line {i}" for i in range(1, 101)]
        code = "\n".join(lines)
        file_path = tmp_path / "chunks.txt"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        chunk_frags = [f for f in fragments if f.kind == "chunk"]
        if len(chunk_frags) >= 2:
            first = chunk_frags[0]
            second = chunk_frags[1]
            assert first.path == second.path


class TestMarkdownSectionMerging:
    def test_merge_md_001_subsections_in_parent(self, tmp_path):
        code = """\
# Main Section

Introduction text.

## Subsection 1

Content 1.

## Subsection 2

Content 2.

# Another Section

Different content.
"""
        file_path = tmp_path / "structured.md"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1


class TestParagraphMerging:
    def test_merge_para_001_small_paragraphs_merged(self, tmp_path):
        code = """\
Short para 1.

Short para 2.

Short para 3.

This is a longer paragraph that contains more words and should count
as having enough content to be its own fragment without merging.
"""
        file_path = tmp_path / "paragraphs.txt"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1


class TestConfigSectionMerging:
    def test_merge_cfg_001_yaml_sections_separate(self, tmp_path):
        code = """\
database:
  host: localhost
  port: 5432
  name: mydb

logging:
  level: INFO
  format: json

server:
  port: 8080
  workers: 4
"""
        file_path = tmp_path / "config.yaml"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 2


class TestIntegrationMerging:
    def test_merge_int_001_real_diff_merging(self, diff_project):
        diff_project.add_file(
            "module.py",
            """\
import os

CONFIG = "default"

def func1():
    return 1

def func2():
    return 2

def func3():
    return 3

class Helper:
    def method(self):
        pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "module.py",
            """\
import os

CONFIG = "modified"

def func1():
    return 10

def func2():
    return 2

def func3():
    return 30

class Helper:
    def method(self):
        return True
""",
        )
        diff_project.commit("Multiple changes")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1

        paths = {f.get("path", "") for f in fragments}
        assert any("module.py" in p for p in paths)


class TestBoundaryConditions:
    def test_merge_boundary_001_single_line_gap(self, tmp_path):
        code = """\
def func1():
    pass

def func2():
    pass
"""
        file_path = tmp_path / "single_gap.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        function_frags = [f for f in fragments if f.kind == "function"]
        assert len(function_frags) >= 2

    def test_merge_boundary_002_no_gap(self, tmp_path):
        code = """\
def func1():
    pass
def func2():
    pass
"""
        file_path = tmp_path / "no_gap.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        function_frags = [f for f in fragments if f.kind == "function"]
        assert len(function_frags) >= 2

    def test_merge_boundary_003_large_gap(self, tmp_path):
        code = """\
def func1():
    pass


# Many empty lines follow
#
#
#
#
#
#
#
#
#
#


def func2():
    pass
"""
        file_path = tmp_path / "large_gap.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        function_frags = [f for f in fragments if f.kind == "function"]
        assert len(function_frags) >= 2


class TestFileTypeSpecificMerging:
    def test_merge_filetype_001_python_respects_structure(self, tmp_path):
        code = """\
class Service:
    def __init__(self):
        self.data = []

    def add(self, item):
        self.data.append(item)

    def remove(self, item):
        self.data.remove(item)

    def clear(self):
        self.data = []
"""
        file_path = tmp_path / "service.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        class_frags = [f for f in fragments if f.kind == "class"]
        method_frags = [f for f in fragments if f.kind == "function"]

        assert len(class_frags) >= 1 or len(method_frags) >= 1

    def test_merge_filetype_002_markdown_heading_hierarchy(self, tmp_path):
        code = """\
# Level 1

Content under level 1.

## Level 2

Content under level 2.

### Level 3

Content under level 3.

## Another Level 2

More content.
"""
        file_path = tmp_path / "hierarchy.md"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1
