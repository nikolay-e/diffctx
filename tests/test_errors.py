# tests/test_errors.py
import logging
import stat
import sys
from pathlib import Path

import pytest

from .conftest import IS_WSL
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
    assert "not a directory" in captured.err


@pytest.mark.skipif(
    sys.platform == "win32" or IS_WSL,
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
    sys.platform == "win32" or IS_WSL,
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


def test_output_path_is_directory(temp_project, run_mapper, capsys):
    """Test: output path (-o) points to existing directory."""
    output_should_be_file = temp_project / "i_am_a_directory"
    output_should_be_file.mkdir()

    assert not run_mapper([".", "-o", str(output_should_be_file)])
    captured = capsys.readouterr()

    assert "Error:" in captured.err
    assert "is a directory" in captured.err

    assert output_should_be_file.is_dir()
    assert not list(output_should_be_file.iterdir())


# --- CLI argument validation tests ---


def test_negative_max_depth(temp_project):
    from .conftest import run_treemapper_subprocess

    result = run_treemapper_subprocess([".", "--max-depth", "-1"], cwd=temp_project)
    assert result.returncode != 0
    assert "Error:" in result.stderr
    assert "max-depth" in result.stderr.lower()


def test_negative_max_file_bytes(temp_project):
    from .conftest import run_treemapper_subprocess

    result = run_treemapper_subprocess([".", "--max-file-bytes", "-1"], cwd=temp_project)
    assert result.returncode != 0
    assert "Error:" in result.stderr
    assert "max-file-bytes" in result.stderr.lower()


def test_oserror_accessing_directory(temp_project, monkeypatch, capsys):
    import sys

    from treemapper.cli import parse_args

    test_dir = temp_project / "testdir"
    test_dir.mkdir()

    original_resolve = Path.resolve

    def mock_resolve(self, strict=False):
        if strict and self.name == "testdir":
            raise OSError("Simulated OSError: disk I/O error")
        return original_resolve(self, strict=strict)

    monkeypatch.setattr(Path, "resolve", mock_resolve)
    monkeypatch.setattr(sys, "argv", ["treemapper", str(test_dir)])

    with pytest.raises(SystemExit) as exc_info:
        parse_args()

    assert exc_info.value.code == 1
    captured = capsys.readouterr()
    assert "Error:" in captured.err
    assert "Cannot access" in captured.err


# --- Logger coverage tests ---


def test_logger_with_existing_handlers(caplog):
    import logging

    from treemapper.logger import setup_logging

    root_logger = logging.getLogger()
    original_handlers = root_logger.handlers[:]
    original_level = root_logger.level

    try:
        test_handler = logging.StreamHandler()
        root_logger.addHandler(test_handler)

        setup_logging(3)

        assert test_handler.level == logging.DEBUG
        assert test_handler.formatter is not None
    finally:
        root_logger.handlers = original_handlers
        root_logger.setLevel(original_level)


def test_logger_with_handler_without_formatter():
    import logging

    from treemapper.logger import setup_logging

    root_logger = logging.getLogger()
    original_handlers = root_logger.handlers[:]
    original_level = root_logger.level

    try:
        root_logger.handlers.clear()
        test_handler = logging.StreamHandler()
        root_logger.addHandler(test_handler)

        setup_logging(2)

        assert test_handler.formatter is not None
    finally:
        root_logger.handlers = original_handlers
        root_logger.setLevel(original_level)


def test_logger_no_handlers():
    import logging

    from treemapper.logger import setup_logging

    root_logger = logging.getLogger()
    original_handlers = root_logger.handlers[:]
    original_level = root_logger.level

    try:
        root_logger.handlers.clear()

        setup_logging(1)

        assert len(root_logger.handlers) == 1
        assert root_logger.handlers[0].level == logging.WARNING
    finally:
        root_logger.handlers = original_handlers
        root_logger.setLevel(original_level)


# --- tree.py coverage tests ---


@pytest.mark.skipif(
    sys.platform == "win32" or IS_WSL,
    reason="os.chmod limited on Windows/WSL",
)
def test_unreadable_directory(temp_project, set_perms):
    from .conftest import run_treemapper_subprocess

    unreadable_dir = temp_project / "locked"
    unreadable_dir.mkdir()
    (unreadable_dir / "secret.txt").write_text("hidden", encoding="utf-8")
    set_perms(unreadable_dir, 0o000)

    output_path = temp_project / "output.yaml"
    result = run_treemapper_subprocess([".", "-o", str(output_path), "--log-level", "warning"], cwd=temp_project)

    assert result.returncode == 0
    assert "Permission denied" in result.stderr

    tree = load_yaml(output_path)
    dir_names = [c["name"] for c in tree.get("children", [])]
    assert "locked" in dir_names


def test_file_with_null_bytes_detected_as_binary(temp_project, run_mapper, caplog):
    file_with_nulls = temp_project / "with_nulls.txt"
    file_with_nulls.write_bytes(b"hello\x00world\x00test")

    output_path = temp_project / "output.yaml"
    with caplog.at_level(logging.WARNING):
        assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    file_node = find_node_by_path(result, ["with_nulls.txt"])

    assert file_node is not None
    assert "<binary file" in file_node.get("content", "")


def test_file_with_null_bytes_after_sample_size(temp_project):
    from .conftest import run_treemapper_subprocess

    file_with_late_null = temp_project / "late_null.txt"
    content = b"x" * 8200 + b"\x00" + b"y" * 100
    file_with_late_null.write_bytes(content)

    output_path = temp_project / "output.yaml"
    result = run_treemapper_subprocess([".", "-o", str(output_path), "--log-level", "warning"], cwd=temp_project)

    assert result.returncode == 0

    tree = load_yaml(output_path)
    file_node = find_node_by_path(tree, ["late_null.txt"])

    assert file_node is not None
    assert "<binary file:" in file_node.get("content", "")


def test_oserror_during_read(temp_project, run_mapper, monkeypatch, caplog):
    test_file = temp_project / "test.txt"
    test_file.write_text("test content", encoding="utf-8")

    original_open = Path.open

    def mock_open(self, *args, **kwargs):
        if self.name == "test.txt" and "rb" in args:
            raise OSError("Simulated OS error")
        return original_open(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", mock_open)

    output_path = temp_project / "output.yaml"
    with caplog.at_level(logging.ERROR):
        assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    file_node = find_node_by_path(result, ["test.txt"])

    assert file_node is not None
    assert "<unreadable content>" in file_node.get("content", "")


# --- ignore.py edge case tests ---


def test_read_ignore_file_nonexistent():
    from treemapper.ignore import read_ignore_file

    result = read_ignore_file(Path("/nonexistent/path/.gitignore"))
    assert result == []


def test_read_ignore_file_ioerror(temp_project, monkeypatch, caplog):
    from treemapper.ignore import read_ignore_file

    ignore_file = temp_project / ".testignore"
    ignore_file.write_text("*.txt\n")

    original_open = Path.open

    def mock_open(self, *args, **kwargs):
        if self.name == ".testignore":
            raise OSError("Simulated IO error")
        return original_open(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", mock_open)

    with caplog.at_level(logging.WARNING):
        result = read_ignore_file(ignore_file)

    assert result == []
    assert any("Could not read ignore file" in rec.message for rec in caplog.records)


def test_get_output_file_pattern_oserror(temp_project, monkeypatch):
    from treemapper.ignore import _get_output_file_pattern

    output_file = temp_project / "output.yaml"

    def mock_resolve(self):
        raise OSError("Simulated resolve error")

    monkeypatch.setattr(Path, "resolve", mock_resolve)

    result = _get_output_file_pattern(output_file, temp_project)
    assert result is None


# --- tree.py edge case tests ---


def test_is_output_file_oserror(temp_project, monkeypatch):
    import pathspec

    from treemapper.tree import TreeBuildContext

    output_file = temp_project / "output.yaml"
    spec = pathspec.PathSpec.from_lines("gitwildmatch", [])
    ctx = TreeBuildContext(
        base_dir=temp_project,
        combined_spec=spec,
        output_file=output_file,
    )

    test_entry = temp_project / "test.txt"
    test_entry.touch()

    original_resolve = Path.resolve

    def mock_resolve(self):
        if self.name in ("test.txt", "output.yaml"):
            raise OSError("Simulated resolve error")
        return original_resolve(self)

    monkeypatch.setattr(Path, "resolve", mock_resolve)

    assert ctx.is_output_file(test_entry) is False


def test_process_entry_oserror(temp_project, monkeypatch, caplog):
    import pathspec

    from treemapper.tree import TreeBuildContext, _process_entry

    test_file = temp_project / "problem.txt"
    test_file.write_text("content")

    spec = pathspec.PathSpec.from_lines("gitwildmatch", [])
    ctx = TreeBuildContext(base_dir=temp_project, combined_spec=spec)

    original_relative_to = Path.relative_to

    def mock_relative_to(self, *args, **kwargs):
        if self.name == "problem.txt":
            raise OSError("Simulated relative_to error")
        return original_relative_to(self, *args, **kwargs)

    monkeypatch.setattr(Path, "relative_to", mock_relative_to)

    with caplog.at_level(logging.WARNING):
        result = _process_entry(test_file, ctx, 0)

    assert result is None
    assert any("Could not process path" in rec.message for rec in caplog.records)


def test_create_node_exception(temp_project, monkeypatch, caplog):
    import pathspec

    from treemapper.tree import TreeBuildContext, _create_node

    test_file = temp_project / "broken.txt"
    test_file.write_text("content")

    spec = pathspec.PathSpec.from_lines("gitwildmatch", [])
    ctx = TreeBuildContext(base_dir=temp_project, combined_spec=spec)

    original_open = Path.open

    def mock_open(self, *args, **kwargs):
        if self.name == "broken.txt":
            raise OSError("Simulated open error")
        return original_open(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", mock_open)

    with caplog.at_level(logging.ERROR):
        result = _create_node(test_file, ctx, 0, is_dir=False)

    assert result is not None
    assert result["content"] == "<unreadable content>\n"
    assert any("Could not read" in rec.message for rec in caplog.records)
