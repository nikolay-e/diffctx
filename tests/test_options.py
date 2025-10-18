# tests/test_options.py
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

    with open(output_file, "r", encoding="utf-8") as f:
        tree = json.load(f)

    assert tree["name"] == temp_project.name


def test_verbosity_with_max_file_bytes(temp_project):
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
            "-v",
            "2",  # INFO level
        ]
    )

    assert result.returncode == 0

    # Should log info about skipping large file
    assert "large" in result.stderr.lower() or "large" in result.stdout.lower()
