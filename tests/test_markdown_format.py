# tests/test_markdown_format.py
import io

import pytest

from treemapper import map_directory
from treemapper.writer import _get_fence_length, _infer_language, _is_placeholder, tree_to_string, write_tree_markdown

from .conftest import run_treemapper_subprocess


class TestMarkdownOutput:
    def test_basic_structure(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "file.txt", "type": "file", "content": "hello\n"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert result.startswith("# project/\n")
        assert "## file.txt" in result
        assert "hello" in result

    def test_directory_with_trailing_slash(self):
        tree = {"name": "mydir", "type": "directory", "children": [{"name": "subdir", "type": "directory", "children": []}]}
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "# mydir/" in result
        assert "## subdir/" in result

    def test_nested_structure_headings(self):
        tree = {
            "name": "root",
            "type": "directory",
            "children": [
                {
                    "name": "level1",
                    "type": "directory",
                    "children": [
                        {
                            "name": "level2",
                            "type": "directory",
                            "children": [{"name": "file.txt", "type": "file", "content": "content"}],
                        }
                    ],
                }
            ],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "# root/" in result
        assert "## level1/" in result
        assert "### level2/" in result
        assert "#### file.txt" in result

    def test_deep_nesting_uses_bullets(self):
        def create_deep_tree(depth):
            if depth == 0:
                return {"name": "file.txt", "type": "file", "content": "deep"}
            return {"name": f"level{depth}", "type": "directory", "children": [create_deep_tree(depth - 1)]}

        tree = {"name": "root", "type": "directory", "children": [create_deep_tree(7)]}
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "###### " in result
        assert "- **" in result

    def test_code_fence_with_language(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "main.py", "type": "file", "content": "print('hello')\n"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "```python" in result
        assert "print('hello')" in result
        assert "```\n" in result

    def test_code_fence_javascript(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "app.js", "type": "file", "content": "console.log('hi');\n"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "```javascript" in result

    def test_code_fence_no_language(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "unknown.xyz", "type": "file", "content": "some content\n"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        lines = result.split("\n")
        has_plain_fence = any(line.strip() == "```" for line in lines)
        has_fence_start = "```\n" in result or "```\r\n" in result
        assert has_plain_fence or has_fence_start

    def test_placeholder_content_italic(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "binary.bin", "type": "file", "content": "<binary file: 1024 bytes>"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "_<binary file: 1024 bytes>_" in result

    def test_unreadable_placeholder(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "bad.txt", "type": "file", "content": "<unreadable content>"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "_<unreadable content>_" in result

    def test_file_too_large_placeholder(self):
        tree = {
            "name": "project",
            "type": "directory",
            "children": [{"name": "huge.log", "type": "file", "content": "<file too large: 10485760 bytes>"}],
        }
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "_<file too large: 10485760 bytes>_" in result

    def test_empty_content(self):
        tree = {"name": "project", "type": "directory", "children": [{"name": "empty.txt", "type": "file", "content": ""}]}
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "## empty.txt" in result
        assert "```" not in result.split("## empty.txt")[1].split("\n\n")[0]

    def test_no_content_key(self):
        tree = {"name": "project", "type": "directory", "children": [{"name": "nokey.txt", "type": "file"}]}
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "## nokey.txt" in result

    def test_empty_directory(self):
        tree = {"name": "empty_project", "type": "directory", "children": []}
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "# empty_project/" in result
        assert result.strip() == "# empty_project/"


class TestCodeFenceLength:
    def test_normal_content_triple_backticks(self):
        assert _get_fence_length("normal content") == 3

    def test_content_with_triple_backticks(self):
        assert _get_fence_length("some ```code``` here") == 4

    def test_content_with_quadruple_backticks(self):
        assert _get_fence_length("````") == 5

    def test_content_with_many_backticks(self):
        assert _get_fence_length("`````````") == 10

    def test_mixed_backticks(self):
        assert _get_fence_length("`` ``` ````") == 5


class TestLanguageInference:
    @pytest.mark.parametrize(
        "filename,expected",
        [
            ("main.py", "python"),
            ("app.js", "javascript"),
            ("index.ts", "typescript"),
            ("style.css", "css"),
            ("page.html", "html"),
            ("config.yaml", "yaml"),
            ("data.json", "json"),
            ("script.sh", "bash"),
            ("main.go", "go"),
            ("lib.rs", "rust"),
            ("Main.java", "java"),
            ("Program.cs", "csharp"),
            ("file.cpp", "cpp"),
            ("query.sql", "sql"),
            ("schema.graphql", "graphql"),
            ("README.md", "markdown"),
            ("Dockerfile", "dockerfile"),
            ("Makefile", "makefile"),
            ("unknown.xyz", ""),
        ],
    )
    def test_extension_inference(self, filename, expected):
        assert _infer_language(filename) == expected

    def test_case_insensitive_extension(self):
        assert _infer_language("FILE.PY") == "python"
        assert _infer_language("App.JS") == "javascript"

    def test_special_filenames(self):
        assert _infer_language("Dockerfile") == "dockerfile"
        assert _infer_language("Makefile") == "makefile"
        assert _infer_language(".bashrc") == "bash"
        assert _infer_language(".gitconfig") == "gitconfig"


class TestPlaceholderDetection:
    def test_binary_placeholder(self):
        assert _is_placeholder("<binary file: 100 bytes>")
        assert _is_placeholder("  <binary file: 1024 bytes>  ")

    def test_file_too_large_placeholder(self):
        assert _is_placeholder("<file too large: 1000000 bytes>")
        assert _is_placeholder("  <file too large: 5000 bytes>  ")

    def test_unreadable_content_placeholder(self):
        assert _is_placeholder("<unreadable content>")
        assert _is_placeholder("<unreadable content: not utf-8>")

    def test_normal_content_not_placeholder(self):
        assert not _is_placeholder("normal content")
        assert not _is_placeholder("print('hello')")
        assert not _is_placeholder("<not a placeholder>")
        assert not _is_placeholder("<html>")


class TestTreeToStringMarkdown:
    def test_tree_to_string_md_format(self):
        tree = {"name": "test", "type": "directory", "children": [{"name": "file.py", "type": "file", "content": "pass\n"}]}
        result = tree_to_string(tree, "md")

        assert "# test/" in result
        assert "```python" in result

    def test_tree_to_string_md_vs_yaml(self):
        tree = {"name": "test", "type": "directory", "children": []}
        md = tree_to_string(tree, "md")
        yaml_out = tree_to_string(tree, "yaml")

        assert md.startswith("# test/")
        assert yaml_out.startswith('name: "test"')


class TestMarkdownCLI:
    def test_md_format_cli(self, temp_project):
        result = run_treemapper_subprocess([str(temp_project), "--format", "md"])
        assert result.returncode == 0
        assert f"# {temp_project.name}/" in result.stdout

    def test_md_format_to_file(self, temp_project):
        output_file = temp_project / "output.md"
        result = run_treemapper_subprocess([str(temp_project), "-o", str(output_file), "--format", "md"])
        assert result.returncode == 0
        assert output_file.exists()

        content = output_file.read_text(encoding="utf-8")
        assert f"# {temp_project.name}/" in content

    def test_md_preserves_code_content(self, temp_project):
        test_file = temp_project / "test.py"
        test_file.write_text("def hello():\n    return 42\n", encoding="utf-8")

        result = run_treemapper_subprocess([str(temp_project), "--format", "md"])
        assert result.returncode == 0
        assert "def hello():" in result.stdout
        assert "```python" in result.stdout


class TestMarkdownIntegration:
    def test_real_directory_to_markdown(self, project_builder):
        project_builder.add_file("main.py", "def main():\n    pass\n")
        project_builder.add_file("utils/helper.py", "def help():\n    return 1\n")
        project_builder.add_file("README.md", "# Project\n\nDescription\n")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert f"# {project_builder.root.name}/" in md
        assert "```python" in md
        assert "```markdown" in md
        assert "def main():" in md

    def test_markdown_unicode_content(self, project_builder):
        project_builder.add_file("unicode.txt", "Привет мир 🎉\n你好世界\n")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "Привет мир" in md
        assert "🎉" in md
        assert "你好世界" in md

    def test_markdown_multiline_content(self, project_builder):
        content = "line1\nline2\nline3\n"
        project_builder.add_file("multi.txt", content)

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "line1" in md
        assert "line2" in md
        assert "line3" in md

    def test_markdown_special_chars_in_content(self, project_builder):
        content = "special: *bold* _italic_ `code` [link](url)\n"
        project_builder.add_file("special.txt", content)

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "*bold*" in md
        assert "_italic_" in md


class TestMarkdownEdgeCases:
    def test_deeply_nested_structure(self, project_builder):
        project_builder.add_file("a/b/c/d/e/f/g/deep.txt", "deep content")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "deep content" in md
        assert "- **" in md

    def test_deeply_nested_code_block_indentation(self):
        tree = {
            "name": "root",
            "type": "directory",
            "children": [
                {
                    "name": "l1",
                    "type": "directory",
                    "children": [
                        {
                            "name": "l2",
                            "type": "directory",
                            "children": [
                                {
                                    "name": "l3",
                                    "type": "directory",
                                    "children": [
                                        {
                                            "name": "l4",
                                            "type": "directory",
                                            "children": [
                                                {
                                                    "name": "l5",
                                                    "type": "directory",
                                                    "children": [
                                                        {
                                                            "name": "l6",
                                                            "type": "directory",
                                                            "children": [
                                                                {"name": "deep.py", "type": "file", "content": "x = 1\n"}
                                                            ],
                                                        }
                                                    ],
                                                }
                                            ],
                                        }
                                    ],
                                }
                            ],
                        }
                    ],
                }
            ],
        }
        md = tree_to_string(tree, "md")
        assert "- **deep.py**" in md
        assert "  ```python" in md
        assert "  x = 1" in md
        assert "  ```\n" in md

    def test_content_with_backticks_escaped(self, project_builder):
        project_builder.add_file("backticks.md", "```python\ncode\n```\n")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "````" in md

    def test_single_backtick_still_uses_triple_fence(self, project_builder):
        project_builder.add_file("single.py", "x = `value`\n")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "```python" in md
        # Ensure we don't have double backticks as fence (should be triple)
        assert "\n``python" not in md

    def test_empty_files(self, project_builder):
        project_builder.add_file("empty.py", "")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "empty.py" in md

    def test_no_children(self):
        tree = {"name": "empty", "type": "directory"}
        output = io.StringIO()
        write_tree_markdown(output, tree)
        result = output.getvalue()

        assert "# empty/" in result

    def test_empty_directory_shows_marker(self):
        tree = {
            "name": "root",
            "type": "directory",
            "children": [{"name": "empty_dir", "type": "directory"}],
        }
        md = tree_to_string(tree, "md")
        assert "## empty_dir/" in md
        assert "_(empty directory)_" in md

    def test_gitignore_has_language_hint(self, project_builder):
        project_builder.add_file(".gitignore", "*.pyc\n")
        project_builder.add_file(".dockerignore", "*.log\n")

        tree = map_directory(project_builder.root)
        md = tree_to_string(tree, "md")

        assert "```gitignore" in md
