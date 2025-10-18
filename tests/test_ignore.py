# tests/test_ignore.py
import logging
import os
import sys

import pytest

from .utils import get_all_files_in_tree, load_yaml


def test_custom_ignore(temp_project, run_mapper):
    """Test custom ignore patterns."""
    ignore_file = temp_project / "custom.ignore"
    ignore_file.write_text(
        """
# Ignore all Python files
*.py
# Ignore docs directory
docs/
# Ignore specific file
.gitignore
"""
    )
    assert run_mapper([".", "-i", str(ignore_file), "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)
    assert not any(isinstance(f, str) and f.endswith(".py") for f in all_files)
    assert "docs" not in all_files
    assert ".gitignore" not in all_files


def test_gitignore_patterns(temp_project, run_mapper):
    """Test .gitignore pattern handling."""
    (temp_project / ".gitignore").write_text("*.pyc\n__pycache__/\n")
    (temp_project / "src" / ".gitignore").write_text("local_only.py\n")
    (temp_project / "test.pyc").touch()
    (temp_project / "__pycache__").mkdir()
    (temp_project / "__pycache__" / "cachefile").touch()
    (temp_project / "src" / "local_only.py").touch()
    (temp_project / "src" / "allowed.py").touch()
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)
    assert "test.pyc" not in all_files
    assert "__pycache__" not in all_files
    assert "cachefile" not in all_files
    assert "local_only.py" not in all_files
    assert "allowed.py" in all_files


def test_symlinks_and_special_files(temp_project, run_mapper):
    """Test ignore patterns with symlinks and special files."""
    hidden_dir = temp_project / ".hidden_dir"
    hidden_file = temp_project / ".hidden_file"
    target_file = temp_project / "target.txt"
    symlink_file = temp_project / "link.txt"

    hidden_dir.mkdir()
    hidden_file.touch()
    target_file.write_text("target content")

    can_symlink = True
    if sys.platform == "win32":
        try:
            symlink_file.symlink_to(target_file, target_is_directory=False)
        except OSError:
            logging.warning("Could not create symlink on Windows, skipping symlink part.")
            can_symlink = False
        except AttributeError:
            logging.warning("Path.symlink_to not available, skipping symlink part.")
            can_symlink = False
    else:
        try:
            symlink_file.symlink_to(target_file)
        except OSError as e:
            logging.warning(f"Could not create symlink: {e}, skipping symlink part.")
            can_symlink = False

    (temp_project / ".treemapperignore").write_text(".*\n!.gitignore\n")
    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)
    assert ".hidden_dir" not in all_files
    assert ".hidden_file" not in all_files
    assert ".gitignore" in all_files
    assert "target.txt" in all_files
    if can_symlink:
        assert symlink_file.name not in all_files


def test_empty_and_invalid_ignores(temp_project, run_mapper):
    """Test handling of empty and invalid ignore files."""
    (temp_project / ".gitignore").write_text("")
    (temp_project / ".treemapperignore").write_text("\n\n# Just comments\n\n")
    (temp_project / "empty.ignore").write_text("")
    (temp_project / "invalid.ignore").write_text("[\ninvalid\npattern\n")
    assert run_mapper([".", "-o", "out_empty.yaml"])
    assert run_mapper(
        [
            ".",
            "-i",
            str(temp_project / "empty.ignore"),
            "-o",
            "out_custom_empty.yaml",
        ]
    )
    assert run_mapper(
        [
            ".",
            "-i",
            str(temp_project / "invalid.ignore"),
            "-o",
            "out_invalid.yaml",
        ]
    )
    assert run_mapper([".", "-i", "nonexistent.ignore", "-o", "out_nonexistent.yaml"])
    assert (temp_project / "out_nonexistent.yaml").exists()
    assert load_yaml(temp_project / "out_empty.yaml") is not None


def test_ignore_negation(temp_project, run_mapper):
    """Test: un-ignoring with '!'."""
    (temp_project / ".gitignore").write_text("*.log\n!important.log\n")
    (temp_project / "app.log").touch()
    (temp_project / "important.log").touch()
    (temp_project / "another.log").touch()
    output_path = temp_project / "negation_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)
    assert "app.log" not in all_files
    assert "another.log" not in all_files
    assert "important.log" in all_files, "Negated file 'important.log' was incorrectly ignored"


def test_ignore_double_star(temp_project, run_mapper, caplog):
    """Test: ignoring with '**'."""

    caplog.set_level(logging.DEBUG)
    (temp_project / ".gitignore").write_text("**/temp_files/\n")
    (temp_project / "a" / "temp_files").mkdir(parents=True)
    (temp_project / "a" / "temp_files" / "file1.tmp").touch()
    (temp_project / "b" / "c" / "temp_files").mkdir(parents=True)
    (temp_project / "b" / "c" / "temp_files" / "file2.tmp").touch()
    (temp_project / "a" / "other_dir").mkdir()
    (temp_project / "a" / "other_dir" / "file3.txt").touch()
    output_path = temp_project / "doublestar_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)
    all_names = get_all_files_in_tree(result)

    assert "temp_files" not in all_names, "'temp_files' dir should be ignored by **/temp_files/"
    assert "file1.tmp" not in all_names, "'file1.tmp' should be ignored (inside ignored dir)"
    assert "file2.tmp" not in all_names, "'file2.tmp' should be ignored (inside ignored dir)"
    assert "other_dir" in all_names, "'other_dir' should NOT be ignored"
    assert "file3.txt" in all_names, "'file3.txt' should NOT be ignored"


