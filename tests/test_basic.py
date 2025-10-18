# tests/test_basic.py
import logging
import shutil
import sys

import pytest

from .utils import find_node_by_path, get_all_files_in_tree, load_yaml, make_hashable


def normalize_content(content):
    """Normalize content by stripping leading/trailing whitespace but keeping internal structure."""
    if content is None:
        return None
    return content


def normalize_tree(tree):
    """Normalize tree for comparison by sorting children."""

    if isinstance(tree, dict):
        result = tree.copy()
        if "children" in result and isinstance(result["children"], list):
            result["children"] = sorted(
                [normalize_tree(child) for child in result["children"] if isinstance(child, dict)],
                key=lambda x: x.get("name", ""),
            )
        return result
    return tree


def test_basic_mapping(temp_project, run_mapper):
    """Test basic directory mapping with default settings."""
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    output_file = temp_project / "directory_tree.yaml"
    assert output_file.exists()
    result = load_yaml(output_file)
    assert result["type"] == "directory"
    assert result["name"] == temp_project.name
    all_files = get_all_files_in_tree(result)
    assert "src" in all_files
    assert "main.py" in all_files
    assert "test.py" in all_files
    assert "docs" in all_files
    assert ".git" not in all_files, ".git should be ignored by default .treemapperignore"
    assert "output" not in all_files
    assert "directory_tree.yaml" not in all_files


def test_directory_content(temp_project, run_mapper):
    """Test directory structure and content preservation."""
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    src_dir = find_node_by_path(result, ["src"])
    assert src_dir is not None and src_dir["type"] == "directory"
    main_py = find_node_by_path(src_dir, ["main.py"])
    assert main_py is not None and main_py["type"] == "file"
    expected_main_content = "def main():\n    print('hello')\n"
    assert main_py.get("content") == expected_main_content


def test_custom_output(temp_project, run_mapper):
    """Test custom output file locations and names."""
    output_path1 = temp_project / "custom.yaml"
    assert run_mapper([".", "-o", str(output_path1)])
    assert output_path1.exists()

    subdir = temp_project / "subdir"
    subdir.mkdir()
    output_path2 = subdir / "output.yaml"
    assert run_mapper([".", "-o", str(output_path2)])
    assert output_path2.exists()

    result1 = load_yaml(output_path1)
    result2 = load_yaml(output_path2)

    assert find_node_by_path(result1, ["src", "main.py"]) is not None
    assert find_node_by_path(result2, ["src", "main.py"]) is not None
    assert find_node_by_path(result1, ["docs", "readme.md"]) is not None
    assert find_node_by_path(result2, ["docs", "readme.md"]) is not None
    assert find_node_by_path(result1, ["custom.yaml"]) is None
    assert find_node_by_path(result2, ["output.yaml"]) is None
    assert find_node_by_path(result2, ["subdir", "output.yaml"]) is None


def test_file_content_encoding(temp_project, run_mapper):
    """Test handling of different file encodings and content."""
    ascii_content_orig = "Hello World"
    multiline_content_orig = "line1\nline2\nline3"
    empty_content_orig = ""

    (temp_project / "ascii.txt").write_text(ascii_content_orig)
    (temp_project / "multiline.txt").write_text(multiline_content_orig)
    (temp_project / "empty.txt").write_text(empty_content_orig)

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")

    ascii_node = find_node_by_path(result, ["ascii.txt"])
    multiline_node = find_node_by_path(result, ["multiline.txt"])
    empty_node = find_node_by_path(result, ["empty.txt"])

    assert ascii_node is not None and ascii_node.get("content") == ascii_content_orig + "\n"
    assert multiline_node is not None and multiline_node.get("content") == multiline_content_orig + "\n"
    assert empty_node is not None and empty_node.get("content") == empty_content_orig


def test_nested_structures(temp_project, run_mapper):
    """Test handling of deeply nested directory structures."""
    current = temp_project
    contents = {}
    for i in range(5):
        current = current / f"level{i}"
        current.mkdir()
        content_str = f"Content {i}"
        contents[i] = content_str
        (current / f"file{i}.txt").write_text(content_str)

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")

    current_node = result
    for i in range(5):
        level_dir_node = find_node_by_path(current_node, [f"level{i}"])
        assert level_dir_node is not None, f"Level {i} directory not found"
        assert level_dir_node.get("type") == "directory"

        level_file_node = find_node_by_path(level_dir_node, [f"file{i}.txt"])
        assert level_file_node is not None, f"File {i} in level {i} not found"
        assert level_file_node.get("type") == "file"

        expected_content = contents[i] + "\n"
        assert level_file_node.get("content") == expected_content
        current_node = level_dir_node


def test_absolute_relative_paths(temp_project, run_mapper):
    """Test handling of absolute and relative paths."""
    output_path_abs = temp_project / "abs_output.yaml"
    output_path_rel_src = temp_project / "src.yaml"
    output_path_rel_root = temp_project / "root.yaml"

    assert run_mapper([str(temp_project.absolute()), "-o", str(output_path_abs)])
    assert output_path_abs.exists()
    assert run_mapper(["./src", "-o", str(output_path_rel_src)])
    assert output_path_rel_src.exists()
    assert run_mapper([".", "-o", str(output_path_rel_root)])
    assert output_path_rel_root.exists()

    abs_result = load_yaml(output_path_abs)
    src_result = load_yaml(output_path_rel_src)
    root_result = load_yaml(output_path_rel_root)

    src_node_in_root = find_node_by_path(root_result, ["src"])
    assert src_node_in_root is not None

    output_files_to_ignore_src = {output_path_rel_src.name}
    src_children_set = {
        make_hashable(child) for child in src_result.get("children", []) if child.get("name") not in output_files_to_ignore_src
    }
    src_node_children_set = {make_hashable(child) for child in src_node_in_root.get("children", [])}
    assert src_children_set == src_node_children_set

    output_files_to_ignore_root = {
        output_path_abs.name,
        output_path_rel_root.name,
        output_path_rel_src.name,
    }
    abs_children_set = {
        make_hashable(child) for child in abs_result.get("children", []) if child.get("name") not in output_files_to_ignore_root
    }
    root_children_set = {
        make_hashable(child) for child in root_result.get("children", []) if child.get("name") not in output_files_to_ignore_root
    }
    assert abs_children_set == root_children_set, f"Set difference: {abs_children_set.symmetric_difference(root_children_set)}"


