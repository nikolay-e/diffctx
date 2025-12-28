# tests/test_coverage_gaps.py
import sys

import pytest

from treemapper import __version__, map_directory, to_text, to_yaml

from .utils import find_node_by_path, get_all_files_in_tree


def test_version_module_exists():
    from treemapper.version import __version__ as version_str

    assert isinstance(version_str, str)
    assert len(version_str) > 0
    parts = version_str.split(".")
    assert len(parts) >= 2


def test_version_exported():
    assert __version__ is not None
    assert isinstance(__version__, str)
    assert "." in __version__


def test_version_format_semver():
    parts = __version__.split(".")
    assert len(parts) >= 2
    assert parts[0].isdigit()
    assert parts[1].isdigit()


def test_map_directory_with_ignore_file_param(tmp_path):
    project = tmp_path / "project"
    project.mkdir()
    (project / "keep.txt").write_text("keep")
    (project / "ignore.log").write_text("ignore")
    (project / "also_ignore.tmp").write_text("tmp")

    ignore_file = tmp_path / "custom.ignore"
    ignore_file.write_text("*.log\n*.tmp\n")

    tree = map_directory(project, ignore_file=ignore_file)
    names = get_all_files_in_tree(tree)

    assert "keep.txt" in names
    assert "ignore.log" not in names
    assert "also_ignore.tmp" not in names


def test_map_directory_with_no_default_ignores_param(tmp_path):
    project = tmp_path / "project"
    project.mkdir()
    (project / ".git").mkdir()
    (project / ".git" / "config").write_text("git config")
    (project / "__pycache__").mkdir()
    (project / "__pycache__" / "module.pyc").write_text("bytecode")
    (project / "regular.txt").write_text("content")
    (project / ".gitignore").write_text("*.pyc\n")
    (project / ".treemapperignore").write_text(".git/\n__pycache__/\n")

    tree_with_defaults = map_directory(project)
    names_with_defaults = get_all_files_in_tree(tree_with_defaults)

    assert ".git" not in names_with_defaults
    assert "__pycache__" not in names_with_defaults

    tree_no_defaults = map_directory(project, no_default_ignores=True)
    names_no_defaults = get_all_files_in_tree(tree_no_defaults)

    assert ".git" in names_no_defaults
    assert "config" in names_no_defaults
    assert "__pycache__" in names_no_defaults


def test_no_default_ignores_still_respects_custom_ignore(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / ".git").mkdir()
    (project / ".git" / "config").write_text("git config")
    (project / "__pycache__").mkdir()
    (project / "__pycache__" / "module.pyc").write_text("bytecode")
    (project / "keep.txt").write_text("keep this")
    (project / "ignore_me.log").write_text("should be ignored")
    (project / ".gitignore").write_text("*.pyc\n")
    (project / ".treemapperignore").write_text(".git/\n__pycache__/\n")

    custom_ignore = tmp_path / "custom.ignore"
    custom_ignore.write_text("*.log\n")

    tree = map_directory(project, no_default_ignores=True, ignore_file=custom_ignore)
    names = get_all_files_in_tree(tree)

    assert ".git" in names
    assert "config" in names
    assert "__pycache__" in names
    assert "module.pyc" in names
    assert "keep.txt" in names

    assert "ignore_me.log" not in names


def test_map_directory_combined_api_params(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "level1").mkdir()
    (project / "level1" / "level2").mkdir()
    (project / "level1" / "level2" / "deep.txt").write_text("deep content")

    large_file = project / "large.txt"
    large_file.write_text("x" * 500)

    (project / "ignore.log").write_text("log content")

    ignore_file = tmp_path / "custom.ignore"
    ignore_file.write_text("*.log\n")

    tree = map_directory(
        project,
        max_depth=2,
        max_file_bytes=100,
        ignore_file=ignore_file,
    )

    names = get_all_files_in_tree(tree)

    assert "level1" in names
    assert "level2" in names
    assert "deep.txt" not in names
    assert "ignore.log" not in names

    large_node = find_node_by_path(tree, ["large.txt"])
    assert large_node is not None
    assert "<file too large:" in large_node.get("content", "")


