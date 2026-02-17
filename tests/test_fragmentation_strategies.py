from pathlib import Path

import pytest

from treemapper.diffctx.fragments import (
    ConfigStrategy,
    FragmentationEngine,
    GenericStrategy,
    HTMLStrategy,
    MistuneMarkdownStrategy,
    ParagraphStrategy,
    PySBDTextStrategy,
    PythonAstStrategy,
    RegexMarkdownStrategy,
    RuamelYamlStrategy,
    fragment_file,
)

try:
    from treemapper.diffctx.parsers.tree_sitter import TreeSitterStrategy

    _HAS_TREE_SITTER = True
except ImportError:
    TreeSitterStrategy = None  # type: ignore[assignment,misc]
    _HAS_TREE_SITTER = False


@pytest.mark.skipif(not _HAS_TREE_SITTER, reason="tree-sitter not installed")
class TestTreeSitterStrategy:
    def test_can_handle_python(self):
        strategy = TreeSitterStrategy()
        assert strategy.can_handle(Path("test.py"), "def foo(): pass")

    def test_can_handle_javascript(self):
        strategy = TreeSitterStrategy()
        assert strategy.can_handle(Path("test.js"), "function foo() {}")

    def test_can_handle_typescript(self):
        strategy = TreeSitterStrategy()
        assert strategy.can_handle(Path("test.ts"), "function foo(): void {}")

    def test_cannot_handle_unsupported(self):
        strategy = TreeSitterStrategy()
        assert not strategy.can_handle(Path("test.xyz"), "content")

    def test_fragment_python_function(self):
        strategy = TreeSitterStrategy()
        code = """def hello():
    print("Hello")
    return True

def world():
    print("World")
    return False
"""
        frags = strategy.fragment(Path("test.py"), code)
        assert len(frags) >= 2
        assert any("hello" in f.content for f in frags)
        assert any("world" in f.content for f in frags)

    def test_fragment_javascript_function(self):
        strategy = TreeSitterStrategy()
        code = """function greet(name) {
    console.log("Hello " + name);
    return true;
}

function farewell(name) {
    console.log("Bye " + name);
}
"""
        frags = strategy.fragment(Path("test.js"), code)
        assert len(frags) >= 1

    def test_fragment_go_function(self):
        strategy = TreeSitterStrategy()
        code = """package main

func hello() string {
    return "hello"
}

func world() string {
    return "world"
}
"""
        frags = strategy.fragment(Path("test.go"), code)
        assert len(frags) >= 1

    def test_fragment_rust_function(self):
        strategy = TreeSitterStrategy()
        code = """fn main() {
    println!("Hello");
}

fn helper() -> i32 {
    42
}
"""
        frags = strategy.fragment(Path("test.rs"), code)
        assert len(frags) >= 1

    def test_fragment_java_class(self):
        strategy = TreeSitterStrategy()
        code = """public class Hello {
    public void greet() {
        System.out.println("Hello");
    }
}
"""
        frags = strategy.fragment(Path("Test.java"), code)
        assert len(frags) >= 1

    def test_fragment_cpp_function(self):
        strategy = TreeSitterStrategy()
        code = """#include <iostream>

void hello() {
    std::cout << "Hello" << std::endl;
}

int main() {
    hello();
    return 0;
}
"""
        frags = strategy.fragment(Path("test.cpp"), code)
        assert len(frags) >= 1

    def test_fragment_ruby_method(self):
        strategy = TreeSitterStrategy()
        code = """class Greeter
  def hello
    puts "Hello"
  end

  def world
    puts "World"
  end
end
"""
        frags = strategy.fragment(Path("test.rb"), code)
        assert len(frags) >= 1


