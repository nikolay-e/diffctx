# tests/test_complete_coverage.py
import sys

import pytest

from treemapper import map_directory, to_json, to_text, to_yaml

from .conftest import run_treemapper_subprocess
from .utils import find_node_by_path, get_all_files_in_tree, load_yaml


class TestDefaultIgnorePatterns:

    def test_svn_directory_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        svn_dir = project / ".svn"
        svn_dir.mkdir()
        (svn_dir / "entries").write_text("svn entries", encoding="utf-8")
        (project / "file.txt").write_text("content", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".svn" not in names
        assert "entries" not in names
        assert "file.txt" in names

    def test_hg_directory_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        hg_dir = project / ".hg"
        hg_dir.mkdir()
        (hg_dir / "dirstate").write_text("hg dirstate", encoding="utf-8")
        (project / "file.txt").write_text("content", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".hg" not in names
        assert "dirstate" not in names
        assert "file.txt" in names

    def test_node_modules_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        node_modules = project / "node_modules"
        node_modules.mkdir()
        (node_modules / "lodash").mkdir()
        (node_modules / "lodash" / "index.js").write_text("module.exports = {}", encoding="utf-8")
        (project / "package.json").write_text("{}", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "node_modules" not in names
        assert "lodash" not in names
        assert "index.js" not in names
        assert "package.json" in names

    def test_npm_cache_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        npm_dir = project / ".npm"
        npm_dir.mkdir()
        (npm_dir / "_cacache").mkdir()
        (project / "file.txt").write_text("content", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".npm" not in names
        assert "_cacache" not in names
        assert "file.txt" in names

    def test_java_target_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        target_dir = project / "target"
        target_dir.mkdir()
        (target_dir / "classes").mkdir()
        (target_dir / "classes" / "App.class").write_text("bytecode", encoding="utf-8")
        (project / "pom.xml").write_text("<project/>", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "target" not in names
        assert "classes" not in names
        assert "App.class" not in names
        assert "pom.xml" in names

    def test_gradle_directory_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        gradle_dir = project / ".gradle"
        gradle_dir.mkdir()
        (gradle_dir / "caches").mkdir()
        (project / "build.gradle").write_text("apply plugin: 'java'", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".gradle" not in names
        assert "caches" not in names
        assert "build.gradle" in names

    def test_dotnet_bin_obj_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        bin_dir = project / "bin"
        bin_dir.mkdir()
        (bin_dir / "Debug").mkdir()
        (bin_dir / "Debug" / "app.dll").write_text("binary", encoding="utf-8")
        obj_dir = project / "obj"
        obj_dir.mkdir()
        (obj_dir / "project.assets.json").write_text("{}", encoding="utf-8")
        (project / "app.csproj").write_text("<Project/>", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "bin" not in names
        assert "obj" not in names
        assert "Debug" not in names
        assert "app.dll" not in names
        assert "app.csproj" in names

    def test_go_vendor_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        vendor_dir = project / "vendor"
        vendor_dir.mkdir()
        (vendor_dir / "github.com").mkdir()
        (project / "go.mod").write_text("module example.com/app", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "vendor" not in names
        assert "github.com" not in names
        assert "go.mod" in names

    def test_dist_build_out_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        for dirname in ["dist", "build", "out"]:
            d = project / dirname
            d.mkdir()
            (d / "output.js").write_text("compiled", encoding="utf-8")

        (project / "src").mkdir()
        (project / "src" / "index.ts").write_text("export {}", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "dist" not in names
        assert "build" not in names
        assert "out" not in names
        assert "output.js" not in names
        assert "src" in names
        assert "index.ts" in names

    def test_ide_directories_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        idea_dir = project / ".idea"
        idea_dir.mkdir()
        (idea_dir / "workspace.xml").write_text("<workspace/>", encoding="utf-8")

        vscode_dir = project / ".vscode"
        vscode_dir.mkdir()
        (vscode_dir / "settings.json").write_text("{}", encoding="utf-8")

        (project / "main.py").write_text("print('hello')", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".idea" not in names
        assert "workspace.xml" not in names
        assert ".vscode" not in names
        assert "settings.json" not in names
        assert "main.py" in names

    def test_os_files_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / ".DS_Store").write_text("macos metadata", encoding="utf-8")
        (project / "Thumbs.db").write_text("windows thumbnails", encoding="utf-8")
        (project / "file.txt").write_text("content", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".DS_Store" not in names
        assert "Thumbs.db" not in names
        assert "file.txt" in names

    def test_python_venv_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        for venv_name in ["venv", ".venv"]:
            venv_dir = project / venv_name
            venv_dir.mkdir()
            (venv_dir / "bin").mkdir()
            (venv_dir / "bin" / "python").write_text("#!/bin/bash", encoding="utf-8")

        (project / "main.py").write_text("print('hello')", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "venv" not in names
        assert ".venv" not in names
        assert "main.py" in names

    def test_python_tox_nox_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        tox_dir = project / ".tox"
        tox_dir.mkdir()
        (tox_dir / "py39").mkdir()

        nox_dir = project / ".nox"
        nox_dir.mkdir()
        (nox_dir / "tests").mkdir()

        (project / "tox.ini").write_text("[tox]", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".tox" not in names
        assert ".nox" not in names
        assert "tox.ini" in names

    def test_python_so_files_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "module.so").write_text("shared object", encoding="utf-8")
        (project / "module.py").write_text("def func(): pass", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "module.so" not in names
        assert "module.py" in names

    def test_python_coverage_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / ".coverage").write_text("coverage data", encoding="utf-8")
        (project / "test.py").write_text("def test(): pass", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".coverage" not in names
        assert "test.py" in names

    def test_eggs_directory_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        eggs_dir = project / ".eggs"
        eggs_dir.mkdir()
        (eggs_dir / "package.egg").write_text("egg", encoding="utf-8")
        (project / "setup.py").write_text("from setuptools import setup", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".eggs" not in names
        assert "setup.py" in names

    def test_cache_directories_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        for cache_name in [".pytest_cache", ".mypy_cache", ".ruff_cache"]:
            cache_dir = project / cache_name
            cache_dir.mkdir()
            (cache_dir / "data").write_text("cached", encoding="utf-8")

        (project / "main.py").write_text("print('hello')", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert ".pytest_cache" not in names
        assert ".mypy_cache" not in names
        assert ".ruff_cache" not in names
        assert "main.py" in names


class TestCLIFeatures:

    def test_version_flag(self, temp_project):
        result = run_treemapper_subprocess(["--version"], cwd=temp_project)
        assert result.returncode == 0
        assert "treemapper" in result.stdout.lower() or "." in result.stdout

    def test_help_flag_short(self, temp_project):
        result = run_treemapper_subprocess(["-h"], cwd=temp_project)
        assert result.returncode == 0
        assert "usage:" in result.stdout.lower()
        assert "--format" in result.stdout
        assert "--max-depth" in result.stdout
        assert "--no-content" in result.stdout
        assert "--max-file-bytes" in result.stdout
        assert "-i" in result.stdout or "--ignore-file" in result.stdout
        assert "--no-default-ignores" in result.stdout
        assert "--log-level" in result.stdout

    def test_help_flag_long(self, temp_project):
        result = run_treemapper_subprocess(["--help"], cwd=temp_project)
        assert result.returncode == 0
        assert "usage:" in result.stdout.lower()

    def test_explicit_stdout_output_dash(self, temp_project):
        result = run_treemapper_subprocess([".", "-o", "-"], cwd=temp_project)
        assert result.returncode == 0
        assert "name:" in result.stdout
        assert "type: directory" in result.stdout

    def test_explicit_stdout_output_long_flag(self, temp_project):
        result = run_treemapper_subprocess([".", "--output-file", "-"], cwd=temp_project)
        assert result.returncode == 0
        assert "name:" in result.stdout

    def test_yaml_to_stdout_default(self, temp_project):
        result = run_treemapper_subprocess(["."], cwd=temp_project)
        assert result.returncode == 0
        assert "name:" in result.stdout
        assert "children:" in result.stdout

    def test_json_format_stdout(self, temp_project):
        import json

        result = run_treemapper_subprocess([".", "--format", "json"], cwd=temp_project)
        assert result.returncode == 0
        parsed = json.loads(result.stdout)
        assert "name" in parsed
        assert "type" in parsed

    def test_txt_format_stdout(self, temp_project):
        result = run_treemapper_subprocess([".", "--format", "txt"], cwd=temp_project)
        assert result.returncode == 0
        assert temp_project.name in result.stdout
        assert "  " in result.stdout

    def test_all_log_levels_cli(self, temp_project):
        for level in ["error", "warning", "info", "debug"]:
            result = run_treemapper_subprocess([".", "--log-level", level], cwd=temp_project)
            assert result.returncode == 0

    def test_invalid_log_level_cli(self, temp_project):
        result = run_treemapper_subprocess([".", "--log-level", "verbose"], cwd=temp_project)
        assert result.returncode != 0

        result = run_treemapper_subprocess([".", "--log-level", "quiet"], cwd=temp_project)
        assert result.returncode != 0


class TestPythonAPIEdgeCases:

    def test_nonexistent_directory(self, tmp_path):
        nonexistent = tmp_path / "does_not_exist"
        with pytest.raises((FileNotFoundError, ValueError, SystemExit)):
            map_directory(nonexistent)

    def test_all_params_combined(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "level1").mkdir()
        (project / "level1" / "level2").mkdir()
        (project / "level1" / "level2" / "deep.txt").write_text("deep content", encoding="utf-8")
        (project / "large.txt").write_text("x" * 1000, encoding="utf-8")
        (project / "small.txt").write_text("small", encoding="utf-8")
        (project / "ignore.log").write_text("log", encoding="utf-8")

        ignore_file = tmp_path / "custom.ignore"
        ignore_file.write_text("*.log\n", encoding="utf-8")

        tree = map_directory(
            project,
            max_depth=2,
            no_content=False,
            max_file_bytes=100,
            ignore_file=ignore_file,
            no_default_ignores=False,
        )

        names = get_all_files_in_tree(tree)

        assert "level1" in names
        assert "level2" in names
        assert "deep.txt" not in names
        assert "ignore.log" not in names
        assert "small.txt" in names

        large_node = find_node_by_path(tree, ["large.txt"])
        assert large_node is not None
        assert "<file too large:" in large_node.get("content", "")

    def test_no_content_true(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        (project / "file.txt").write_text("some content here", encoding="utf-8")

        tree = map_directory(project, no_content=True)

        file_node = find_node_by_path(tree, ["file.txt"])
        assert file_node is not None
        assert "content" not in file_node

    def test_max_depth_zero(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        (project / "subdir").mkdir()
        (project / "subdir" / "file.txt").write_text("content", encoding="utf-8")
        (project / "root.txt").write_text("root content", encoding="utf-8")

        tree = map_directory(project, max_depth=0)

        assert tree["name"] == "project"
        assert tree.get("children") is None or len(tree.get("children", [])) == 0

    def test_max_depth_one(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        (project / "subdir").mkdir()
        (project / "subdir" / "nested").mkdir()
        (project / "subdir" / "file.txt").write_text("content", encoding="utf-8")

        tree = map_directory(project, max_depth=1)
        names = get_all_files_in_tree(tree)

        assert "subdir" in names
        assert "file.txt" not in names
        assert "nested" not in names

    def test_serializers_roundtrip(self, tmp_path):
        import json

        import yaml

        project = tmp_path / "project"
        project.mkdir()
        (project / "file.txt").write_text("content", encoding="utf-8")

        tree = map_directory(project)

        yaml_output = to_yaml(tree)
        json_output = to_json(tree)
        text_output = to_text(tree)

        assert "file.txt" in yaml_output
        assert "file.txt" in json_output
        assert "file.txt" in text_output

        parsed_yaml = yaml.safe_load(yaml_output)
        parsed_json = json.loads(json_output)

        assert parsed_yaml["name"] == "project"
        assert parsed_json["name"] == "project"


class TestIgnorePatternEdgeCases:

    def test_negation_with_no_default_ignores(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "__pycache__").mkdir()
        (project / "__pycache__" / "module.pyc").write_text("bytecode", encoding="utf-8")
        (project / "important.pyc").write_text("important", encoding="utf-8")
        (project / ".gitignore").write_text("*.pyc\n!important.pyc\n", encoding="utf-8")
        (project / "main.py").write_text("print('hello')", encoding="utf-8")

        tree = map_directory(project, no_default_ignores=True)
        names = get_all_files_in_tree(tree)

        assert "__pycache__" in names
        assert "important.pyc" in names
        assert "main.py" in names

    def test_trailing_slash_directory_pattern(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "logs").mkdir()
        (project / "logs" / "app.log").write_text("log entry", encoding="utf-8")
        logs_file = project / "logs.txt"
        logs_file.write_text("not a directory", encoding="utf-8")

        (project / ".gitignore").write_text("logs/\n", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "logs" not in names
        assert "logs.txt" in names

    def test_pattern_matches_exact_name(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "temp").mkdir()
        (project / "temp" / "data.txt").write_text("data", encoding="utf-8")
        (project / "temp.txt").write_text("temp file", encoding="utf-8")

        (project / ".gitignore").write_text("temp\n", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "temp" not in names
        assert "data.txt" not in names
        assert "temp.txt" in names

    def test_pattern_with_spaces(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        spaced_file = project / "my file.txt"
        spaced_file.write_text("content with spaces", encoding="utf-8")
        (project / "myfile.txt").write_text("no spaces", encoding="utf-8")

        (project / ".gitignore").write_text("my file.txt\n", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "my file.txt" not in names
        assert "myfile.txt" in names

    def test_double_star_in_middle(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "src").mkdir()
        (project / "src" / "tests").mkdir()
        (project / "src" / "tests" / "test_main.py").write_text("test", encoding="utf-8")
        (project / "tests").mkdir()
        (project / "tests" / "test_other.py").write_text("test", encoding="utf-8")

        (project / ".gitignore").write_text("**/tests/\n", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "tests" not in names
        assert "test_main.py" not in names
        assert "test_other.py" not in names
        assert "src" in names

    def test_root_anchored_pattern(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "config.json").write_text("{}", encoding="utf-8")
        (project / "src").mkdir()
        (project / "src" / "config.json").write_text("{}", encoding="utf-8")

        (project / ".gitignore").write_text("/config.json\n", encoding="utf-8")

        tree = map_directory(project)

        root_node = find_node_by_path(tree, ["config.json"])
        nested_node = find_node_by_path(tree, ["src", "config.json"])

        assert root_node is None
        assert nested_node is not None

    def test_comment_lines_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "file.txt").write_text("content", encoding="utf-8")
        (project / "ignored.log").write_text("log", encoding="utf-8")

        (project / ".gitignore").write_text("# This is a comment\n*.log\n# Another comment\n", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "file.txt" in names
        assert "ignored.log" not in names

    def test_empty_lines_ignored(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / "file.txt").write_text("content", encoding="utf-8")
        (project / "ignored.log").write_text("log", encoding="utf-8")

        (project / ".gitignore").write_text("\n\n*.log\n\n", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "file.txt" in names
        assert "ignored.log" not in names


class TestContentPlaceholders:

    def test_large_file_placeholder(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        large_file = project / "large.txt"
        large_file.write_text("x" * 10000, encoding="utf-8")

        tree = map_directory(project, max_file_bytes=1000)

        node = find_node_by_path(tree, ["large.txt"])
        assert node is not None
        content = node.get("content", "")
        assert "<file too large:" in content
        assert "10000 bytes" in content

    def test_binary_file_placeholder(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        binary_file = project / "binary.bin"
        binary_file.write_bytes(b"text\x00binary\x00data")

        tree = map_directory(project)

        node = find_node_by_path(tree, ["binary.bin"])
        assert node is not None
        content = node.get("content", "")
        assert "<binary file:" in content

    def test_non_utf8_placeholder(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        non_utf8 = project / "non_utf8.txt"
        non_utf8.write_bytes(b"\x80\x81\x82\x83")

        tree = map_directory(project)

        node = find_node_by_path(tree, ["non_utf8.txt"])
        assert node is not None
        content = node.get("content", "")
        assert "<unreadable content" in content

    @pytest.mark.skipif(
        sys.platform == "win32",
        reason="Permission tests skipped on Windows",
    )
    def test_unreadable_file_placeholder(self, tmp_path, set_perms):
        project = tmp_path / "project"
        project.mkdir()

        unreadable = project / "unreadable.txt"
        unreadable.write_text("secret content", encoding="utf-8")
        set_perms(unreadable, 0o000)

        tree = map_directory(project)

        node = find_node_by_path(tree, ["unreadable.txt"])
        assert node is not None
        content = node.get("content", "")
        assert "<unreadable content>" in content


class TestOutputFileAutoIgnore:

    def test_output_file_excluded_from_tree(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        (project / "file.txt").write_text("content", encoding="utf-8")
        output_file = project / "output.yaml"

        result = run_treemapper_subprocess([".", "-o", str(output_file)], cwd=project)
        assert result.returncode == 0

        tree = load_yaml(output_file)
        names = get_all_files_in_tree(tree)

        assert "output.yaml" not in names
        assert "file.txt" in names

    def test_output_file_in_subdir_excluded(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()
        (project / "src").mkdir()
        (project / "src" / "main.py").write_text("print('hello')", encoding="utf-8")
        output_file = project / "src" / "tree.yaml"

        result = run_treemapper_subprocess([".", "-o", str(output_file)], cwd=project)
        assert result.returncode == 0

        tree = load_yaml(output_file)
        names = get_all_files_in_tree(tree)

        assert "tree.yaml" not in names
        assert "main.py" in names


class TestHierarchicalIgnoreFiles:

    def test_nested_gitignore_files(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / ".gitignore").write_text("*.log\n", encoding="utf-8")
        (project / "root.log").write_text("root log", encoding="utf-8")
        (project / "root.txt").write_text("root txt", encoding="utf-8")

        (project / "subdir").mkdir()
        (project / "subdir" / ".gitignore").write_text("*.tmp\n", encoding="utf-8")
        (project / "subdir" / "file.tmp").write_text("temp", encoding="utf-8")
        (project / "subdir" / "file.txt").write_text("text", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "root.log" not in names
        assert "root.txt" in names
        assert "file.tmp" not in names
        assert "file.txt" in names

    def test_nested_treemapperignore_files(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / ".treemapperignore").write_text("*.bak\n", encoding="utf-8")
        (project / "root.bak").write_text("backup", encoding="utf-8")

        (project / "subdir").mkdir()
        (project / "subdir" / ".treemapperignore").write_text("*.cache\n", encoding="utf-8")
        (project / "subdir" / "data.cache").write_text("cached", encoding="utf-8")
        (project / "subdir" / "data.txt").write_text("data", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "root.bak" not in names
        assert "data.cache" not in names
        assert "data.txt" in names

    def test_treemapperignore_precedence_over_gitignore(self, tmp_path):
        project = tmp_path / "project"
        project.mkdir()

        (project / ".gitignore").write_text("important.txt\n", encoding="utf-8")
        (project / ".treemapperignore").write_text("!important.txt\n", encoding="utf-8")
        (project / "important.txt").write_text("important content", encoding="utf-8")

        tree = map_directory(project)
        names = get_all_files_in_tree(tree)

        assert "important.txt" in names
