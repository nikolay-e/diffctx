from treemapper.treemapper import main
import os
import sys

import pytest
import yaml

# Add the src directory to the Python path
sys.path.insert(
    0,
    os.path.abspath(
        os.path.join(
            os.path.dirname(__file__),
            '..',
            'src')))


@pytest.fixture
def complex_directory(tmp_path):
    (tmp_path / "dir1").mkdir()
    (tmp_path / "dir1" / "file1.txt").write_text("Content of file1")
    (tmp_path / "dir1" / "file2.py").write_text("print('Hello from file2')")
    (tmp_path / "dir2").mkdir()
    (tmp_path / "dir2" / "subdir").mkdir()
    (tmp_path / "dir2" / "subdir" / "file3.log").write_text("Log content")
    (tmp_path / "dir2" / "file4.txt").write_text("Content of file4")
    (tmp_path / "dir2" / "file5.class").write_text("Compiled class content")
    (tmp_path / ".gitignore").write_text("*.log\n")
    (tmp_path / ".treemapperignore").write_text("*.txt\ndir1\n*.class\n")
    (tmp_path / ".git").mkdir()
    return tmp_path


def test_mixed_ignore_patterns(complex_directory, tmp_path, monkeypatch):
    # Adjust the .treemapperignore to ensure .py files aren't ignored
    ignore_file = complex_directory / ".treemapperignore"
    ignore_file.write_text("*.class\n*.txt\n")  # Ensure '*.py' is not ignored

    output_file = tmp_path / "output.yaml"
    monkeypatch.setattr('sys.argv', ['treemapper', str(
        complex_directory), '-o', str(output_file)])
    main()
    content = output_file.read_text()
    assert "file2.py" in content  # Ensure non-ignored files are included
    assert "file1.txt" not in content  # Ensure ignored files are excluded
