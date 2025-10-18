# tests/test_ignore_advanced.py
import logging

from .utils import find_node_by_path, get_all_files_in_tree, load_yaml


def test_ignore_precedence_subdir_over_root(temp_project, run_mapper):
    """Test: negation pattern in subdirectory .gitignore correctly un-ignores files (Git-compliant behavior)"""
    (temp_project / ".gitignore").write_text("*.txt\n")
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / ".gitignore").write_text("!allow.txt\n")
    (temp_project / "root.txt").touch()
    (temp_project / "subdir" / "ignore.txt").touch()
    (temp_project / "subdir" / "allow.txt").touch()

    assert run_mapper([".", "-o", "directory_tree.yaml"])
    result = load_yaml(temp_project / "directory_tree.yaml")
    all_files = get_all_files_in_tree(result)

    # Root level *.txt pattern should ignore root.txt
    assert "root.txt" not in all_files
    # Subdir *.txt without negation should ignore ignore.txt
    assert "ignore.txt" not in all_files
    # Negation pattern !allow.txt in subdir should un-ignore allow.txt (correct Git behavior)
    assert "allow.txt" in all_files, "'allow.txt' should be un-ignored by negation pattern in subdir/.gitignore"


def test_ignore_precedence_treemapper_over_git(temp_project, run_mapper):
    """Test: .treemapperignore and .gitignore rules are combined"""
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
    """Test: custom file (-i) SUPPLEMENTS default ignores"""
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
    """Test: File ignored by combined_spec, but un-ignored in .gitignore"""
    output_path = temp_project / "output.yaml"
    (temp_project / ".gitignore").write_text("!output.yaml\n")

    assert run_mapper([".", "-o", str(output_path)])
    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)

    assert output_path.name not in all_files, "Output file should be ignored even if negated in .gitignore"


def test_ignore_patterns_anchored(temp_project, run_mapper, caplog):
    """Test: anchored patterns (starting with '/') and non-anchored patterns work correctly"""
    caplog.set_level(logging.DEBUG)
    (temp_project / ".gitignore").write_text("/root_ignore.txt\nsubdir_ignore.txt")
    (temp_project / "root_ignore.txt").touch()
    (temp_project / "subdir").mkdir()
    (temp_project / "subdir" / "root_ignore.txt").touch()
    (temp_project / "subdir" / "subdir_ignore.txt").touch()
    (temp_project / "subdir_ignore.txt").touch()

    assert run_mapper([".", "-o", "anchored_output.yaml"])
    result = load_yaml(temp_project / "anchored_output.yaml")

    # Anchored pattern /root_ignore.txt should ignore file ONLY in root (use find_node_by_path to check specific location)
    assert find_node_by_path(result, ["root_ignore.txt"]) is None, "Anchored pattern '/root_ignore.txt' should ignore root file"

    # Anchored pattern should NOT affect subdirectory files with same name
    assert find_node_by_path(result, ["subdir", "root_ignore.txt"]) is not None, "/root_ignore.txt should NOT ignore subdir file"
    # Non-anchored pattern should ignore files everywhere
    assert find_node_by_path(result, ["subdir", "subdir_ignore.txt"]) is None, "subdir_ignore.txt should ignore file in subdir"
    assert find_node_by_path(result, ["subdir_ignore.txt"]) is None, "subdir_ignore.txt should ignore file in root"
