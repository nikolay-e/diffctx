# tests/test_options.py
from treemapper import map_directory

from .conftest import run_treemapper_subprocess
from .utils import load_yaml


def test_max_depth_option(run_mapper, temp_project):
    (temp_project / "level1").mkdir()
    (temp_project / "level1" / "level2").mkdir()
    (temp_project / "level1" / "level2" / "level3").mkdir()
    (temp_project / "level1" / "level2" / "level3" / "deep.txt").write_text("deep")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--max-depth", "2"])

    tree = load_yaml(output_file)

    level1 = None
    for child in tree.get("children", []):
        if child["name"] == "level1":
            level1 = child
            break

    assert level1 is not None
    assert "children" in level1

    level2 = None
    for child in level1.get("children", []):
        if child["name"] == "level2":
            level2 = child
            break

    assert level2 is not None

    if "children" in level2:
        level3_names = [c["name"] for c in level2["children"]]
        assert "level3" not in level3_names

    api_tree = map_directory(temp_project, max_depth=2)
    api_level1 = next((c for c in api_tree.get("children", []) if c["name"] == "level1"), None)
    assert api_level1 is not None
    api_level2 = next((c for c in api_level1.get("children", []) if c["name"] == "level2"), None)
    assert api_level2 is not None
    if "children" in api_level2:
        assert "level3" not in [c["name"] for c in api_level2["children"]]