class TestPythonAstStrategy:
    def test_can_handle_python(self):
        strategy = PythonAstStrategy()
        assert strategy.can_handle(Path("test.py"), "def foo(): pass")
        assert strategy.can_handle(Path("test.pyw"), "def foo(): pass")
        assert strategy.can_handle(Path("test.pyi"), "def foo(): pass")

    def test_cannot_handle_other(self):
        strategy = PythonAstStrategy()
        assert not strategy.can_handle(Path("test.js"), "function foo() {}")

    def test_fragment_function(self):
        strategy = PythonAstStrategy()
        code = """def hello():
    print("Hello")
    return True
"""
        frags = strategy.fragment(Path("test.py"), code)
        assert len(frags) >= 1
        assert frags[0].kind == "function"

    def test_fragment_class(self):
        strategy = PythonAstStrategy()
        code = """class MyClass:
    def __init__(self):
        self.value = 1

    def method(self):
        return self.value
"""
        frags = strategy.fragment(Path("test.py"), code)
        assert any(f.kind == "class" for f in frags)

    def test_fragment_async_function(self):
        strategy = PythonAstStrategy()
        code = """async def fetch_data():
    await something()
    return data
"""
        frags = strategy.fragment(Path("test.py"), code)
        assert len(frags) >= 1
        assert frags[0].kind == "function"

    def test_fragment_decorated_function(self):
        strategy = PythonAstStrategy()
        code = """@decorator
def decorated():
    return True
"""
        frags = strategy.fragment(Path("test.py"), code)
        assert len(frags) >= 1
        assert "@decorator" in frags[0].content

    def test_syntax_error_returns_empty(self):
        strategy = PythonAstStrategy()
        code = "def broken(:\n    pass"
        frags = strategy.fragment(Path("test.py"), code)
        assert frags == []

    def test_empty_content(self):
        strategy = PythonAstStrategy()
        frags = strategy.fragment(Path("test.py"), "")
        assert frags == []


class TestMistuneMarkdownStrategy:
    def test_availability(self):
        strategy = MistuneMarkdownStrategy()
        assert isinstance(strategy._available, bool)

    def test_can_handle_markdown(self):
        strategy = MistuneMarkdownStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert strategy.can_handle(Path("README.md"), "# Title")
        assert strategy.can_handle(Path("doc.markdown"), "# Title")
        assert strategy.can_handle(Path("page.mdx"), "# Title")

    def test_cannot_handle_other(self):
        strategy = MistuneMarkdownStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert not strategy.can_handle(Path("test.py"), "# comment")

    def test_fragment_headings(self):
        strategy = MistuneMarkdownStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        content = """# Title

Some intro text.

## Section 1

Content of section 1.

## Section 2

Content of section 2.
"""
        frags = strategy.fragment(Path("doc.md"), content)
        assert len(frags) >= 2

    def test_empty_content(self):
        strategy = MistuneMarkdownStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        frags = strategy.fragment(Path("doc.md"), "")
        assert frags == []

    def test_no_headings(self):
        strategy = MistuneMarkdownStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        content = "Just some text without any headings."
        frags = strategy.fragment(Path("doc.md"), content)
        assert frags == []


class TestRegexMarkdownStrategy:
    def test_can_handle_markdown(self):
        strategy = RegexMarkdownStrategy()
        assert strategy.can_handle(Path("README.md"), "# Title")
        assert strategy.can_handle(Path("doc.markdown"), "# Title")

    def test_fragment_headings(self):
        strategy = RegexMarkdownStrategy()
        content = """# Main Title

Introduction paragraph.

## First Section

First section content.

## Second Section

Second section content.
"""
        frags = strategy.fragment(Path("doc.md"), content)
        assert len(frags) >= 2

    def test_nested_headings(self):
        strategy = RegexMarkdownStrategy()
        content = """# Level 1

## Level 2

### Level 3

Content here.

## Another Level 2

More content.
"""
        frags = strategy.fragment(Path("doc.md"), content)
        assert len(frags) >= 3

    def test_empty_content(self):
        strategy = RegexMarkdownStrategy()
        frags = strategy.fragment(Path("doc.md"), "")
        assert frags == []

    def test_no_headings(self):
        strategy = RegexMarkdownStrategy()
        content = "Just plain text without headings."
        frags = strategy.fragment(Path("doc.md"), content)
        assert frags == []


