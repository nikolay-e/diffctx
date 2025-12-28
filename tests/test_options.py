# tests/test_options.py
from treemapper import map_directory

from .conftest import run_treemapper_subprocess
from .utils import load_yaml


def test_max_depth_option(temp_project):
    """Test --max-depth option limits traversal depth."""
    # Create nested directory structure
    (temp_project / "level1").mkdir()
    (temp_project / "level1" / "level2").mkdir()
    (temp_project / "level1" / "level2" / "level3").mkdir()
    (temp_project / "level1" / "level2" / "level3" / "deep.txt").write_text("deep")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--max-depth", "2"])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    # Should have level1
    level1 = None
    for child in tree.get("children", []):
        if child["name"] == "level1":
            level1 = child
            break

    assert level1 is not None
    assert "children" in level1

    # Should have level2
    level2 = None
    for child in level1.get("children", []):
        if child["name"] == "level2":
            level2 = child
            break

    assert level2 is not None

    # Should NOT have level3 due to max-depth=2
    if "children" in level2:
        level3_names = [c["name"] for c in level2["children"]]
        assert "level3" not in level3_names

    # Verify Python API max_depth works the same
    api_tree = map_directory(temp_project, max_depth=2)
    api_level1 = next((c for c in api_tree.get("children", []) if c["name"] == "level1"), None)
    assert api_level1 is not None
    api_level2 = next((c for c in api_level1.get("children", []) if c["name"] == "level2"), None)
    assert api_level2 is not None
    if "children" in api_level2:
        assert "level3" not in [c["name"] for c in api_level2["children"]]


def test_no_content_option(temp_project):
    """Test --no-content option excludes file contents."""
    # Create a file with content
    test_file = temp_project / "test.txt"
    test_file.write_text("This should not appear", encoding="utf-8")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--no-content"])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    # Find test.txt
    for child in tree.get("children", []):
        if child.get("name") == "test.txt":
            # Should not have content key
            assert "content" not in child
            break

    # Verify Python API no_content works the same
    api_tree = map_directory(temp_project, no_content=True)
    api_test = next((c for c in api_tree.get("children", []) if c.get("name") == "test.txt"), None)
    assert api_test is not None
    assert "content" not in api_test


