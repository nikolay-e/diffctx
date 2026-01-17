from pathlib import Path

from treemapper.diffctx.fragments import (
    GenericStrategy,
    ParagraphStrategy,
    _compute_bracket_balance,
    _find_balanced_end_line,
    _find_indent_safe_end_line,
    _find_sentence_boundary,
    _find_smart_split_point,
    fragment_file,
)


class TestBracketBalance:
    def test_empty_text_is_balanced(self):
        assert _compute_bracket_balance("") == 0

    def test_no_brackets_is_balanced(self):
        assert _compute_bracket_balance("hello world") == 0

    def test_balanced_braces(self):
        assert _compute_bracket_balance("{ x = 1 }") == 0

    def test_unbalanced_open_brace(self):
        assert _compute_bracket_balance("{ x = 1") == 1

    def test_unbalanced_close_brace(self):
        assert _compute_bracket_balance("x = 1 }") == 0

    def test_nested_balanced(self):
        assert _compute_bracket_balance("{ [ ( ) ] }") == 0

    def test_nested_unbalanced(self):
        assert _compute_bracket_balance("{ [ ( }") == 3

    def test_brackets_in_string_ignored(self):
        assert _compute_bracket_balance('x = "{ [ ("') == 0

    def test_escaped_quote_handled(self):
        assert _compute_bracket_balance('x = "hello \\" world"') == 0

    def test_escaped_backslash_before_quote(self):
        assert _compute_bracket_balance('x = "test\\\\" {') == 1

    def test_odd_backslashes_escapes_quote(self):
        assert _compute_bracket_balance('x = "test\\\\\\" {') == 0

    def test_javascript_function(self):
        code = """function foo() {
    const arr = [1, 2, 3];
    return arr.map((x) => x * 2);
}"""
        assert _compute_bracket_balance(code) == 0

    def test_incomplete_javascript_function(self):
        code = """function foo() {
    const arr = [1, 2, 3];
    return arr.map((x) => x * 2);"""
        assert _compute_bracket_balance(code) == 1


class TestFindBalancedEndLine:
    def test_already_balanced(self):
        lines = ["function foo() {", "  return 1;", "}"]
        result = _find_balanced_end_line(lines, 0, 2)
        assert result == 2

    def test_extend_to_close_brace(self):
        lines = ["function foo() {", "  const x = {", "    a: 1", "  };", "}"]
        result = _find_balanced_end_line(lines, 0, 2)
        assert result == 4

    def test_shrink_to_balanced_point(self):
        lines = ["function foo() {", "  return 1;", "}", "function bar() {"]
        result = _find_balanced_end_line(lines, 0, 3)
        assert result == 2

    def test_no_brackets_returns_target(self):
        lines = ["hello", "world", "test"]
        result = _find_balanced_end_line(lines, 0, 2)
        assert result == 2


class TestFindIndentSafeEndLine:
    def test_safe_at_dedent(self):
        lines = ["def foo():", "    x = 1", "    y = 2", "def bar():"]
        result = _find_indent_safe_end_line(lines, 0, 2)
        assert result == 2

    def test_unsafe_mid_indent(self):
        lines = ["def foo():", "    if True:", "        x = 1", "        y = 2", "    z = 3"]
        result = _find_indent_safe_end_line(lines, 0, 3)
        assert result == 3

    def test_empty_line_is_safe(self):
        lines = ["def foo():", "    x = 1", "", "def bar():"]
        result = _find_indent_safe_end_line(lines, 0, 2)
        assert result == 2


class TestFindSentenceBoundary:
    def test_find_period_end(self):
        lines = ["This is a sentence.", "This is another."]
        result = _find_sentence_boundary(lines, 0, 0)
        assert result == 0

    def test_find_question_end(self):
        lines = ["What is this?", "It is a test."]
        result = _find_sentence_boundary(lines, 0, 0)
        assert result == 0

    def test_no_sentence_end_returns_target(self):
        lines = ["This is incomplete", "text without period"]
        result = _find_sentence_boundary(lines, 0, 1)
        assert result == 1

    def test_finds_sentence_boundary_in_range(self):
        lines = ["First sentence.", "Second sentence.", "Third continues"]
        result = _find_sentence_boundary(lines, 0, 2)
        assert result == 1