class TestPySBDTextStrategy:
    def test_availability(self):
        strategy = PySBDTextStrategy()
        assert isinstance(strategy._available, bool)

    def test_can_handle_text(self):
        strategy = PySBDTextStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert strategy.can_handle(Path("doc.txt"), "Some text.")
        assert strategy.can_handle(Path("doc.text"), "Some text.")
        assert strategy.can_handle(Path("doc.rst"), "Some text.")

    def test_cannot_handle_code(self):
        strategy = PySBDTextStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert not strategy.can_handle(Path("test.py"), "# comment")

    def test_fragment_paragraphs(self):
        strategy = PySBDTextStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        content = """This is the first paragraph. It has multiple sentences. Here is another one.

This is the second paragraph. It also has content. More sentences follow here for testing.

And a third paragraph with enough words to meet the minimum threshold for fragments.
"""
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1

    def test_empty_content(self):
        strategy = PySBDTextStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        frags = strategy.fragment(Path("doc.txt"), "")
        assert frags == []


class TestParagraphStrategy:
    def test_can_handle_text(self):
        strategy = ParagraphStrategy()
        assert strategy.can_handle(Path("doc.txt"), "Some text.")
        assert strategy.can_handle(Path("doc.rst"), "Some text.")

    def test_fragment_paragraphs(self):
        strategy = ParagraphStrategy()
        content = """This is the first paragraph with enough words to meet the minimum threshold for fragments.

This is the second paragraph that also contains enough words for the minimum word count requirement.

And a third paragraph with more words than needed for testing purposes in this unit test.
"""
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1

    def test_empty_content(self):
        strategy = ParagraphStrategy()
        frags = strategy.fragment(Path("doc.txt"), "")
        assert frags == []

    def test_merge_small_paragraphs(self):
        strategy = ParagraphStrategy()
        content = """First paragraph with more than ten words needed for the minimum word count threshold.

Second paragraph that also needs more than ten words to pass the minimum word count check.

Third paragraph containing enough words to satisfy the minimum fragment word requirement here.
"""
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1


class TestHTMLStrategy:
    def test_availability(self):
        strategy = HTMLStrategy()
        assert isinstance(strategy._available, bool)

    def test_can_handle_html(self):
        strategy = HTMLStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert strategy.can_handle(Path("page.html"), "<html></html>")
        assert strategy.can_handle(Path("page.htm"), "<html></html>")
        assert strategy.can_handle(Path("page.xhtml"), "<html></html>")
        assert strategy.can_handle(Path("data.xml"), "<root></root>")

    def test_cannot_handle_other(self):
        strategy = HTMLStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert not strategy.can_handle(Path("test.py"), "# comment")

    def test_fragment_sections(self):
        strategy = HTMLStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        content = """<html>
<body>
<section>
    <h1>Title</h1>
    <p>Content paragraph one.</p>
    <p>Content paragraph two.</p>
</section>
<article>
    <h2>Article Title</h2>
    <p>Article content here.</p>
</article>
</body>
</html>
"""
        frags = strategy.fragment(Path("page.html"), content)
        assert len(frags) >= 1

    def test_empty_content(self):
        strategy = HTMLStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        frags = strategy.fragment(Path("page.html"), "")
        assert frags == []

    def test_invalid_html(self):
        strategy = HTMLStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        frags = strategy.fragment(Path("page.html"), "<<<invalid>>>")
        assert isinstance(frags, list)


class TestRuamelYamlStrategy:
    def test_availability(self):
        strategy = RuamelYamlStrategy()
        assert isinstance(strategy._available, bool)

    def test_can_handle_yaml(self):
        strategy = RuamelYamlStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert strategy.can_handle(Path("config.yaml"), "key: value")
        assert strategy.can_handle(Path("config.yml"), "key: value")

    def test_cannot_handle_other(self):
        strategy = RuamelYamlStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        assert not strategy.can_handle(Path("test.py"), "# comment")

    def test_fragment_yaml(self):
        strategy = RuamelYamlStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        content = """database:
  host: localhost
  port: 5432
  name: mydb

server:
  host: 0.0.0.0
  port: 8080
  debug: true

logging:
  level: INFO
  format: json
"""
        frags = strategy.fragment(Path("config.yaml"), content)
        assert len(frags) >= 2

    def test_empty_content(self):
        strategy = RuamelYamlStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        frags = strategy.fragment(Path("config.yaml"), "")
        assert frags == []

    def test_invalid_yaml(self):
        strategy = RuamelYamlStrategy()
        if not strategy._available:
            pytest.skip("optional dependency not installed")
        frags = strategy.fragment(Path("config.yaml"), "invalid: yaml: content:")
        assert isinstance(frags, list)