def test_max_file_bytes_option(temp_project):
    """Test --max-file-bytes option limits file reading."""
    # Create a large file
    large_file = temp_project / "large.txt"
    large_content = "x" * 1000  # 1000 bytes
    large_file.write_text(large_content, encoding="utf-8")

    # Create a small file
    small_file = temp_project / "small.txt"
    small_content = "small"
    small_file.write_text(small_content, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--max-file-bytes", "100"])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    # Check large file has placeholder
    large_found = False
    small_found = False

    for child in tree.get("children", []):
        if child.get("name") == "large.txt":
            content = child.get("content", "")
            assert "<file too large:" in content
            assert "1000 bytes>" in content or "1001 bytes>" in content  # May include newline
            large_found = True
        elif child.get("name") == "small.txt":
            content = child.get("content", "")
            assert small_content in content
            small_found = True

    assert large_found and small_found

    # Verify Python API max_file_bytes works the same
    api_tree = map_directory(temp_project, max_file_bytes=100)
    api_large = next((c for c in api_tree.get("children", []) if c.get("name") == "large.txt"), None)
    api_small = next((c for c in api_tree.get("children", []) if c.get("name") == "small.txt"), None)
    assert api_large is not None
    assert "<file too large:" in api_large.get("content", "")
    assert api_small is not None
    assert small_content in api_small.get("content", "")


def test_max_depth_zero(temp_project):
    """Test --max-depth 0 only shows root directory."""
    # Create some files
    (temp_project / "file1.txt").write_text("content1")
    (temp_project / "dir1").mkdir()

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--max-depth", "0"])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    # Should have no children due to max-depth=0
    children = tree.get("children", [])
    assert len(children) == 0


def test_no_content_with_binary_files(temp_project):
    """Test --no-content with binary files."""
    # Create a binary file
    binary_file = temp_project / "binary.bin"
    binary_file.write_bytes(b"\x00\x01\x02\x03")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--no-content"])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    # Binary file should be listed but without content
    for child in tree.get("children", []):
        if child.get("name") == "binary.bin":
            assert "content" not in child
            break


def test_combined_options(temp_project):
    """Test combining multiple options."""
    # Create nested structure
    (temp_project / "level1").mkdir()
    (temp_project / "level1" / "file1.txt").write_text("x" * 500)
    (temp_project / "level1" / "level2").mkdir()
    (temp_project / "level1" / "level2" / "file2.txt").write_text("content")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess(
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

    assert result.returncode == 0
    assert output_file.exists()

    # Should be valid JSON
    import json

    with open(output_file, encoding="utf-8") as f:
        tree = json.load(f)

    assert tree["name"] == temp_project.name


def test_log_level_with_max_file_bytes(temp_project):
    """Test verbose output when skipping large files."""
    # Create a large file
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

    # Should log info about skipping large file
    assert "large" in result.stderr.lower() or "large" in result.stdout.lower()


def test_max_file_bytes_zero_means_unlimited(temp_project):
    """Test --max-file-bytes 0 disables file size limit."""
    large_file = temp_project / "large.txt"
    large_content = "x" * 10000
    large_file.write_text(large_content, encoding="utf-8")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--max-file-bytes", "0"])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    for child in tree.get("children", []):
        if child.get("name") == "large.txt":
            content = child.get("content", "")
            assert "<file too large:" not in content
            assert large_content in content
            break


def test_default_max_file_bytes_limit(temp_project):
    """Test default 10 MB limit is applied."""
    from treemapper.cli import DEFAULT_MAX_FILE_BYTES

    assert DEFAULT_MAX_FILE_BYTES == 10 * 1024 * 1024


def test_known_binary_extension_detected(temp_project):
    """Test files with known binary extensions are detected without reading."""
    from treemapper.tree import KNOWN_BINARY_EXTENSIONS

    pdf_file = temp_project / "document.pdf"
    pdf_file.write_text("not really a pdf but has extension")

    output_file = temp_project / "output.yaml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file)])

    assert result.returncode == 0

    tree = load_yaml(output_file)

    for child in tree.get("children", []):
        if child.get("name") == "document.pdf":
            content = child.get("content", "")
            assert "<binary file:" in content
            break

    assert ".pdf" in KNOWN_BINARY_EXTENSIONS
    assert ".xlsx" in KNOWN_BINARY_EXTENSIONS
    assert ".jpg" in KNOWN_BINARY_EXTENSIONS


def test_output_file_without_argument_uses_default_name(temp_project):
    """Test -o without filename creates tree.{format}."""
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    result = run_treemapper_subprocess([str(temp_project), "-o"], cwd=temp_project)
    assert result.returncode == 0

    default_output = temp_project / "tree.yaml"
    assert default_output.exists()

    tree = load_yaml(default_output)
    assert tree["name"] == temp_project.name


def test_output_file_without_argument_respects_format(temp_project):
    """Test -o without filename uses correct extension for format."""
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    import json

    for fmt, ext in [("json", "tree.json"), ("txt", "tree.txt"), ("md", "tree.md"), ("yaml", "tree.yaml"), ("yml", "tree.yaml")]:
        expected_file = temp_project / ext
        if expected_file.exists():
            expected_file.unlink()

        result = run_treemapper_subprocess([str(temp_project), "-o", "-f", fmt], cwd=temp_project)
        assert result.returncode == 0, f"Failed for format {fmt}: {result.stderr}"
        assert expected_file.exists(), f"Expected {expected_file} for format {fmt}"

        content = expected_file.read_text(encoding="utf-8")
        if fmt == "json":
            parsed = json.loads(content)
            assert parsed["name"] == temp_project.name
        elif fmt in ("yaml", "yml"):
            tree = load_yaml(expected_file)
            assert tree["name"] == temp_project.name

        expected_file.unlink()


def test_yml_format_alias(temp_project):
    """Test yml format is treated same as yaml."""
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    output_file = temp_project / "output.yml"

    result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "-f", "yml"])
    assert result.returncode == 0
    assert output_file.exists()

    tree = load_yaml(output_file)
    assert tree["name"] == temp_project.name
    assert tree["type"] == "directory"


def test_max_safe_file_size_constant():
    """Test MAX_SAFE_FILE_SIZE is defined as 100 MB."""
    from treemapper.tree import MAX_SAFE_FILE_SIZE

    assert MAX_SAFE_FILE_SIZE == 100 * 1024 * 1024


def test_max_depth_zero_warning(temp_project):
    """Test warning is shown when --max-depth 0 is used."""
    (temp_project / "test.txt").write_text("content", encoding="utf-8")

    result = run_treemapper_subprocess([str(temp_project), "--max-depth", "0"])

    assert result.returncode == 0
    assert "max-depth 0" in result.stderr.lower() or "empty tree" in result.stderr.lower()