@pytest.mark.skipif(sys.platform == "win32", reason="Symlinks require elevated privileges on Windows")
def test_symlink_loop_handling(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "regular.txt").write_text("content")

    subdir = project / "subdir"
    subdir.mkdir()

    try:
        loop_link = subdir / "loop"
        loop_link.symlink_to(project, target_is_directory=True)
    except OSError:
        pytest.skip("Cannot create symlinks on this system")

    tree = map_directory(project)
    names = get_all_files_in_tree(tree)

    assert "regular.txt" in names
    assert "subdir" in names
    assert "loop" not in names


@pytest.mark.skipif(sys.platform == "win32", reason="Symlinks require elevated privileges on Windows")
def test_symlink_to_self(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "file.txt").write_text("content")

    try:
        self_link = project / "self_link"
        self_link.symlink_to(project / "self_link")
    except OSError:
        pytest.skip("Cannot create symlinks on this system")

    tree = map_directory(project)
    names = get_all_files_in_tree(tree)

    assert "file.txt" in names
    assert "self_link" not in names


def test_file_at_exact_max_bytes_boundary(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    max_bytes = 100

    exact_file = project / "exact.txt"
    exact_file.write_text("x" * max_bytes)

    one_over_file = project / "one_over.txt"
    one_over_file.write_text("x" * (max_bytes + 1))

    one_under_file = project / "one_under.txt"
    one_under_file.write_text("x" * (max_bytes - 1))

    tree = map_directory(project, max_file_bytes=max_bytes)

    exact_node = find_node_by_path(tree, ["exact.txt"])
    one_over_node = find_node_by_path(tree, ["one_over.txt"])
    one_under_node = find_node_by_path(tree, ["one_under.txt"])

    assert exact_node is not None
    assert one_over_node is not None
    assert one_under_node is not None

    assert "<file too large:" in one_over_node.get("content", "")

    assert "<file too large:" not in exact_node.get("content", "")
    assert "x" * max_bytes in exact_node.get("content", "")

    assert "<file too large:" not in one_under_node.get("content", "")
    assert "x" * (max_bytes - 1) in one_under_node.get("content", "")


def test_bracket_expression_in_ignore_pattern(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "file_a.txt").write_text("a")
    (project / "file_b.txt").write_text("b")
    (project / "file_c.txt").write_text("c")
    (project / "file_d.txt").write_text("d")

    (project / ".gitignore").write_text("file_[abc].txt\n")

    tree = map_directory(project)
    names = get_all_files_in_tree(tree)

    assert "file_a.txt" not in names
    assert "file_b.txt" not in names
    assert "file_c.txt" not in names
    assert "file_d.txt" in names


def test_question_mark_wildcard_in_pattern(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "file1.txt").write_text("1")
    (project / "file2.txt").write_text("2")
    (project / "file10.txt").write_text("10")
    (project / "other.txt").write_text("other")

    (project / ".gitignore").write_text("file?.txt\n")

    tree = map_directory(project)
    names = get_all_files_in_tree(tree)

    assert "file1.txt" not in names
    assert "file2.txt" not in names
    assert "file10.txt" in names
    assert "other.txt" in names


def test_deep_nesting_text_format(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    depth = 10
    current = project
    for i in range(depth):
        current = current / f"level{i}"
        current.mkdir()
        (current / f"file{i}.txt").write_text(f"content{i}")

    tree = map_directory(project)
    text_output = to_text(tree)

    for i in range(depth):
        assert f"level{i}" in text_output
        assert f"file{i}.txt" in text_output

    lines = text_output.strip().split("\n")
    assert len(lines) > depth * 2


def test_text_format_preserves_structure(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "dir_a").mkdir()
    (project / "dir_a" / "file1.txt").write_text("content1")
    (project / "dir_b").mkdir()
    (project / "dir_b" / "file2.txt").write_text("content2")
    (project / "root_file.txt").write_text("root content")

    tree = map_directory(project)
    text_output = to_text(tree)

    assert "dir_a" in text_output
    assert "dir_b" in text_output
    assert "file1.txt" in text_output
    assert "file2.txt" in text_output
    assert "root_file.txt" in text_output


def test_yaml_with_special_unicode_nel(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    nel_char = "\u0085"
    (project / "nel_file.txt").write_text(f"line1{nel_char}line2", encoding="utf-8")

    tree = map_directory(project)
    yaml_output = to_yaml(tree)

    assert "nel_file.txt" in yaml_output

    import yaml

    parsed = yaml.safe_load(yaml_output)
    assert parsed is not None


def test_yaml_with_unicode_line_separators(tmp_path):
    import yaml

    project = tmp_path / "project"
    project.mkdir()

    ls_char = "\u2028"
    ps_char = "\u2029"
    (project / "line_sep.txt").write_text(f"line1{ls_char}line2", encoding="utf-8")
    (project / "para_sep.txt").write_text(f"para1{ps_char}para2", encoding="utf-8")

    tree = map_directory(project)
    yaml_output = to_yaml(tree)

    parsed = yaml.safe_load(yaml_output)
    assert parsed is not None

    line_sep_node = find_node_by_path(parsed, ["line_sep.txt"])
    para_sep_node = find_node_by_path(parsed, ["para_sep.txt"])

    assert line_sep_node is not None
    assert ls_char in line_sep_node.get("content", "")
    assert para_sep_node is not None
    assert ps_char in para_sep_node.get("content", "")


def test_yaml_literal_style_without_trailing_newline(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "no_newline.txt").write_text("content without newline")

    tree = map_directory(project)
    yaml_output = to_yaml(tree)

    assert "no_newline.txt" in yaml_output

    import yaml

    parsed = yaml.safe_load(yaml_output)
    node = find_node_by_path(parsed, ["no_newline.txt"])
    assert node is not None
    assert "content without newline" in node.get("content", "")


def test_empty_directory_handling(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "empty_dir").mkdir()
    (project / "file.txt").write_text("content")

    tree = map_directory(project)
    names = get_all_files_in_tree(tree)

    assert "empty_dir" in names
    assert "file.txt" in names

    empty_node = find_node_by_path(tree, ["empty_dir"])
    assert empty_node is not None
    assert empty_node.get("type") == "directory"
    assert empty_node.get("children") is None or len(empty_node.get("children", [])) == 0


def test_all_verbosity_levels(tmp_path):
    import logging

    from treemapper.logger import setup_logging

    project = tmp_path / "project"
    project.mkdir()
    (project / "file.txt").write_text("content")

    root_logger = logging.getLogger()
    original_level = root_logger.level
    original_handlers = root_logger.handlers[:]

    try:
        for level in range(4):
            setup_logging(level)

            expected_level = {
                0: logging.ERROR,
                1: logging.WARNING,
                2: logging.INFO,
                3: logging.DEBUG,
            }[level]

            assert root_logger.level == expected_level
    finally:
        root_logger.setLevel(original_level)
        root_logger.handlers = original_handlers


def test_binary_detection_at_exact_boundary(tmp_path):
    from treemapper.tree import BINARY_DETECTION_SAMPLE_SIZE

    project = tmp_path / "project"
    project.mkdir()

    null_at_boundary = project / "null_at_boundary.bin"
    content = b"x" * (BINARY_DETECTION_SAMPLE_SIZE - 1) + b"\x00" + b"y" * 100
    null_at_boundary.write_bytes(content)

    null_after_boundary = project / "null_after_boundary.txt"
    content2 = b"x" * BINARY_DETECTION_SAMPLE_SIZE + b"\x00" + b"y" * 100
    null_after_boundary.write_bytes(content2)

    tree = map_directory(project)

    boundary_node = find_node_by_path(tree, ["null_at_boundary.bin"])
    after_node = find_node_by_path(tree, ["null_after_boundary.txt"])

    assert boundary_node is not None
    assert "<binary file:" in boundary_node.get("content", "")

    assert after_node is not None
    assert "<binary file:" in after_node.get("content", "")


def test_deep_nesting_with_ignore_patterns(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    depth = 12
    current = project
    for i in range(depth):
        current = current / f"level{i}"
        current.mkdir()
        (current / f"keep{i}.txt").write_text(f"keep{i}")
        (current / f"ignore{i}.bak").write_text(f"ignore{i}")

    (project / "level0" / ".gitignore").write_text("*.bak\n")

    tree = map_directory(project)
    names = get_all_files_in_tree(tree)

    for i in range(depth):
        assert f"keep{i}.txt" in names

    assert "ignore0.bak" not in names
    assert "ignore5.bak" not in names
    assert "ignore11.bak" not in names


def test_middle_slash_pattern_is_anchored(tmp_path):
    project = tmp_path / "project"
    project.mkdir()

    (project / "subdir").mkdir()
    (project / "subdir" / "docs").mkdir()
    (project / "subdir" / "docs" / "api.txt").write_text("should be ignored")
    (project / "subdir" / "other").mkdir()
    (project / "subdir" / "other" / "docs").mkdir()
    (project / "subdir" / "other" / "docs" / "api.txt").write_text("should be kept")

    (project / "subdir" / ".gitignore").write_text("docs/api.txt\n")

    tree = map_directory(project)

    direct_node = find_node_by_path(tree, ["subdir", "docs", "api.txt"])
    nested_node = find_node_by_path(tree, ["subdir", "other", "docs", "api.txt"])

    assert direct_node is None
    assert nested_node is not None


class TestYamlEscaping:
    def test_escape_yaml_string_newline(self):
        from treemapper.writer import _escape_yaml_string

        result = _escape_yaml_string("file\nname.txt")
        assert "\\n" in result
        assert "\n" not in result

    def test_escape_yaml_string_carriage_return(self):
        from treemapper.writer import _escape_yaml_string

        result = _escape_yaml_string("file\rname.txt")
        assert "\\r" in result
        assert "\r" not in result

    def test_escape_yaml_string_null_byte(self):
        from treemapper.writer import _escape_yaml_string

        result = _escape_yaml_string("file\x00name.txt")
        assert "\\0" in result
        assert "\x00" not in result

    def test_escape_yaml_string_combined(self):
        from treemapper.writer import _escape_yaml_string

        result = _escape_yaml_string('test\n\r\x00"\\name')
        assert "\\n" in result
        assert "\\r" in result
        assert "\\0" in result
        assert '\\"' in result
        assert "\\\\" in result

    def test_escape_yaml_content_null_byte(self):
        from treemapper.writer import _escape_yaml_content

        result = _escape_yaml_content("content\x00with\x00nulls")
        assert "\\0" in result
        assert "\x00" not in result

    def test_escape_yaml_content_all_special_chars(self):
        from treemapper.writer import _escape_yaml_content

        input_str = 'test\n\t\r\x00\x85\u2028\u2029"\\'
        result = _escape_yaml_content(input_str)

        assert "\\n" in result
        assert "\\t" in result
        assert "\\r" in result
        assert "\\0" in result
        assert "\\x85" in result
        assert "\\u2028" in result
        assert "\\u2029" in result
        assert '\\"' in result
        assert "\\\\" in result

    def test_yaml_output_with_escaped_filename(self, tmp_path):
        import io

        from treemapper.writer import write_tree_yaml

        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": 'file"with"quotes.txt', "type": "file", "content": "test\n"}],
        }
        output = io.StringIO()
        write_tree_yaml(output, tree)
        result = output.getvalue()

        import yaml

        parsed = yaml.safe_load(result)
        assert parsed["children"][0]["name"] == 'file"with"quotes.txt'

    def test_has_problematic_chars_function(self):
        from treemapper.writer import _has_problematic_chars

        assert _has_problematic_chars("normal text") is False
        assert _has_problematic_chars("text with newline\n") is False

        assert _has_problematic_chars("text\x85with NEL") is True
        assert _has_problematic_chars("text\u2028with line sep") is True
        assert _has_problematic_chars("text\u2029with para sep") is True

        assert _has_problematic_chars("") is False

    def test_has_problematic_chars_early_exit(self):
        from treemapper.writer import _has_problematic_chars

        long_text = "a" * 100000 + "\x85" + "b" * 100000
        assert _has_problematic_chars(long_text) is True

        normal_long_text = "a" * 200000
        assert _has_problematic_chars(normal_long_text) is False


class TestTokenCountAlwaysDisplayed:
    def test_token_count_displayed_to_stderr(self, temp_project):
        from .conftest import run_treemapper_subprocess

        (temp_project / "test.txt").write_text("hello world")
        result = run_treemapper_subprocess([str(temp_project)])
        assert result.returncode == 0
        assert "tokens" in result.stderr