class TestConfigStrategy:
    def test_can_handle_configs(self):
        strategy = ConfigStrategy()
        assert strategy.can_handle(Path("config.yaml"), "key: value")
        assert strategy.can_handle(Path("config.yml"), "key: value")
        assert strategy.can_handle(Path("config.json"), '{"key": "value"}')
        assert strategy.can_handle(Path("config.toml"), "[section]")

    def test_fragment_yaml(self):
        strategy = ConfigStrategy()
        content = """database:
  host: localhost

server:
  port: 8080

logging:
  level: info
"""
        frags = strategy.fragment(Path("config.yaml"), content)
        assert len(frags) >= 2

    def test_fragment_toml(self):
        strategy = ConfigStrategy()
        content = """[database]
host = "localhost"
port = 5432

[server]
host = "0.0.0.0"
port = 8080
"""
        frags = strategy.fragment(Path("config.toml"), content)
        assert len(frags) >= 2

    def test_fragment_json(self):
        strategy = ConfigStrategy()
        content = """{
  "database": {
    "host": "localhost"
  },
  "server": {
    "port": 8080
  }
}"""
        frags = strategy.fragment(Path("config.json"), content)
        assert len(frags) >= 1

    def test_empty_content(self):
        strategy = ConfigStrategy()
        frags = strategy.fragment(Path("config.yaml"), "")
        assert frags == []


class TestGenericStrategy:
    def test_can_handle_anything(self):
        strategy = GenericStrategy()
        assert strategy.can_handle(Path("any.file"), "content")
        assert strategy.can_handle(Path("unknown.xyz"), "content")

    def test_fragment_small_file(self):
        strategy = GenericStrategy()
        content = "line 1\nline 2\nline 3\n"
        frags = strategy.fragment(Path("test.txt"), content)
        assert len(frags) >= 1

    def test_fragment_large_file(self):
        strategy = GenericStrategy()
        lines = [f"line {i}" for i in range(300)]
        content = "\n".join(lines)
        frags = strategy.fragment(Path("test.txt"), content)
        assert len(frags) >= 2

    def test_empty_content(self):
        strategy = GenericStrategy()
        frags = strategy.fragment(Path("test.txt"), "")
        assert frags == []


class TestFragmentationEngine:
    def test_python_uses_tree_sitter_or_ast(self):
        engine = FragmentationEngine()
        code = """def hello():
    print("Hello")
    return True
"""
        frags = engine.fragment(Path("test.py"), code)
        assert len(frags) >= 1

    def test_markdown_uses_mistune_or_regex(self):
        engine = FragmentationEngine()
        content = """# Title

## Section 1

Content here.

## Section 2

More content.
"""
        frags = engine.fragment(Path("doc.md"), content)
        assert len(frags) >= 1

    def test_yaml_uses_ruamel_or_config(self):
        engine = FragmentationEngine()
        content = """database:
  host: localhost

server:
  port: 8080
"""
        frags = engine.fragment(Path("config.yaml"), content)
        assert len(frags) >= 1

    def test_unknown_file_uses_generic(self):
        engine = FragmentationEngine()
        content = "some content\nmore content\n"
        frags = engine.fragment(Path("file.xyz"), content)
        assert len(frags) >= 1


class TestFragmentFileFunction:
    def test_fragment_file_python(self):
        code = """def hello():
    print("Hello")
    return True
"""
        frags = fragment_file(Path("test.py"), code)
        assert len(frags) >= 1

    def test_fragment_file_markdown(self):
        content = """# Title

## Section

Content here.
"""
        frags = fragment_file(Path("doc.md"), content)
        assert len(frags) >= 1

    def test_fragment_file_empty(self):
        frags = fragment_file(Path("test.py"), "")
        assert frags == []
