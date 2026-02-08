# tests/test_ignore.py
import logging
import sys

import pytest

from .conftest import IS_WSL
from .utils import get_all_files_in_tree, load_yaml


def test_custom_ignore(temp_project, run_mapper):
    """Test custom ignore patterns."""
    ignore_file = temp_project / "custom.ignore"
    ignore_file.write_text("""
# Ignore all Python files
*.py
# Ignore docs directory
docs/
# Ignore specific file
.gitignore
""")
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
    assert not run_mapper([".", "-i", "nonexistent.ignore", "-o", "out_nonexistent.yaml"])
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
    sys.platform == "win32" or IS_WSL,
    reason="os.chmod limited on Windows/WSL",
)
def test_unreadable_ignore_file(temp_project, run_mapper, set_perms, caplog):
    """Test: .treemapperignore file has no read permissions."""
    ignore_file = temp_project / ".treemapperignore"
    ignore_file.write_text(".git/\n")
    set_perms(ignore_file, 0o000)

    with caplog.at_level(logging.WARNING):
        assert run_mapper([".", "-o", "directory_tree.yaml", "--log-level", "warning"])

    assert any(
        "Could not read ignore file" in rec.message and ignore_file.name in rec.message
        for rec in caplog.records
        if rec.levelno >= logging.WARNING
    ), "Expected WARNING log about unreadable ignore file not found"


def test_bad_encoding_ignore_file(temp_project, run_mapper, caplog):
    """Test: .treemapperignore file has non-UTF8 encoding."""
    ignore_file = temp_project / ".treemapperignore"
    try:
        # Cyrillic text intentional for CP1251 encoding test
        ignore_file.write_text(".git/\nпапка_игнор/\n", encoding="cp1251")  # noqa: RUF001
    except LookupError:
        pytest.skip("CP1251 codec not found")

    with caplog.at_level(logging.WARNING):
        assert run_mapper([".", "-o", "directory_tree.yaml", "--log-level", "warning"])

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


# --- Advanced ignore pattern tests (merged from test_ignore_advanced.py) ---


def test_ignore_precedence_subdir_over_root(temp_project, run_mapper):
    (temp_project / ".gitignore").write_text("*.txt\n")
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / ".gitignore").write_text("!allow.txt\n")
    (temp_project / "root.txt").touch()
    (temp_project / "subdir" / "ignore.txt").touch()
    (temp_project / "subdir" / "allow.txt").touch()

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    assert "root.txt" not in all_files
    assert "ignore.txt" not in all_files
    assert "allow.txt" in all_files


def test_ignore_precedence_treemapper_over_git(temp_project, run_mapper):
    (temp_project / ".gitignore").write_text("*.pyc\n")
    (temp_project / ".treemapperignore").write_text("*.log\n.git/\n")
    (temp_project / "file.pyc").touch()
    (temp_project / "file.log").touch()
    (temp_project / "file.txt").touch()

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    assert "file.pyc" not in all_files
    assert "file.log" not in all_files
    assert "file.txt" in all_files


def test_ignore_precedence_custom_over_defaults(temp_project, run_mapper):
    (temp_project / ".gitignore").write_text("*.pyc\n")
    (temp_project / ".treemapperignore").write_text("*.log\n.git/\n")
    custom_ignore = temp_project / "custom.ignore"
    custom_ignore.write_text("*.tmp\n")

    (temp_project / "file.pyc").touch()
    (temp_project / "file.log").touch()
    (temp_project / "file.tmp").touch()
    (temp_project / "file.txt").touch()

    assert run_mapper([".", "-i", str(custom_ignore), "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    assert "file.pyc" not in all_files
    assert "file.log" not in all_files
    assert "file.tmp" not in all_files
    assert "file.txt" in all_files


def test_ignore_interaction_combined_vs_git(temp_project, run_mapper):
    output_path = temp_project / "output.yaml"
    (temp_project / ".gitignore").write_text("!output.yaml\n")

    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)

    assert output_path.name not in all_files


def test_ignore_patterns_anchored(temp_project, run_mapper, caplog):
    from .utils import find_node_by_path

    caplog.set_level(logging.DEBUG)
    (temp_project / ".gitignore").write_text("/root_ignore.txt\nsubdir_ignore.txt")
    (temp_project / "root_ignore.txt").touch()
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / "root_ignore.txt").touch()
    (temp_project / "subdir" / "subdir_ignore.txt").touch()
    (temp_project / "subdir_ignore.txt").touch()

    assert run_mapper([".", "-o", "anchored_output.yaml"])
    result = load_yaml(temp_project / "anchored_output.yaml")

    assert find_node_by_path(result, ["root_ignore.txt"]) is None
    assert find_node_by_path(result, ["subdir", "root_ignore.txt"]) is not None
    assert find_node_by_path(result, ["subdir", "subdir_ignore.txt"]) is None
    assert find_node_by_path(result, ["subdir_ignore.txt"]) is None


