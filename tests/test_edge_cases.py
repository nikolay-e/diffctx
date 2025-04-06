# tests/test_edge_cases.py


from .utils import get_all_files_in_tree, load_yaml


def test_empty_directory(temp_project, run_mapper):
    """Тест: пустая директория в качестве входной."""
    empty_dir = temp_project / "empty_test_dir"
    empty_dir.mkdir()

    output_path = temp_project / "empty_dir_output.yaml"
    assert run_mapper([str(empty_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == empty_dir.name
    assert result["type"] == "directory"
    assert "children" not in result or not result["children"]


def test_directory_with_only_ignored(temp_project, run_mapper):
    """Тест: директория содержит только игнорируемые файлы/папки."""
    ignored_dir = temp_project / "ignored_only_dir"
    ignored_dir.mkdir()
    (ignored_dir / ".DS_Store").touch()
    (ignored_dir / "temp").mkdir()
    (ignored_dir / "temp" / "file.tmp").touch()
    (ignored_dir / ".gitignore").write_text(".DS_Store\ntemp/\n")

    output_path = temp_project / "ignored_only_output.yaml"
    assert run_mapper([str(ignored_dir), "-o", str(output_path)])
    result = load_yaml(output_path)

    assert result["name"] == ignored_dir.name
    assert result["type"] == "directory"
    assert "children" in result and len(result["children"]) == 1
    assert result["children"][0]["name"] == ".gitignore"


def test_filenames_with_special_yaml_chars(temp_project, run_mapper):
    """Тест: имена файлов со спецсимволами YAML (проверка ручного writer'а)."""

    (temp_project / "-startswithdash.txt").touch()
    (temp_project / "quotes'single'.txt").touch()
    (temp_project / "bracket[].txt").touch()
    (temp_project / "curly{}.txt").touch()
    (temp_project / "percent%.txt").touch()
    (temp_project / "ampersand&.txt").touch()

    output_path = temp_project / "special_chars_output.yaml"
    assert run_mapper([".", "-o", str(output_path)])

    result = load_yaml(output_path)
    all_files = get_all_files_in_tree(result)

    if (temp_project / "-startswithdash.txt").exists():
        assert "-startswithdash.txt" in all_files
    if (temp_project / "quotes'single'.txt").exists():
        assert "quotes'single'.txt" in all_files
    if (temp_project / "bracket[].txt").exists():
        assert "bracket[].txt" in all_files
    if (temp_project / "curly{}.txt").exists():
        assert "curly{}.txt" in all_files
    if (temp_project / "percent%.txt").exists():
        assert "percent%.txt" in all_files
    if (temp_project / "ampersand&.txt").exists():
        assert "ampersand&.txt" in all_files
