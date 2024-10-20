# tests/unit/test_core_functions.py
import pytest
from treemapper.treemapper import read_ignore_file, load_pathspec, should_ignore
from pathlib import Path

def test_read_ignore_file(tmp_path):
    ignore_file = tmp_path / ".treemapperignore"
    ignore_file.write_text("*.py\n__pycache__/\n")
    patterns = read_ignore_file(ignore_file)
    assert patterns == ["*.py", "__pycache__/"]

def test_load_pathspec():
    patterns = ["*.py", "__pycache__/"]
    spec = load_pathspec(patterns)
    assert spec.match_file("test.py")
    assert spec.match_file("__pycache__/")
    assert not spec.match_file("test.txt")

def test_should_ignore():
    patterns = ["*.py", "__pycache__/"]
    spec = load_pathspec(patterns)
    assert should_ignore("test.py", spec) == True
    assert should_ignore("__pycache__/", spec) == True
    assert should_ignore("test.txt", spec) == False