# --- Anchored pattern tests (merged from test_anchored_patterns.py) ---


def test_anchored_pattern_fix(temp_project, run_mapper):
    from .utils import find_node_by_path

    test_dir = temp_project / "anchored_test_dir"
    test_dir.mkdir()

    (test_dir / "root_file.txt").touch()
    (test_dir / "subdir").mkdir()
    (test_dir / "subdir" / "nested_file.txt").touch()
    (test_dir / "subdir" / "root_file.txt").touch()
    (test_dir / ".gitignore").write_text("/root_file.txt\n")

    output_path = temp_project / "anchored_output.yaml"
    assert run_mapper([str(test_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == test_dir.name
    assert find_node_by_path(result, ["root_file.txt"]) is None
    assert find_node_by_path(result, ["subdir", "root_file.txt"]) is not None


def test_non_anchored_pattern(temp_project, run_mapper):
    from .utils import find_node_by_path

    test_dir = temp_project / "non_anchored_test_dir"
    test_dir.mkdir()

    (test_dir / "file.log").touch()
    (test_dir / "subdir").mkdir()
    (test_dir / "subdir" / "file.log").touch()
    (test_dir / "subdir" / "data.txt").touch()
    (test_dir / "data.txt").touch()
    (test_dir / ".gitignore").write_text("*.log\n")

    output_path = temp_project / "non_anchored_output.yaml"
    assert run_mapper([str(test_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == test_dir.name
    assert find_node_by_path(result, ["file.log"]) is None
    assert find_node_by_path(result, ["data.txt"]) is not None
    assert find_node_by_path(result, ["subdir", "file.log"]) is None
    assert find_node_by_path(result, ["subdir", "data.txt"]) is not None


def test_combined_patterns(temp_project, run_mapper):
    from .utils import find_node_by_path

    test_dir = temp_project / "combined_test_dir"
    test_dir.mkdir()

    (test_dir / "root_only.txt").touch()
    (test_dir / "all_dirs.log").touch()
    (test_dir / "regular.txt").touch()
    (test_dir / "subdir").mkdir()
    (test_dir / "subdir" / "root_only.txt").touch()
    (test_dir / "subdir" / "all_dirs.log").touch()
    (test_dir / "subdir" / "regular.txt").touch()
    (test_dir / ".gitignore").write_text("/root_only.txt\n*.log\n")

    output_path = temp_project / "combined_output.yaml"
    assert run_mapper([str(test_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == test_dir.name
    assert find_node_by_path(result, ["root_only.txt"]) is None
    assert find_node_by_path(result, ["all_dirs.log"]) is None
    assert find_node_by_path(result, ["regular.txt"]) is not None
    assert find_node_by_path(result, ["subdir", "root_only.txt"]) is not None
    assert find_node_by_path(result, ["subdir", "all_dirs.log"]) is None
    assert find_node_by_path(result, ["subdir", "regular.txt"]) is not None


def test_hierarchical_treemapperignore(temp_project, run_mapper):
    from .utils import find_node_by_path

    (temp_project / ".treemapperignore").write_text("*.tmp\n")
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / ".treemapperignore").write_text("*.bak\n!important.bak\n")
    (temp_project / "root.tmp").touch()
    (temp_project / "root.bak").touch()
    (temp_project / "subdir" / "nested.tmp").touch()
    (temp_project / "subdir" / "nested.bak").touch()
    (temp_project / "subdir" / "important.bak").touch()
    (temp_project / "subdir" / "keep.txt").touch()

    output_path = temp_project / "hierarchical_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)

    assert find_node_by_path(result, ["root.tmp"]) is None
    assert find_node_by_path(result, ["root.bak"]) is not None
    assert find_node_by_path(result, ["subdir", "nested.tmp"]) is None
    assert find_node_by_path(result, ["subdir", "nested.bak"]) is None
    assert find_node_by_path(result, ["subdir", "important.bak"]) is not None
    assert find_node_by_path(result, ["subdir", "keep.txt"]) is not None


def test_hierarchical_ignore_applies_recursively(temp_project, run_mapper):
    from .utils import find_node_by_path

    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / "deep").mkdir()
    (temp_project / "subdir" / "deep" / "deeper").mkdir()
    (temp_project / "subdir" / ".gitignore").write_text("*.bak\n")

    (temp_project / "subdir" / "level1.bak").touch()
    (temp_project / "subdir" / "level1.txt").touch()
    (temp_project / "subdir" / "deep" / "level2.bak").touch()
    (temp_project / "subdir" / "deep" / "level2.txt").touch()
    (temp_project / "subdir" / "deep" / "deeper" / "level3.bak").touch()
    (temp_project / "subdir" / "deep" / "deeper" / "level3.txt").touch()

    (temp_project / "root.bak").touch()

    output_path = temp_project / "recursive_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)

    assert find_node_by_path(result, ["root.bak"]) is not None
    assert find_node_by_path(result, ["subdir", "level1.bak"]) is None
    assert find_node_by_path(result, ["subdir", "level1.txt"]) is not None
    assert find_node_by_path(result, ["subdir", "deep", "level2.bak"]) is None
    assert find_node_by_path(result, ["subdir", "deep", "level2.txt"]) is not None
    assert find_node_by_path(result, ["subdir", "deep", "deeper", "level3.bak"]) is None
    assert find_node_by_path(result, ["subdir", "deep", "deeper", "level3.txt"]) is not None


def test_prune_dirs_skipped_during_aggregation():
    from treemapper.ignore import PRUNE_DIRS

    assert ".git" in PRUNE_DIRS
    assert "node_modules" in PRUNE_DIRS
    assert "__pycache__" in PRUNE_DIRS
    assert "venv" in PRUNE_DIRS
    assert ".venv" in PRUNE_DIRS


def test_is_cache_dir_function():
    from treemapper.ignore import _is_cache_dir

    assert _is_cache_dir(".pytest_cache") is True
    assert _is_cache_dir(".mypy_cache") is True
    assert _is_cache_dir(".ruff_cache") is True

    assert _is_cache_dir("pytest_cache") is False
    assert _is_cache_dir("user_cache") is False
    assert _is_cache_dir("api_cache_manager") is False
    assert _is_cache_dir(".cache") is False
    assert _is_cache_dir("cache") is False


def test_aggregate_all_ignore_patterns_single_walk(tmp_path):
    from treemapper.ignore import _aggregate_all_ignore_patterns

    root = tmp_path / "project"
    root.mkdir()
    (root / ".gitignore").write_text("*.pyc\n")
    (root / ".treemapperignore").write_text("*.log\n")
    (root / "subdir").mkdir()
    (root / "subdir" / ".gitignore").write_text("local.txt\n")

    patterns = _aggregate_all_ignore_patterns(root, [".gitignore", ".treemapperignore"])

    assert "*.pyc" in patterns
    assert "*.log" in patterns
    assert any("local.txt" in p for p in patterns)


def test_aggregate_all_ignore_patterns_empty_dir(tmp_path):
    from treemapper.ignore import _aggregate_all_ignore_patterns

    root = tmp_path / "empty_project"
    root.mkdir()

    patterns = _aggregate_all_ignore_patterns(root, [".gitignore", ".treemapperignore"])

    assert patterns == []


def test_process_ignore_line_anchored():
    from treemapper.ignore import _process_ignore_line

    assert _process_ignore_line("/root.txt", "") == "/root.txt"
    assert _process_ignore_line("/root.txt", "subdir") == "/subdir/root.txt"

    assert _process_ignore_line("dir/file.txt", "") == "/dir/file.txt"
    assert _process_ignore_line("dir/file.txt", "sub") == "/sub/dir/file.txt"


def test_process_ignore_line_unanchored():
    from treemapper.ignore import _process_ignore_line

    assert _process_ignore_line("*.log", "") == "*.log"
    assert _process_ignore_line("*.log", "subdir") == "subdir/**/*.log"


def test_process_ignore_line_negation():
    from treemapper.ignore import _process_ignore_line

    assert _process_ignore_line("!important.txt", "") == "!important.txt"
    assert _process_ignore_line("!/root.txt", "sub") == "!/sub/root.txt"


def test_cache_dirs_not_traversed_during_aggregation(tmp_path):
    from treemapper.ignore import _aggregate_all_ignore_patterns

    root = tmp_path / "project"
    root.mkdir()
    (root / ".pytest_cache").mkdir()
    (root / ".pytest_cache" / ".gitignore").write_text("should_not_be_found\n")
    (root / ".gitignore").write_text("*.pyc\n")

    patterns = _aggregate_all_ignore_patterns(root, [".gitignore"])

    assert "*.pyc" in patterns
    assert not any("should_not_be_found" in p for p in patterns)


def test_parent_directory_ignore_patterns(tmp_path):
    from treemapper.ignore import _collect_parent_ignore_patterns

    parent = tmp_path / "parent"
    parent.mkdir()
    (parent / ".git").mkdir()
    child = parent / "packages" / "tools"
    child.mkdir(parents=True)

    (parent / ".treemapperignore").write_text("packages/tools/data/\npackages/tools/cache/\n*.log\n")
    (parent / ".gitignore").write_text("packages/tools/generated/\n**/__pycache__/\n")

    patterns = _collect_parent_ignore_patterns(child, [".gitignore", ".treemapperignore"])

    assert "/data/" in patterns
    assert "/cache/" in patterns
    assert "/generated/" in patterns
    assert "*.log" in patterns
    assert "**/__pycache__/" in patterns


def test_transform_parent_pattern_cases():
    from treemapper.ignore import _transform_parent_pattern

    assert _transform_parent_pattern("pkg/sub/data/", "pkg/sub") == "/data/"
    assert _transform_parent_pattern("/pkg/sub/data/", "pkg/sub") == "/data/"
    assert _transform_parent_pattern("other/dir/", "pkg/sub") is None
    assert _transform_parent_pattern("/other/dir/", "pkg/sub") is None
    assert _transform_parent_pattern("*.log", "pkg/sub") == "*.log"
    assert _transform_parent_pattern("**/*.pyc", "pkg/sub") == "**/*.pyc"
    assert _transform_parent_pattern("!pkg/sub/keep/", "pkg/sub") == "!/keep/"


def test_parent_ignore_integration(tmp_path, run_mapper):
    from .utils import find_node_by_path

    parent = tmp_path / "parent_project"
    parent.mkdir()
    (parent / ".git").mkdir()
    subdir = parent / "packages" / "app"
    subdir.mkdir(parents=True)

    (parent / ".treemapperignore").write_text("packages/app/ignored_dir/\npackages/app/data/\n")
    (subdir / "ignored_dir").mkdir()
    (subdir / "ignored_dir" / "secret.txt").write_text("secret")
    (subdir / "data").mkdir()
    (subdir / "data" / "large.json").write_text('{"big": "data"}')
    (subdir / "src").mkdir()
    (subdir / "src" / "main.py").write_text("print('hello')")
    (subdir / "README.md").write_text("# App")

    output_path = tmp_path / "output.yaml"
    assert run_mapper([str(subdir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert find_node_by_path(result, ["ignored_dir"]) is None
    assert find_node_by_path(result, ["data"]) is None
    assert find_node_by_path(result, ["src"]) is not None
    assert find_node_by_path(result, ["src", "main.py"]) is not None
    assert find_node_by_path(result, ["README.md"]) is not None


def test_parent_ignore_stops_at_git_root(tmp_path):
    from treemapper.ignore import _collect_parent_ignore_patterns

    grandparent = tmp_path / "grandparent"
    grandparent.mkdir()
    (grandparent / ".treemapperignore").write_text("should_not_appear/\n")

    parent = grandparent / "parent"
    parent.mkdir()
    (parent / ".git").mkdir()
    (parent / ".treemapperignore").write_text("packages/app/from_parent/\n")

    child = parent / "packages" / "app"
    child.mkdir(parents=True)

    patterns = _collect_parent_ignore_patterns(child, [".treemapperignore"])

    assert "/from_parent/" in patterns
    assert "should_not_appear/" not in patterns
