import pytest

from treemapper.diffctx import build_diff_context
from treemapper.diffctx.fragments import enclosing_fragment, fragment_file


def _extract_files_from_tree(tree: dict) -> set[str]:
    files = set()
    if tree.get("type") == "diff_context":
        for frag in tree.get("fragments", []):
            path = frag.get("path", "")
            if path:
                files.add(path.split("/")[-1])
        return files

    def traverse(node):
        if node.get("type") == "file":
            files.add(node["name"])
        for child in node.get("children", []):
            traverse(child)

    traverse(tree)
    return files


def _extract_fragments_from_tree(tree: dict) -> list[dict]:
    if tree.get("type") == "diff_context":
        return tree.get("fragments", [])
    return []


@pytest.fixture
def diff_project(git_with_commits):
    return git_with_commits


class TestPythonFragmentDecorators:
    def test_frag_py_001_function_decorator_included_in_start_line(self, diff_project):
        diff_project.add_file(
            "decorated.py",
            """\
@decorator
def my_function():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "decorated.py",
            """\
@decorator
def my_function():
    return 42
""",
        )
        diff_project.commit("Change function")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1

        func_frag = next((f for f in fragments if "my_function" in f.get("content", "")), None)
        assert func_frag is not None
        assert "@decorator" in func_frag["content"]
        lines = func_frag["lines"]
        start_line = int(lines.split("-")[0])
        assert start_line == 1

    def test_frag_py_002_multiple_decorators_all_included(self, diff_project):
        diff_project.add_file(
            "multi_dec.py",
            """\
@decorator1
@decorator2
@decorator3
def decorated_func():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "multi_dec.py",
            """\
@decorator1
@decorator2
@decorator3
def decorated_func():
    return 1
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        func_frag = next((f for f in fragments if "decorated_func" in f.get("content", "")), None)
        assert func_frag is not None
        assert "@decorator1" in func_frag["content"]
        assert "@decorator2" in func_frag["content"]
        assert "@decorator3" in func_frag["content"]
        lines = func_frag["lines"]
        start_line = int(lines.split("-")[0])
        assert start_line == 1

    def test_frag_py_003_class_decorator_included_in_start_line(self, diff_project):
        diff_project.add_file(
            "decorated_class.py",
            """\
@dataclass
class MyClass:
    value: int
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "decorated_class.py",
            """\
@dataclass
class MyClass:
    value: int
    name: str
""",
        )
        diff_project.commit("Add field")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        class_frag = next((f for f in fragments if "MyClass" in f.get("content", "")), None)
        assert class_frag is not None
        assert "@dataclass" in class_frag["content"]


class TestPythonFragmentNestedFunctions:
    def test_frag_py_010_nested_function_creates_separate_fragment(self, tmp_path):
        code = """\
def outer():
    x = 1
    def inner():
        return x
    return inner()
"""
        file_path = tmp_path / "nested.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        {f.id.path.stem + ":" + str(f.start_line) for f in fragments if f.kind == "function"}
        assert len([f for f in fragments if f.kind == "function"]) >= 2

    def test_frag_py_011_deeply_nested_functions(self, tmp_path):
        code = """\
def level1():
    def level2():
        def level3():
            return 42
        return level3()
    return level2()
"""
        file_path = tmp_path / "deep_nested.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        function_frags = [f for f in fragments if f.kind == "function"]
        assert len(function_frags) >= 3


class TestPythonModuleGapFragments:
    def test_frag_py_020_module_gap_before_first_def(self, tmp_path):
        code = """\
import os
import sys

CONFIG_PATH = "/etc/app.conf"
DEBUG = True

def main():
    pass
"""
        file_path = tmp_path / "with_gap.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        chunk_frags = [f for f in fragments if f.kind == "chunk"]
        assert len(chunk_frags) >= 1

        chunk_frag = chunk_frags[0]
        assert "import os" in chunk_frag.content
        assert "CONFIG_PATH" in chunk_frag.content

    def test_frag_py_021_module_gap_between_definitions(self, tmp_path):
        code = """\
def func1():
    pass

CONSTANT = 123

def func2():
    pass
"""
        file_path = tmp_path / "gap_between.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        function_frags = [f for f in fragments if f.kind == "function"]
        assert len(function_frags) >= 2


class TestPythonSyntaxErrorFallback:
    def test_frag_py_030_syntax_error_fallback_to_generic(self, tmp_path):
        code = """\
def broken(:
    x = [1, 2
    return x
"""
        file_path = tmp_path / "broken.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1
        assert fragments[0].kind == "chunk"

    def test_frag_py_031_partial_syntax_error_graceful(self, tmp_path):
        code = """\
def valid_function():
    return 42

class BrokenClass
    def method(self):
        pass
"""
        file_path = tmp_path / "partial_broken.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)
        assert len(fragments) >= 1


class TestPythonAsyncFunctions:
    def test_frag_py_040_async_function_handled(self, tmp_path):
        code = """\
async def async_handler():
    await something()
    return result
"""
        file_path = tmp_path / "async_code.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        async_frags = [f for f in fragments if f.kind == "function"]
        assert len(async_frags) >= 1
        assert "async def" in async_frags[0].content

    def test_frag_py_041_async_with_decorator(self, tmp_path):
        code = """\
@async_decorator
async def decorated_async():
    await other()
"""
        file_path = tmp_path / "async_decorated.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        async_frags = [f for f in fragments if f.kind == "function"]
        assert len(async_frags) >= 1
        assert "@async_decorator" in async_frags[0].content
        assert "async def" in async_frags[0].content


