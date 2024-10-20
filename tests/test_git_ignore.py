from treemapper.treemapper import main
import os
import sys
import yaml

import pytest

# Add the src directory to the Python path
sys.path.insert(
    0,
    os.path.abspath(
        os.path.join(
            os.path.dirname(__file__),
            '..',
            'src')))


@pytest.fixture
def git_directory(tmp_path):
    (tmp_path / "dir1").mkdir()
    (tmp_path / "dir1" / "file1.txt").write_text("Content of file1")
    (tmp_path / "dir2").mkdir()
    (tmp_path / "dir2" / "file2.py").write_text("print('Hello, World!')")
    (tmp_path / ".gitignore").write_text("*.log\n")
    (tmp_path / ".git").mkdir()
    return tmp_path


def test_git_ignore_default(git_directory, tmp_path, monkeypatch, capsys):
    output_file = tmp_path / "output.yaml"
    monkeypatch.setattr(
        'sys.argv', [
            'treemapper', str(git_directory), '-o', str(output_file)])
    main()

    with open(output_file, 'r') as f:
        content = yaml.safe_load(f)

    def check_git_not_in_tree(node):
        assert node['name'] != '.git'
        if 'children' in node:
            for child in node['children']:
                check_git_not_in_tree(child)

    check_git_not_in_tree(content)
    assert any(child['name'] == '.gitignore' for child in content['children'])
    assert any(child['name'] == 'dir1' for child in content['children'])
    assert any(child['name'] == 'dir2' for child in content['children'])


def test_no_git_ignore_option(git_directory, tmp_path, monkeypatch, capsys):
    output_file = tmp_path / "output.yaml"
    monkeypatch.setattr('sys.argv', ['treemapper', str(
        git_directory), '--no-git-ignore', '-o', str(output_file)])
    main()

    content = output_file.read_text()
    assert ".git" in content  # .git should be included when using --no-git-ignore
    assert ".gitignore" in content  # .gitignore should NOT be ignored
    assert "dir1" in content  # dir1 should not be ignored
    assert "file1.txt" in content  # file1.txt should not be ignored
    assert "file2.py" in content  # file2.py should not be ignored


def test_gitignore_respected(git_directory, tmp_path, monkeypatch, capsys):
    (git_directory / "test.log").write_text("This is a log file")
    output_file = tmp_path / "output.yaml"
    monkeypatch.setattr(
        'sys.argv', [
            'treemapper', str(git_directory), '-o', str(output_file)])
    main()

    content = output_file.read_text()
    assert "test.log" not in content  # .log files should be ignored due to .gitignore
