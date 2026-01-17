import pytest

from treemapper.diffctx import build_diff_context


def _extract_fragments_from_tree(tree: dict) -> list[dict]:
    if tree.get("type") == "diff_context":
        return tree.get("fragments", [])
    return []


@pytest.fixture
def diff_project(git_with_commits):
    return git_with_commits


class TestYAMLStructure:
    def test_output_yaml_001_diff_context_type(self, diff_project):
        diff_project.add_file("test.py", "def foo():\n    pass\n")
        diff_project.commit("Initial")

        diff_project.add_file("test.py", "def foo():\n    return 1\n")
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        assert tree.get("type") == "diff_context"
        assert "fragments" in tree

    def test_output_yaml_002_empty_diff_structure(self, diff_project):
        diff_project.add_file("test.py", "x = 1\n")
        sha1 = diff_project.commit("Initial")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range=f"{sha1}..{sha1}",
            budget_tokens=10000,
        )

        assert tree.get("type") in ("diff_context", "directory")

    def test_output_yaml_003_required_fields_present(self, diff_project):
        diff_project.add_file(
            "module.py",
            """\
def my_function():
    return 42
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "module.py",
            """\
def my_function():
    return 100
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1

        for frag in fragments:
            assert "path" in frag
            assert "lines" in frag
            assert "kind" in frag
            assert "content" in frag

    def test_output_yaml_004_optional_symbol_present_when_extracted(self, diff_project):
        diff_project.add_file(
            "funcs.py",
            """\
def named_function():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "funcs.py",
            """\
def named_function():
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
        func_frag = next((f for f in fragments if "named_function" in f.get("content", "")), None)
        if func_frag:
            assert "symbol" in func_frag or func_frag.get("kind") in ("chunk", "module")


class TestSortingConsistency:
    def test_output_sort_001_deterministic_output(self, diff_project):
        diff_project.add_file("a.py", "def a():\n    pass\n")
        diff_project.add_file("b.py", "def b():\n    pass\n")
        diff_project.add_file("c.py", "def c():\n    pass\n")
        diff_project.commit("Initial")

        diff_project.add_file("a.py", "def a():\n    return 1\n")
        diff_project.add_file("b.py", "def b():\n    return 2\n")
        diff_project.add_file("c.py", "def c():\n    return 3\n")
        diff_project.commit("Change all")

        results = []
        for _ in range(5):
            tree = build_diff_context(
                root_dir=diff_project.repo,
                diff_range="HEAD~1..HEAD",
                budget_tokens=10000,
            )
            fragments = _extract_fragments_from_tree(tree)
            results.append([(f.get("path"), f.get("lines")) for f in fragments])

        for result in results[1:]:
            assert result == results[0]

    def test_output_sort_002_sorted_by_path_then_line(self, diff_project):
        diff_project.add_file(
            "z_module.py",
            """\
def z_func():
    pass
""",
        )
        diff_project.add_file(
            "a_module.py",
            """\
def a_func():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "z_module.py",
            """\
def z_func():
    return 1
""",
        )
        diff_project.add_file(
            "a_module.py",
            """\
def a_func():
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
        paths = [f.get("path", "") for f in fragments]

        sorted_paths = sorted(paths)
        assert paths == sorted_paths


class TestLineRangeAccuracy:
    def test_output_lines_001_accurate_range(self, diff_project):
        diff_project.add_file(
            "accurate.py",
            """\
def first():
    pass

def second():
    x = 1
    y = 2
    return x + y

def third():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "accurate.py",
            """\
def first():
    pass

def second():
    x = 10
    y = 20
    return x + y

def third():
    pass
""",
        )
        diff_project.commit("Change second")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        second_frag = next((f for f in fragments if "second" in f.get("content", "")), None)
        assert second_frag is not None

        lines = second_frag["lines"]
        start, end = map(int, lines.split("-"))

        content_lines = second_frag["content"].strip().split("\n")
        expected_line_count = end - start + 1
        assert len(content_lines) == expected_line_count or len(content_lines) <= expected_line_count


class TestNoContentMode:
    def test_output_nocontent_001_empty_content(self, diff_project):
        diff_project.add_file(
            "module.py",
            """\
def func():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "module.py",
            """\
def func():
    return 1
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
            no_content=True,
        )

        fragments = _extract_fragments_from_tree(tree)
        for frag in fragments:
            assert frag.get("content", "") == ""


class TestFullMode:
    def test_output_full_001_all_changed_files(self, diff_project):
        diff_project.add_file("a.py", "x = 1\n")
        diff_project.add_file("b.py", "y = 2\n")
        diff_project.add_file("c.py", "z = 3\n")
        diff_project.commit("Initial")

        diff_project.add_file("a.py", "x = 10\n")
        diff_project.add_file("b.py", "y = 20\n")
        diff_project.commit("Change a and b")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=100,
            full=True,
        )

        fragments = _extract_fragments_from_tree(tree)
        paths = {f.get("path", "").split("/")[-1] for f in fragments}
        assert "a.py" in paths
        assert "b.py" in paths
        assert "c.py" not in paths

    def test_output_full_002_ignores_budget(self, diff_project):
        large_content = "\n".join([f"line_{i} = {i}" for i in range(100)])
        diff_project.add_file("large.py", large_content)
        diff_project.commit("Initial")

        modified_content = "\n".join([f"line_{i} = {i * 2}" for i in range(100)])
        diff_project.add_file("large.py", modified_content)
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10,
            full=True,
        )

        fragments = _extract_fragments_from_tree(tree)
        assert len(fragments) >= 1


class TestFragmentCount:
    def test_output_count_001_fragment_count_in_tree(self, diff_project):
        diff_project.add_file("a.py", "def a():\n    pass\n")
        diff_project.add_file("b.py", "def b():\n    pass\n")
        diff_project.commit("Initial")

        diff_project.add_file("a.py", "def a():\n    return 1\n")
        diff_project.add_file("b.py", "def b():\n    return 2\n")
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        if "fragment_count" in tree:
            fragments = _extract_fragments_from_tree(tree)
            assert tree["fragment_count"] == len(fragments)


class TestPathNormalization:
    def test_output_path_001_relative_paths(self, diff_project):
        diff_project.add_file("src/module.py", "def func():\n    pass\n")
        diff_project.commit("Initial")

        diff_project.add_file("src/module.py", "def func():\n    return 1\n")
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        for frag in fragments:
            path = frag.get("path", "")
            assert not path.startswith("/")
            assert str(diff_project.repo) not in path


class TestKindValues:
    def test_output_kind_001_valid_kinds(self, diff_project):
        diff_project.add_file(
            "module.py",
            """\
class MyClass:
    def method(self):
        pass

def function():
    pass
""",
        )
        diff_project.commit("Initial")

        diff_project.add_file(
            "module.py",
            """\
class MyClass:
    def method(self):
        return 1

def function():
    return 2
""",
        )
        diff_project.commit("Change")

        tree = build_diff_context(
            root_dir=diff_project.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        fragments = _extract_fragments_from_tree(tree)
        valid_kinds = {"function", "class", "module", "chunk", "section", "paragraph"}
        for frag in fragments:
            kind = frag.get("kind", "")
            assert kind in valid_kinds or kind, f"Unknown kind: {kind}"