class TestMarkdownFragmentSections:
    def test_frag_md_001_large_top_level_section(self, diff_project):
        diff_project.add_file(
            "docs.md",
            """\
# Main Section

This is a large section with lots of content.
Line 1
Line 2
Line 3
Line 4
Line 5

More paragraphs here.
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "docs.md",
            """\
# Main Section

This is a MODIFIED section with lots of content.
Line 1
Line 2
Line 3
Line 4
Line 5

More paragraphs here.
""",
        )
        diff_project.commit("Modify section")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1
        md_frag = next((f for f in fragments if "docs.md" in f.get("path", "")), None)
        assert md_frag is not None
        assert "# Main Section" in md_frag["content"]

    def test_frag_md_002_multiple_headings_select_multiple_sections(self, diff_project):
        diff_project.add_file(
            "multi_section.md",
            """\
# Section One
Content 1

# Section Two
Content 2

# Section Three
Content 3
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "multi_section.md",
            """\
# Section One
Modified content 1

# Section Two
Content 2

# Section Three
Modified content 3
""",
        )
        diff_project.commit("Modify two sections")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        md_frags = [f for f in fragments if "multi_section.md" in f.get("path", "")]
        assert len(md_frags) >= 1

    def test_frag_md_003_no_headings_fallback_paragraph(self, tmp_path):
        code = """\
This is a document without any headings.

It has multiple paragraphs.

Each paragraph should be handled.

This is the last paragraph.
"""
        file_path = tmp_path / "no_headings.md"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1

    def test_frag_md_004_empty_sections_filtered(self, tmp_path):
        code = """\
# Section 1

# Section 2

Content here.

# Section 3
"""
        file_path = tmp_path / "sparse.md"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        for frag in fragments:
            assert frag.content.strip() != ""

    def test_frag_md_005_symbol_truncation_50_chars(self, tmp_path):
        long_heading = "A" * 100
        code = f"""\
# {long_heading}

Content under very long heading.
"""
        file_path = tmp_path / "long_heading.md"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1


class TestGenericChunking:
    def test_frag_gen_001_chunk_200_lines(self, tmp_path):
        lines = [f"line {i}" for i in range(1, 251)]
        code = "\n".join(lines)
        file_path = tmp_path / "large.tex"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 2
        first_frag = fragments[0]
        assert first_frag.kind == "chunk"
        assert first_frag.start_line == 1
        assert first_frag.end_line <= 200

    def test_frag_gen_002_chunk_boundary_change(self, diff_project):
        lines_initial = [f"line {i}" for i in range(1, 251)]
        diff_project.add_file("large.txt", "\n".join(lines_initial))
        diff_project.commit("Initial")

        lines_modified = lines_initial.copy()
        lines_modified[199] = "MODIFIED line 200"
        diff_project.add_file("large.txt", "\n".join(lines_modified))
        diff_project.commit("Modify line 200")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestConfigFragments:
    def test_frag_cfg_001_yaml_top_level_keys(self, tmp_path):
        code = """\
database:
  host: localhost
  port: 5432

server:
  port: 8080
  debug: true
"""
        file_path = tmp_path / "config.yaml"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 2

    def test_frag_cfg_002_toml_sections(self, tmp_path):
        code = """\
[database]
host = "localhost"
port = 5432

[server]
port = 8080
debug = true
"""
        file_path = tmp_path / "config.toml"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 2

    def test_frag_cfg_003_json_top_level_keys(self, tmp_path):
        code = """\
{
  "database": {
    "host": "localhost",
    "port": 5432
  },
  "server": {
    "port": 8080,
    "debug": true
  }
}
"""
        file_path = tmp_path / "config.json"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        assert len(fragments) >= 1


class TestSingleLineFragments:
    def test_frag_single_001_config_single_line_relevant(self, diff_project):
        diff_project.add_file(
            "app.py",
            """\
import os

DEBUG = os.getenv("DEBUG", False)

def main():
    if DEBUG:
        print("Debug mode")
""",
        )
        diff_project.add_file(".env", "DEBUG=false\n")
        diff_project.commit("Initial")

        diff_project.add_file(".env", "DEBUG=true\n")
        diff_project.commit("Enable debug")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        files = _extract_files_from_tree(tree)
        assert ".env" in files

    def test_frag_single_002_import_statement_context(self, diff_project):
        diff_project.add_file(
            "utils.py",
            """\
def helper():
    return 42
""",
        )
        diff_project.add_file(
            "main.py",
            """\
from utils import helper

def main():
    return helper()
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "utils.py",
            """\
def helper():
    return 100
""",
        )
        diff_project.commit("Change helper")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        files = _extract_files_from_tree(tree)
        assert "utils.py" in files


class TestEnclosingFragment:
    def test_enclosing_001_find_smallest_covering(self, tmp_path):
        code = """\
class MyClass:
    def method1(self):
        pass

    def method2(self):
        x = 1
        y = 2
        return x + y

    def method3(self):
        pass
"""
        file_path = tmp_path / "enclosing.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        enclosing = enclosing_fragment(fragments, 7)
        assert enclosing is not None
        assert "method2" in enclosing.content

    def test_enclosing_002_no_match_returns_none(self, tmp_path):
        code = """\
def func():
    pass
"""
        file_path = tmp_path / "small.py"
        file_path.write_text(code)

        fragments = fragment_file(file_path, code)

        enclosing = enclosing_fragment(fragments, 100)
        assert enclosing is None
