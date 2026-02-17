import tempfile
from pathlib import Path

import pytest
import yaml

pytest.importorskip("hypothesis")

from hypothesis import given, settings
from hypothesis import strategies as st

from treemapper import to_yaml
from treemapper.ignore import read_ignore_file
from treemapper.tree import _read_file_content

# Include problematic NEL character (U+0085) that was causing YAML roundtrip failures
filename_chars_with_nel = st.text(
    alphabet=st.sampled_from("abcdefghijklmnopqrstuvwxyz0123456789_-\x85"),
    min_size=1,
    max_size=30,
)

pattern_text = st.text(min_size=0, max_size=50).filter(lambda x: "\n" not in x and "\r" not in x)


@given(st.text(max_size=5000))
@settings(max_examples=100)
def test_file_content_no_null_bytes(content):
    with tempfile.TemporaryDirectory() as tmp_dir:
        f = Path(tmp_dir) / "test.txt"
        f.write_text(content, encoding="utf-8")
        result = _read_file_content(f, max_file_bytes=None)
        assert "\x00" not in result


@given(
    st.binary(min_size=10, max_size=1000),
    st.integers(min_value=0, max_value=999),
)
@settings(max_examples=50)
def test_binary_detection_with_null_byte(data, null_offset):
    from treemapper.tree import BINARY_DETECTION_SAMPLE_SIZE

    max_insert = min(len(data), BINARY_DETECTION_SAMPLE_SIZE - 1)
    insert_pos = null_offset % (max_insert + 1)
    content = data[:insert_pos] + b"\x00" + data[insert_pos:]
    with tempfile.TemporaryDirectory() as tmp_dir:
        f = Path(tmp_dir) / "test.bin"
        f.write_bytes(content)
        result = _read_file_content(f, max_file_bytes=None)
        assert "<binary file:" in result


@given(st.binary(min_size=10, max_size=1000).filter(lambda x: b"\x00" not in x))
@settings(max_examples=50)
def test_text_file_not_detected_as_binary(data):
    with tempfile.TemporaryDirectory() as tmp_dir:
        f = Path(tmp_dir) / "test.txt"
        f.write_bytes(data)
        result = _read_file_content(f, max_file_bytes=None)
        assert "<binary file:" not in result or "<unreadable content" in result


@given(st.lists(pattern_text, min_size=0, max_size=20))
@settings(max_examples=100)
def test_ignore_patterns_roundtrip(patterns):
    with tempfile.TemporaryDirectory() as tmp_dir:
        valid = [p.rstrip() for p in patterns if p.strip() and not p.startswith("#")]
        f = Path(tmp_dir) / ".gitignore"
        f.write_text("\n".join(patterns), encoding="utf-8")
        result = read_ignore_file(f)
        assert result == valid


@given(
    st.fixed_dictionaries(
        {
            "name": filename_chars_with_nel,
            "type": st.sampled_from(["file", "directory"]),
        }
    )
)
@settings(max_examples=100)
def test_yaml_roundtrip_preserves_structure(node):
    yaml_str = to_yaml(node)
    parsed = yaml.safe_load(yaml_str)
    assert parsed["name"] == node["name"]
    assert parsed["type"] == node["type"]


filename_safe = st.text(
    alphabet=st.sampled_from("abcdefghijklmnopqrstuvwxyz0123456789_-."),
    min_size=1,
    max_size=20,
)

yaml_safe_content = st.text(
    alphabet=st.characters(
        blacklist_categories=("Cs", "Cc"),
        whitelist_characters="\t\n",
    ),
    min_size=1,
    max_size=200,
).map(lambda s: s if s.endswith("\n") else s + "\n")


@given(
    st.fixed_dictionaries(
        {
            "name": filename_chars_with_nel,
            "type": st.just("directory"),
            "children": st.lists(
                st.fixed_dictionaries(
                    {
                        "name": filename_safe,
                        "type": st.just("file"),
                        "content": yaml_safe_content,
                    }
                ),
                min_size=0,
                max_size=5,
            ),
        }
    )
)
@settings(max_examples=100)
def test_yaml_roundtrip_nested_tree(tree):
    yaml_str = to_yaml(tree)
    parsed = yaml.safe_load(yaml_str)
    assert parsed["name"] == tree["name"]
    assert parsed["type"] == tree["type"]
    assert len(parsed.get("children", [])) == len(tree["children"])
    for original, roundtripped in zip(tree["children"], parsed.get("children", [])):
        assert roundtripped["name"] == original["name"]
        assert roundtripped["content"] == original["content"]


@given(
    st.fixed_dictionaries(
        {
            "name": filename_chars_with_nel,
            "type": st.sampled_from(["file", "directory"]),
        }
    )
)
@settings(max_examples=100)
def test_json_roundtrip_preserves_structure(node):
    import json

    from treemapper import to_json

    json_str = to_json(node)
    parsed = json.loads(json_str)
    assert parsed["name"] == node["name"]
    assert parsed["type"] == node["type"]


@given(st.integers(min_value=1, max_value=10000))
@settings(max_examples=50)
def test_max_file_bytes_respected(max_bytes):
    with tempfile.TemporaryDirectory() as tmp_dir:
        f = Path(tmp_dir) / "large.txt"
        content = "x" * (max_bytes + 100)
        f.write_text(content, encoding="utf-8")
        result = _read_file_content(f, max_file_bytes=max_bytes)
        assert "<file too large:" in result or len(result) <= max_bytes + 1


def test_yaml_roundtrip_multiline_with_nel():
    file_node = {"name": "test.txt", "type": "file", "content": "line1\x85line2\nline3"}
    tree = {"name": "root", "type": "directory", "children": [file_node]}
    yaml_str = to_yaml(tree)
    parsed = yaml.safe_load(yaml_str)
    assert parsed["children"][0]["content"] == file_node["content"]
