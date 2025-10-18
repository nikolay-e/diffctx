# tests/test_errors.py
import logging
import os
import stat
import sys
from pathlib import Path

import pytest

from .utils import find_node_by_path, load_yaml

# --- Tests for invalid input ---


def test_invalid_directory_path(run_mapper, capsys):
    """Test: non-existent directory specified."""
    dir_name = "non_existent_directory"
    assert not run_mapper([dir_name])
    captured = capsys.readouterr()

    assert "Error:" in captured.err
    assert f"'{dir_name}'" in captured.err or f"{Path(dir_name).resolve()}" in captured.err
    assert "does not exist" in captured.err or "not a valid directory" in captured.err


def test_input_path_is_file(run_mapper, temp_project, capsys):
    """Test: file specified instead of directory."""
    file_path = temp_project / "some_file.txt"
    file_path.touch()
    assert not run_mapper([str(file_path)])
    captured = capsys.readouterr()

    assert "Error:" in captured.err
    assert str(file_path.resolve()) in captured.err
    assert "not a valid directory" in captured.err


@pytest.mark.skipif(
    sys.platform == "win32"
    or ("microsoft" in open("/proc/version", "r").read().lower() if os.path.exists("/proc/version") else False),
    reason="os.chmod limited on Windows/WSL",
)
def test_unreadable_file(temp_project, run_mapper, set_perms, caplog):
    """Test: file without read permissions."""
    unreadable_file = temp_project / "unreadable.txt"
    unreadable_file.write_text("secret")
    set_perms(unreadable_file, 0o000)
    output_path = temp_project / "output_unreadable.yaml"
    with caplog.at_level(logging.ERROR):
        assert run_mapper([".", "-o", str(output_path)])
    assert output_path.exists(), f"Output file {output_path} was not created"
    result = load_yaml(output_path)
    file_node = find_node_by_path(result, ["unreadable.txt"])
    assert file_node is not None, "'unreadable.txt' node not found in generated YAML"
    assert file_node.get("type") == "file"
    assert file_node.get("content") == "<unreadable content>\n"
    assert any(
        "Could not read" in record.message and "unreadable.txt" in record.message
        for record in caplog.records
        if record.levelno >= logging.ERROR
    ), "Expected ERROR log message about reading failure not found"


@pytest.mark.skipif(
    sys.platform == "win32"
    or ("microsoft" in open("/proc/version", "r").read().lower() if os.path.exists("/proc/version") else False),
    reason="os.chmod limited on Windows/WSL",
)
def test_unwritable_output_dir(temp_project, run_mapper, set_perms, caplog):
    """Test: attempt to write to directory without write permissions."""
    unwritable_dir = temp_project / "locked_dir"
    unwritable_dir.mkdir()
    read_execute_perms = stat.S_IRUSR | stat.S_IXUSR | stat.S_IRGRP | stat.S_IXGRP | stat.S_IROTH | stat.S_IXOTH
    set_perms(unwritable_dir, read_execute_perms)
    output_path = unwritable_dir / "output.yaml"
    with caplog.at_level(logging.ERROR):
        run_mapper([".", "-o", str(output_path)])
    assert any(
        "Unable to write to file" in record.message and str(output_path) in record.message
        for record in caplog.records
        if record.levelno >= logging.ERROR
    ), f"Expected ERROR log message about writing failure to {output_path} not found"
    assert not output_path.exists()


def test_output_path_is_directory(temp_project, run_mapper, caplog):
    """Test: output path (-o) points to existing directory."""
    output_should_be_file = temp_project / "i_am_a_directory"
    output_should_be_file.mkdir()

    with caplog.at_level(logging.ERROR):
        run_mapper([".", "-o", str(output_should_be_file)])

    assert any(
        "Unable to write to file" in rec.message and str(output_should_be_file) in rec.message
        for rec in caplog.records
        if rec.levelno >= logging.ERROR
    ), f"Expected ERROR log message about writing failure to directory {output_should_be_file} not found"

    assert output_should_be_file.is_dir()
    assert not list(output_should_be_file.iterdir())