def test_ignore_output_file_itself(temp_project, run_mapper):
    """Test: ignoring the output file itself in different locations."""
    output_path1 = temp_project / "output1.yaml"
    assert run_mapper([".", "-o", str(output_path1)])
    result1 = load_yaml(output_path1)
    all_files1 = get_all_files_in_tree(result1)
    assert output_path1.name not in all_files1, f"Output file {output_path1.name} was not ignored in root"

    output_path2 = temp_project / "src" / "output2.yaml"
    assert run_mapper([".", "-o", str(output_path2)])
    result2 = load_yaml(output_path2)
    all_files2 = get_all_files_in_tree(result2)
    assert output_path2.name not in all_files2, f"Output file {output_path2.name} was not ignored in existing subdir"

    output_dir3 = temp_project / "output_dir"
    output_path3 = output_dir3 / "output3.yaml"

    output_dir3.mkdir(parents=True, exist_ok=True)
    assert run_mapper([".", "-o", str(output_path3)])
    assert output_path3.exists()
    result3 = load_yaml(output_path3)
    all_files3 = get_all_files_in_tree(result3)
    assert output_dir3.name in all_files3, f"Output directory {output_dir3.name} should be listed"
    assert output_path3.name not in all_files3, f"Output file {output_path3.name} was not ignored when in a new subdir"


def test_no_default_ignores_flag(temp_project, run_mapper):
    """Test: --no-default-ignores flag."""
    git_dir = temp_project / ".git"
    git_dir.mkdir(parents=True, exist_ok=True)
    (git_dir / "config").write_text("test")
    (temp_project / "ignored_by_git.pyc").touch()
    (temp_project / ".gitignore").write_text("*.pyc\n")

    treemapper_ignore = temp_project / ".tmignore_fortest"
    treemapper_ignore.write_text("treemapper_ignored/\n")
    (temp_project / "treemapper_ignored").mkdir()
    (temp_project / "treemapper_ignored" / "file.txt").touch()
    output_path = temp_project / "output_no_defaults.yaml"

    custom_ignore = temp_project / "custom_empty.ignore"
    custom_ignore.touch()
    assert run_mapper(
        [
            ".",
            "--no-default-ignores",
            "-i",
            str(custom_ignore),
            "-o",
            str(output_path),
        ]
    )
    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)
    assert ".git" in all_files, ".git directory should NOT be ignored with --no-default-ignores"
    assert "config" in all_files, "File inside .git should NOT be ignored with --no-default-ignores"
    assert "ignored_by_git.pyc" in all_files, "*.pyc from .gitignore should NOT be ignored with --no-default-ignores"
    assert "treemapper_ignored" in all_files, "'treemapper_ignored/' should NOT be ignored with --no-default-ignores"
    assert "file.txt" in all_files
    assert ".gitignore" in all_files
    assert ".treemapperignore" in all_files
    assert treemapper_ignore.name in all_files
    assert custom_ignore.name in all_files
    assert (
        output_path.name not in all_files
    ), f"Output file {output_path.name} should still be ignored even with --no-default-ignores"


@pytest.mark.skipif(
    sys.platform == "win32"
    or ("microsoft" in open("/proc/version", "r").read().lower() if os.path.exists("/proc/version") else False),
    reason="os.chmod limited on Windows/WSL",
)
def test_unreadable_ignore_file(temp_project, run_mapper, set_perms, caplog):
    """Test: .treemapperignore file has no read permissions."""
    ignore_file = temp_project / ".treemapperignore"
    ignore_file.write_text(".git/\n")
    set_perms(ignore_file, 0o000)

    with caplog.at_level(logging.WARNING):
        assert run_mapper([".", "-o", "directory_tree.yaml", "-v", "1"])

    assert any(
        "Could not read ignore file" in rec.message and ignore_file.name in rec.message
        for rec in caplog.records
        if rec.levelno >= logging.WARNING
    ), "Expected WARNING log about unreadable ignore file not found"


def test_bad_encoding_ignore_file(temp_project, run_mapper, caplog):
    """Test: .treemapperignore file has non-UTF8 encoding."""
    ignore_file = temp_project / ".treemapperignore"
    try:
        ignore_file.write_text(".git/\nпапка_игнор/\n", encoding="cp1251")
    except LookupError:
        pytest.skip("CP1251 codec not found")

    with caplog.at_level(logging.WARNING):
        assert run_mapper([".", "-o", "directory_tree.yaml", "-v", "1"])

    assert any(
        "Could not decode ignore file" in rec.message and ignore_file.name in rec.message and "UTF-8" in rec.message
        for rec in caplog.records
        if rec.levelno >= logging.WARNING
    ), "Expected WARNING log about undecodable ignore file not found"

    result = load_yaml(temp_project / "directory_tree.yaml")

    (temp_project / "папка_игнор").mkdir()

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)
    assert "папка_игнор" in all_files