def test_output_handling(temp_project, run_mapper):
    """Test various output file scenarios."""
    output_file_overwrite = temp_project / "output_overwrite.yaml"
    output_file_overwrite.write_text("original content")
    assert run_mapper([".", "-o", str(output_file_overwrite)])
    assert "original content" not in output_file_overwrite.read_text()
    assert load_yaml(output_file_overwrite) is not None

    new_dir = temp_project / "new_dir_created_by_writer"
    output_path_new_dir = new_dir / "tree.yaml"
    if new_dir.exists():
        shutil.rmtree(new_dir)
    assert run_mapper([".", "-o", str(output_path_new_dir)])
    assert output_path_new_dir.exists()
    assert new_dir.is_dir()
    assert load_yaml(output_path_new_dir) is not None


WIN_SKIP_MSG = "Skipping unicode filename test on Windows (potential FS issues)"


@pytest.mark.skipif(sys.platform == "win32", reason=WIN_SKIP_MSG)
def test_unicode_filenames(temp_project, run_mapper):
    """Test: files and directories with Unicode names."""
    (temp_project / "привет_мир").mkdir()
    (temp_project / "привет_мир" / "файл.txt").write_text("содержимое", encoding="utf-8")
    (temp_project / "你好.txt").write_text("世界", encoding="utf-8")
    (temp_project / "📄").touch()

    output_path = temp_project / "unicode_names_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)
    all_names = get_all_files_in_tree(result)

    assert "привет_мир" in all_names
    assert "файл.txt" in all_names
    assert "你好.txt" in all_names
    assert "📄" in all_names

    nihao_node = find_node_by_path(result, ["你好.txt"])
    assert nihao_node is not None

    assert nihao_node.get("content") == "世界\n"

    privet_file_node = find_node_by_path(result, ["привет_мир", "файл.txt"])
    assert privet_file_node is not None

    assert privet_file_node.get("content") == "содержимое\n"


def test_unicode_content_and_encoding_errors(temp_project, run_mapper, caplog):
    """Test: content in UTF-8, non-UTF8 (CP1251), binary."""
    utf8_content = "привет мир"
    try:
        cp1251_content_bytes = "тест".encode("cp1251")
    except LookupError:
        pytest.skip("CP1251 codec not found, skipping test")
    binary_content = b"\x00\x81\x9f\xff"

    (temp_project / "utf8.txt").write_text(utf8_content, encoding="utf-8")
    (temp_project / "cp1251.txt").write_bytes(cp1251_content_bytes)
    (temp_project / "binary.bin").write_bytes(binary_content)

    output_path = temp_project / "encodings_output.yaml"
    with caplog.at_level(logging.WARNING):
        assert run_mapper([".", "-o", str(output_path), "-v", "1"])
    result = load_yaml(output_path)

    utf8_node = find_node_by_path(result, ["utf8.txt"])
    cp1251_node = find_node_by_path(result, ["cp1251.txt"])
    binary_node = find_node_by_path(result, ["binary.bin"])

    assert utf8_node is not None, "'utf8.txt' not found"

    assert utf8_node.get("content") == utf8_content + "\n"

    assert cp1251_node is not None, "'cp1251.txt' not found"

    assert cp1251_node.get("content") == "<unreadable content: not utf-8>\n"
    assert any(
        "Cannot decode cp1251.txt as UTF-8" in record.message for record in caplog.records if record.levelno >= logging.WARNING
    ), "Expected WARNING log message about decoding failure not found for cp1251.txt"

    assert binary_node is not None, "'binary.bin' not found"

    assert isinstance(binary_node.get("content"), str)
    # Binary files can be detected as binary or fail UTF-8 decode
    assert binary_node.get("content", "").startswith("<unreadable content") or binary_node.get("content", "").startswith(
        "<binary file:"
    )
    if binary_node.get("content") not in ("<unreadable content>", "<unreadable content>\n"):
        assert binary_node.get("content", "").endswith("\n") or binary_node.get("content", "").startswith("<binary file:")

    # Binary files may be detected early (no warning) or fail during read (warning/error)
    # Accept either case
    has_binary_log = any(
        ("Cannot decode binary.bin as UTF-8" in record.message and record.levelno >= logging.WARNING)
        or ("Could not read binary.bin" in record.message and record.levelno >= logging.ERROR)
        or ("Unexpected error reading binary.bin" in record.message and record.levelno >= logging.ERROR)
        or ("Removed NULL bytes from content of binary.bin" in record.message and record.levelno >= logging.WARNING)
        or ("Detected binary file binary.bin" in record.message and record.levelno >= logging.DEBUG)
        for record in caplog.records
    )
    # Either we have a log about the binary file, OR it was detected as binary (starts with "<binary file:")
    assert has_binary_log or binary_node.get("content", "").startswith(
        "<binary file:"
    ), "Expected WARNING/ERROR log message about reading/decoding failure or binary detection for binary.bin"