class TestGenericStrategyIntegrity:
    def test_javascript_function_not_split_mid_body(self):
        code = "\n".join([f"line {i}" for i in range(100)])
        code += "\nfunction foo() {\n"
        code += "\n".join([f"    const x{i} = {i};" for i in range(50)])
        code += "\n    return x0;\n}\n"
        code += "\n".join([f"line {i}" for i in range(100, 150)])

        fragmenter = GenericStrategy()
        frags = fragmenter.fragment(Path("test.js"), code)

        for frag in frags:
            balance = _compute_bracket_balance(frag.content)
            assert balance == 0, f"Unbalanced brackets in fragment: {frag.id}"

    def test_python_creates_fragments(self):
        lines = []
        for i in range(180):
            lines.append(f"x{i} = {i}")
        lines.append("def foo():")
        for i in range(30):
            lines.append(f"    y{i} = {i}")
        lines.append("    return y0")
        lines.append("")
        lines.append("z = 1")

        code = "\n".join(lines)
        fragmenter = GenericStrategy()
        frags = fragmenter.fragment(Path("test.py"), code)

        assert len(frags) >= 1, "Should create at least one fragment"
        total_lines = sum(f.line_count for f in frags)
        assert total_lines == len(lines), "All lines should be covered"


class TestFragmentFileIntegrity:
    def test_typescript_file_balanced(self):
        code = """
interface User {
    name: string;
    age: number;
}

function processUser(user: User): void {
    console.log(user.name);
    if (user.age > 18) {
        console.log("Adult");
    }
}

const users: User[] = [
    { name: "Alice", age: 25 },
    { name: "Bob", age: 17 }
];

users.forEach((user) => {
    processUser(user);
});
"""
        frags = fragment_file(Path("test.ts"), code)

        for frag in frags:
            balance = _compute_bracket_balance(frag.content)
            assert balance == 0, f"Unbalanced fragment in TS file: {frag.content[:50]}"

    def test_yaml_indent_preserved(self):
        lines = ["database:"]
        for i in range(150):
            lines.append(f"  key{i}: value{i}")
        lines.append("server:")
        for i in range(50):
            lines.append(f"  port{i}: {8000 + i}")

        code = "\n".join(lines)
        frags = fragment_file(Path("config.yaml"), code)

        assert len(frags) >= 1, "Should have at least one fragment"


class TestParagraphStrategyIntegrity:
    def test_large_paragraph_split_at_sentence(self):
        sentences = []
        for i in range(150):
            sentences.append(f"This is sentence number {i}.")
        text = " ".join(sentences)

        fragmenter = ParagraphStrategy()
        frags = fragmenter.fragment(Path("text.txt"), text)

        for frag in frags:
            content = frag.content.rstrip()
            if content and not content.endswith((".", "!", "?")):
                last_word = content.split()[-1] if content.split() else ""
                assert last_word.endswith((".", "!", "?", '"')), f"Fragment should end at sentence boundary: ...{content[-50:]}"


class TestSmartSplitPoint:
    def test_code_file_uses_bracket_balance(self):
        lines = ["function f() {", "  x = 1;", "}"]
        result = _find_smart_split_point(lines, 0, 1, Path("test.js"))
        assert result == 2

    def test_python_file_uses_indent(self):
        lines = ["def foo():", "    x = 1", "    y = 2", "def bar():"]
        result = _find_smart_split_point(lines, 0, 2, Path("test.py"))
        assert result == 2

    def test_yaml_file_uses_indent(self):
        lines = ["key:", "  nested: value", "  other: value2", "key2:"]
        result = _find_smart_split_point(lines, 0, 2, Path("config.yaml"))
        assert result == 2

    def test_text_file_returns_target(self):
        lines = ["hello", "world", "test"]
        result = _find_smart_split_point(lines, 0, 1, Path("readme.txt"))
        assert result == 1
