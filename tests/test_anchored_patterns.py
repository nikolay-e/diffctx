# tests/test_anchored_patterns.py
import sys  # Required for platform checks
from pathlib import Path  # Path operations in tests

import pytest  # Used for test decorators

from .utils import find_node_by_path, get_all_files_in_tree, load_yaml

# Skip tests on Windows since they may behave differently
skip_on_windows = pytest.mark.skipif(sys.platform == "win32", reason="Skipping on Windows")


# Check path resolution with Path
def _is_valid_path(p: str) -> bool:
    """Verify path is valid using Path."""
    return Path(p).exists()


def test_anchored_pattern_fix(temp_project, run_mapper):
    """Test for the fixed handling of anchored patterns in gitignore."""
    # Create a clean directory for this test
    test_dir = temp_project / "anchored_test_dir"
    test_dir.mkdir()

    # Create test files
    (test_dir / "root_file.txt").touch()
    (test_dir / "subdir").mkdir()
    (test_dir / "subdir" / "nested_file.txt").touch()
    (test_dir / "subdir" / "root_file.txt").touch()  # Same name as root file

    # Create gitignore with anchored pattern to ignore only root file
    (test_dir / ".gitignore").write_text("/root_file.txt\n")

    # Run TreeMapper on the clean test directory
    output_path = temp_project / "anchored_output.yaml"
    assert run_mapper([str(test_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    # The root should be the test directory
    assert result["name"] == test_dir.name

    # Check if anchored pattern correctly ignores only root file
    all_files = get_all_files_in_tree(result)

    # .gitignore should be included
    assert ".gitignore" in all_files

    # Root file should be ignored
    root_node = find_node_by_path(result, ["root_file.txt"])
    assert root_node is None, "Anchored pattern failed to ignore root file"

    # Files in subdirectories shouldn't be affected by anchored pattern
    subdir_node = find_node_by_path(result, ["subdir"])
    assert subdir_node is not None, "Subdir not found in output"

    # Direct check for subdir/root_file.txt
    nested_file = find_node_by_path(result, ["subdir", "root_file.txt"])
    assert nested_file is not None, "Anchored pattern incorrectly ignored file in subdirectory"


def test_non_anchored_pattern(temp_project, run_mapper):
    """Test non-anchored pattern that should ignore files in all directories."""
    # Create a clean directory for this test
    test_dir = temp_project / "non_anchored_test_dir"
    test_dir.mkdir()

    # Create test files
    (test_dir / "file.log").touch()
    (test_dir / "subdir").mkdir()
    (test_dir / "subdir" / "file.log").touch()
    (test_dir / "subdir" / "data.txt").touch()
    (test_dir / "data.txt").touch()

    # Create gitignore with non-anchored pattern
    (test_dir / ".gitignore").write_text("*.log\n")

    # Run TreeMapper
    output_path = temp_project / "non_anchored_output.yaml"
    assert run_mapper([str(test_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    # The root should be the test directory
    assert result["name"] == test_dir.name

    # Log files should be ignored in all directories
    log_file = find_node_by_path(result, ["file.log"])
    assert log_file is None, "Non-anchored pattern failed to ignore root log file"

    # Regular files should be included
    data_txt = find_node_by_path(result, ["data.txt"])
    assert data_txt is not None, "Regular file incorrectly ignored"

    # Check nested directory
    subdir_node = find_node_by_path(result, ["subdir"])
    assert subdir_node is not None

    # Nested log file should be ignored
    nested_log = find_node_by_path(result, ["subdir", "file.log"])
    assert nested_log is None, "Non-anchored pattern failed to ignore nested file"

    # Nested regular file should be included
    nested_txt = find_node_by_path(result, ["subdir", "data.txt"])
    assert nested_txt is not None, "Regular file incorrectly ignored"


def test_combined_patterns(temp_project, run_mapper):
    """Test combination of anchored and non-anchored patterns."""
    # Create a clean directory for this test
    test_dir = temp_project / "combined_test_dir"
    test_dir.mkdir()

    # Create test files
    (test_dir / "root_only.txt").touch()
    (test_dir / "all_dirs.log").touch()
    (test_dir / "regular.txt").touch()
    (test_dir / "subdir").mkdir()
    (test_dir / "subdir" / "root_only.txt").touch()
    (test_dir / "subdir" / "all_dirs.log").touch()
    (test_dir / "subdir" / "regular.txt").touch()

    # Create gitignore with anchored and non-anchored patterns
    (test_dir / ".gitignore").write_text("/root_only.txt\n*.log\n")

    # Run TreeMapper
    output_path = temp_project / "combined_output.yaml"
    assert run_mapper([str(test_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    # The root should be the test directory
    assert result["name"] == test_dir.name

    # Check gitignore effects

    # Root anchored file should be ignored
    root_only = find_node_by_path(result, ["root_only.txt"])
    assert root_only is None, "Anchored pattern failed to ignore root file"

    # Log files should be ignored in all directories
    all_dirs_log = find_node_by_path(result, ["all_dirs.log"])
    assert all_dirs_log is None, "Non-anchored pattern failed to ignore root log file"

    # Regular files should be included
    regular_txt = find_node_by_path(result, ["regular.txt"])
    assert regular_txt is not None, "Regular file incorrectly ignored"

    # Check nested directory
    subdir_node = find_node_by_path(result, ["subdir"])
    assert subdir_node is not None

    # Nested "root_only.txt" should NOT be ignored by anchored pattern
    nested_root_only = find_node_by_path(result, ["subdir", "root_only.txt"])
    assert nested_root_only is not None, "Anchored pattern incorrectly ignored file in subdirectory"

    # Nested log file should be ignored
    nested_log = find_node_by_path(result, ["subdir", "all_dirs.log"])
    assert nested_log is None, "Non-anchored pattern failed to ignore nested file"

    # Nested regular file should be included
    nested_txt = find_node_by_path(result, ["subdir", "regular.txt"])
    assert nested_txt is not None, "Regular file incorrectly ignored"