def test_no_content_option(run_mapper, temp_project):
    test_file = temp_project / "test.txt"
    test_file.write_text("This should not appear", encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--no-content"])

    tree = load_yaml(output_file)

    test_node = next((c for c in tree.get("children", []) if c.get("name") == "test.txt"), None)
    assert test_node is not None
    assert "content" not in test_node

    api_tree = map_directory(temp_project, no_content=True)
    api_test = next((c for c in api_tree.get("children", []) if c.get("name") == "test.txt"), None)
    assert api_test is not None
    assert "content" not in api_test


def test_max_file_bytes_option(run_mapper, temp_project):
    large_file = temp_project / "large.txt"
    large_content = "x" * 1000
    large_file.write_text(large_content, encoding="utf-8")

    small_file = temp_project / "small.txt"
    small_content = "small"
    small_file.write_text(small_content, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--max-file-bytes", "100"])

    tree = load_yaml(output_file)

    large_found = False
    small_found = False

    for child in tree.get("children", []):
        if child.get("name") == "large.txt":
            content = child.get("content", "")
            assert "<file too large:" in content
            assert "bytes>" in content
            large_found = True
        elif child.get("name") == "small.txt":
            content = child.get("content", "")
            assert small_content in content
            small_found = True

    assert large_found and small_found

    api_tree = map_directory(temp_project, max_file_bytes=100)
    api_large = next((c for c in api_tree.get("children", []) if c.get("name") == "large.txt"), None)
    api_small = next((c for c in api_tree.get("children", []) if c.get("name") == "small.txt"), None)
    assert api_large is not None
    assert "<file too large:" in api_large.get("content", "")
    assert api_small is not None
    assert small_content in api_small.get("content", "")


def test_max_depth_zero(run_mapper, temp_project):
    (temp_project / "file1.txt").write_text("content1")
    (temp_project / "dir1").mkdir()

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--max-depth", "0"])

    tree = load_yaml(output_file)

    children = tree.get("children", [])
    assert len(children) == 0


def test_no_content_with_binary_files(run_mapper, temp_project):
    binary_file = temp_project / "binary.bin"
    binary_file.write_bytes(b"\x00\x01\x02\x03")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--no-content"])

    tree = load_yaml(output_file)

    binary_node = next((c for c in tree.get("children", []) if c.get("name") == "binary.bin"), None)
    assert binary_node is not None
    assert "content" not in binary_node


def test_combined_options(run_mapper, temp_project):
    (temp_project / "level1").mkdir()
    (temp_project / "level1" / "file1.txt").write_text("x" * 500)
    (temp_project / "level1" / "level2").mkdir()
    (temp_project / "level1" / "level2" / "file2.txt").write_text("content")

    output_file = temp_project / "output.yaml"

    assert run_mapper(
        [
            str(temp_project),
            "-o",
            str(output_file),
            "--max-depth",
            "2",
            "--max-file-bytes",
            "100",
            "--format",
            "json",
        ]
    )

    assert output_file.exists()

    import json

    with open(output_file, encoding="utf-8") as f:
        tree = json.load(f)

    assert tree["name"] == temp_project.name


def test_log_level_with_max_file_bytes(temp_project):
    large_file = temp_project / "large.txt"
    large_file.write_text("x" * 1000, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess(
        [
            str(temp_project),
            "-o",
            str(output_file),
            "--max-file-bytes",
            "100",
            "--log-level",
            "info",
        ]
    )

    assert result.returncode == 0

    assert "large" in result.stderr.lower() or "large" in result.stdout.lower()


def test_max_file_bytes_zero_is_error(temp_project):
    result = run_treemapper_subprocess([str(temp_project), "--max-file-bytes", "0"])
    assert result.returncode != 0
    assert "ambiguous" in result.stderr.lower()
    assert "--no-file-size-limit" in result.stderr


def test_no_file_size_limit_includes_all(run_mapper, temp_project):
    large_file = temp_project / "large.txt"
    large_content = "x" * 10000
    large_file.write_text(large_content, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--no-file-size-limit"])

    tree = load_yaml(output_file)

    for child in tree.get("children", []):
        if child.get("name") == "large.txt":
            content = child.get("content", "")
            assert "<file too large:" not in content
            assert large_content in content
            break


def test_default_max_file_bytes_limit(temp_project):
    from treemapper.cli import DEFAULT_MAX_FILE_BYTES

    assert DEFAULT_MAX_FILE_BYTES == 10 * 1024 * 1024


def test_known_binary_extension_detected(run_mapper, temp_project):
    pdf_file = temp_project / "document.pdf"
    pdf_file.write_text("not really a pdf but has extension")

    xlsx_file = temp_project / "spreadsheet.xlsx"
    xlsx_file.write_bytes(b"\x00\x01\x02")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file)])

    tree = load_yaml(output_file)

    pdf_node = next((c for c in tree.get("children", []) if c.get("name") == "document.pdf"), None)
    assert pdf_node is not None
    assert "<binary file:" in pdf_node.get("content", "")

    xlsx_node = next((c for c in tree.get("children", []) if c.get("name") == "spreadsheet.xlsx"), None)
    assert xlsx_node is not None
    assert "<binary file:" in xlsx_node.get("content", "")


def test_output_file_without_argument_uses_default_name(run_mapper, temp_project):
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    assert run_mapper([".", "--save"])

    default_output = temp_project / "tree.yaml"
    assert default_output.exists()

    tree = load_yaml(default_output)
    assert tree["name"] == temp_project.name


def test_output_file_without_argument_respects_format(run_mapper, temp_project):
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    import json

    for fmt, ext in [("json", "tree.json"), ("txt", "tree.txt"), ("md", "tree.md"), ("yaml", "tree.yaml")]:
        expected_file = temp_project / ext
        if expected_file.exists():
            expected_file.unlink()

        assert run_mapper([".", "--save", "-f", fmt]), f"Failed for format {fmt}"
        assert expected_file.exists(), f"Expected {expected_file} for format {fmt}"

        content = expected_file.read_text(encoding="utf-8")
        if fmt == "json":
            parsed = json.loads(content)
            assert parsed["name"] == temp_project.name
        elif fmt == "yaml":
            tree = load_yaml(expected_file)
            assert tree["name"] == temp_project.name

        expected_file.unlink()


def test_max_safe_file_size_constant():
    from treemapper.tree import MAX_SAFE_FILE_SIZE

    assert MAX_SAFE_FILE_SIZE == 100 * 1024 * 1024


def test_max_depth_zero_warning(temp_project):
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    result = run_treemapper_subprocess([str(temp_project), "--max-depth", "0"])

    assert result.returncode == 0
    assert "max-depth 0" in result.stderr.lower() or "empty tree" in result.stderr.lower()


def test_max_file_bytes_boundary_exact_limit(run_mapper, temp_project):
    exact_file = temp_project / "exact.txt"
    exact_file.write_text("x" * 100, encoding="utf-8")

    one_over_file = temp_project / "one_over.txt"
    one_over_file.write_text("x" * 101, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--max-file-bytes", "100"])

    tree = load_yaml(output_file)

    exact_node = next((c for c in tree.get("children", []) if c.get("name") == "exact.txt"), None)
    assert exact_node is not None
    assert "<file too large:" not in exact_node.get("content", "")

    over_node = next((c for c in tree.get("children", []) if c.get("name") == "one_over.txt"), None)
    assert over_node is not None
    assert "<file too large:" in over_node.get("content", "")


def test_max_file_bytes_one_byte_limit(run_mapper, temp_project):
    one_byte = temp_project / "one.txt"
    one_byte.write_text("a", encoding="utf-8")

    two_bytes = temp_project / "two.txt"
    two_bytes.write_text("ab", encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--max-file-bytes", "1"])

    tree = load_yaml(output_file)

    one_node = next((c for c in tree.get("children", []) if c.get("name") == "one.txt"), None)
    assert one_node is not None
    assert "<file too large:" not in one_node.get("content", "")

    two_node = next((c for c in tree.get("children", []) if c.get("name") == "two.txt"), None)
    assert two_node is not None
    assert "<file too large:" in two_node.get("content", "")


def test_no_content_with_max_file_bytes_combined(run_mapper, temp_project):
    small_file = temp_project / "small.txt"
    small_file.write_text("small content", encoding="utf-8")

    large_file = temp_project / "large.txt"
    large_file.write_text("x" * 1000, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper(
        [
            str(temp_project),
            "-o",
            str(output_file),
            "--no-content",
            "--max-file-bytes",
            "100",
        ]
    )

    tree = load_yaml(output_file)

    small_node = next((c for c in tree.get("children", []) if c.get("name") == "small.txt"), None)
    assert small_node is not None
    assert "content" not in small_node

    large_node = next((c for c in tree.get("children", []) if c.get("name") == "large.txt"), None)
    assert large_node is not None
    assert "content" not in large_node


def test_max_depth_large_value_acts_as_unlimited(run_mapper, temp_project):
    current = temp_project
    for i in range(5):
        current = current / f"d{i}"
        current.mkdir()
    (current / "deep.txt").write_text("found", encoding="utf-8")

    output_file = temp_project / "output.yaml"

    assert run_mapper([str(temp_project), "-o", str(output_file), "--max-depth", "999"])

    tree = load_yaml(output_file)

    from .utils import get_all_files_in_tree

    all_names = get_all_files_in_tree(tree)
    assert "deep.txt" in all_names
