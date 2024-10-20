from treemapper.treemapper import write_yaml_node
import os
import sys

# Add the src directory to the Python path
sys.path.insert(
    0,
    os.path.abspath(
        os.path.join(
            os.path.dirname(__file__),
            '..',
            'src')))


def test_write_yaml_node(tmp_path):
    node = {
        "name": "test_dir",
        "type": "directory",
        "children": [
            {
                "name": "test_file.txt",
                "type": "file",
                "content": "Test content"
            }
        ]
    }
    output_file = tmp_path / "test_output.yaml"
    with output_file.open('w') as f:
        write_yaml_node(f, node)

    content = output_file.read_text()
    assert "- name: test_dir" in content
    assert "type: directory" in content
    assert "- name: test_file.txt" in content
    assert "type: file" in content
    assert "content: |" in content
    assert "Test content" in content


def test_write_yaml_node_with_unicode(tmp_path):
    node = {
        "name": "test_dir",
        "type": "directory",
        "children": [
            {
                "name": "unicode_file.txt",
                "type": "file",
                "content": "Content with unicode: ÜñíçÕdê"
            }
        ]
    }
    output_file = tmp_path / "test_output.yaml"
    with output_file.open('w', encoding='utf-8') as f:
        write_yaml_node(f, node)

    with output_file.open('r', encoding='utf-8') as f:
        content = f.read()
    assert "Content with unicode: ÜñíçÕdê" in content


def test_yaml_output_format(tmp_path):
    node = {"name": "test", "type": "directory", "children": [
        {"name": "file.txt", "type": "file", "content": "content"}]}
    output_file = tmp_path / "output.yaml"
    with output_file.open('w') as f:
        write_yaml_node(f, node)
    content = output_file.read_text()
    assert "name: test" in content
    assert "name: file.txt" in content
