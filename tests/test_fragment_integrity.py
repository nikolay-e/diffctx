from pathlib import Path

import pytest

from treemapper.diffctx.fragments import (
    PySBDTextStrategy,
    _compute_bracket_balance,
    fragment_file,
)

_CONTAINER_HEADER_KINDS = frozenset({"class", "interface", "struct"})


def _assert_all_fragments_bracket_balanced(frags):
    for frag in frags:
        if frag.kind in _CONTAINER_HEADER_KINDS:
            continue
        balance = _compute_bracket_balance(frag.content)
        assert balance == 0, f"Unbalanced brackets (balance={balance}) in fragment {frag.id}: {frag.content[:80]}"


def _assert_fragments_span_file(frags, total_file_lines):
    if total_file_lines == 0:
        return
    min_start = min(f.id.start_line for f in frags)
    max_end = max(f.id.end_line for f in frags)
    assert min_start == 1, f"First fragment should start at line 1, starts at {min_start}"
    assert (
        max_end >= total_file_lines - 1
    ), f"Last fragment should reach near end of file (line {total_file_lines}), ends at {max_end}"


class TestBracketBalancedFragments:
    def test_typescript_interface_and_functions(self):
        code = """interface User {
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
        _assert_all_fragments_bracket_balanced(frags)

    def test_javascript_large_file_with_function(self):
        code = "\n".join([f"var line{i} = {i};" for i in range(100)])
        code += "\nfunction foo() {\n"
        code += "\n".join([f"    const x{i} = {i};" for i in range(50)])
        code += "\n    return x0;\n}\n"
        code += "\n".join([f"var line{i} = {i};" for i in range(100, 150)])

        frags = fragment_file(Path("test.js"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_deeply_nested_javascript_functions(self):
        code = "function outer() {\n"
        for i in range(10):
            code += "  " * (i + 1) + f"function level{i}() {{\n"
        code += "  " * 11 + "return 42;\n"
        for i in range(9, -1, -1):
            code += "  " * (i + 1) + "}\n"
        code += "}\n"

        frags = fragment_file(Path("nested.js"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_javascript_strings_containing_brackets(self):
        code = """function render() {
    const html = '<div class="container">';
    const json = '{"key": [1, 2, 3]}';
    const regex = /\\{[^}]*\\}/g;
    return html + json;
}
"""
        frags = fragment_file(Path("strings.js"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_javascript_escaped_quotes_with_brackets(self):
        code = """function build() {
    const msg = "say \\"hello\\" {world}";
    const path = 'C:\\\\Users\\\\test\\\\{dir}';
    return msg;
}
"""
        frags = fragment_file(Path("escaped.js"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_go_file_with_maps_and_loops(self):
        code = """package main

import "fmt"

func main() {
    data := map[string][]int{
        "a": {1, 2, 3},
        "b": {4, 5, 6},
    }
    for key, values := range data {
        for _, v := range values {
            fmt.Printf("%s: %d\\n", key, v)
        }
    }
}

func helper(x int) int {
    if x > 0 {
        return x * 2
    }
    return 0
}
"""
        frags = fragment_file(Path("main.go"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_rust_file_with_nested_generics(self):
        code = """fn main() {
    let data: Vec<(String, Vec<i32>)> = vec![
        ("a".to_string(), vec![1, 2, 3]),
        ("b".to_string(), vec![4, 5, 6]),
    ];
    for (key, values) in &data {
        for v in values {
            println!("{}: {}", key, v);
        }
    }
}

fn helper(x: i32) -> i32 {
    if x > 0 {
        x * 2
    } else {
        0
    }
}
"""
        frags = fragment_file(Path("main.rs"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_json_large_object(self):
        entries = ",\n".join([f'  "key{i}": {i}' for i in range(100)])
        code = "{\n" + entries + "\n}"

        frags = fragment_file(Path("data.json"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_java_class_with_nested_structures(self):
        code = """public class DataProcessor {
    private Map<String, List<Integer>> data = new HashMap<>();

    public void process(String key) {
        if (data.containsKey(key)) {
            for (Integer val : data.get(key)) {
                System.out.println(val);
            }
        }
    }

    public void add(String key, int value) {
        data.computeIfAbsent(key, k -> new ArrayList<>()).add(value);
    }
}
"""
        frags = fragment_file(Path("DataProcessor.java"), code)
        _assert_all_fragments_bracket_balanced(frags)


class TestLineCoverage:
    def test_python_large_file_fragments_span_entire_file(self):
        lines = [f"x{i} = {i}" for i in range(180)]
        lines.append("def foo():")
        for i in range(30):
            lines.append(f"    y{i} = {i}")
        lines.append("    return y0")
        lines.append("")
        lines.append("z = 1")

        code = "\n".join(lines)
        frags = fragment_file(Path("test.py"), code)
        assert len(frags) >= 1
        _assert_fragments_span_file(frags, len(lines))

    def test_mixed_python_content_fragments_span_entire_file(self):
        lines = [f"# Comment {i}" for i in range(50)]
        lines.append("def real_function():")
        for i in range(50):
            lines.append(f"    statement_{i} = {i}")
        lines.append("    return statement_0")
        lines.append("")
        for i in range(50):
            lines.append(f"CONSTANT_{i} = {i}")

        code = "\n".join(lines)
        frags = fragment_file(Path("mixed.py"), code)
        assert len(frags) >= 1
        _assert_fragments_span_file(frags, len(lines))

    def test_javascript_large_file_fragments_span_entire_file(self):
        lines = []
        for i in range(10):
            lines.append(f"function func{i}() {{")
            for j in range(15):
                lines.append(f"    const v{j} = {j};")
            lines.append("    return v0;")
            lines.append("}")
            lines.append("")

        code = "\n".join(lines)
        frags = fragment_file(Path("large.js"), code)
        assert len(frags) >= 1
        _assert_fragments_span_file(frags, len(lines))


class TestTextBoundaries:
    def test_large_text_non_final_fragments_end_at_sentence_boundary(self):
        sentences = [f"This is sentence number {i}." for i in range(150)]
        text = " ".join(sentences)

        frags = fragment_file(Path("text.txt"), text)

        for frag in frags[:-1]:
            content = frag.content.rstrip()
            if content:
                last_char = content[-1]
                assert last_char in '.!?"', f"Non-final fragment should end at sentence boundary: ...{content[-50:]}"


class TestEdgeCases:
    def test_empty_file(self):
        frags = fragment_file(Path("empty.py"), "")
        assert isinstance(frags, list)

    def test_single_line_file(self):
        frags = fragment_file(Path("single.py"), "x = 1")
        assert len(frags) >= 1

    def test_whitespace_only_file(self):
        frags = fragment_file(Path("blank.py"), "   \n\n   \n")
        assert isinstance(frags, list)

    def test_yaml_large_file_produces_fragments(self):
        lines = ["database:"]
        for i in range(150):
            lines.append(f"  key{i}: value{i}")
        lines.append("server:")
        for i in range(50):
            lines.append(f"  port{i}: {8000 + i}")

        code = "\n".join(lines)
        frags = fragment_file(Path("config.yaml"), code)
        assert len(frags) >= 1

    def test_markdown_produces_fragments(self):
        lines = ["# Title", "", "Some intro text.", ""]
        for i in range(50):
            lines.append(f"Line {i} of content.")
        lines.append("")
        lines.append("## Section 2")
        lines.append("")
        for i in range(50):
            lines.append(f"More content line {i}.")

        code = "\n".join(lines)
        frags = fragment_file(Path("doc.md"), code)
        assert len(frags) >= 1

    def test_file_with_no_newline_at_end(self):
        code = "x = 1\ny = 2\nz = 3"
        frags = fragment_file(Path("no_newline.py"), code)
        assert len(frags) >= 1

    def test_file_with_very_long_single_line(self):
        code = "x = " + " + ".join([f'"{i}"' for i in range(500)])
        frags = fragment_file(Path("long_line.py"), code)
        assert isinstance(frags, list)
        assert len(frags) >= 1

    def test_shell_script_bracket_balanced(self):
        code = """#!/bin/bash

if [ -f "config.txt" ]; then
    while read -r line; do
        if [[ "$line" == *"="* ]]; then
            key="${line%%=*}"
            value="${line#*=}"
            echo "$key -> $value"
        fi
    done < "config.txt"
fi

for i in $(seq 1 10); do
    echo "Item $i"
done
"""
        frags = fragment_file(Path("script.sh"), code)
        _assert_all_fragments_bracket_balanced(frags)

    def test_c_file_bracket_balanced(self):
        code = """#include <stdio.h>

typedef struct {
    int x;
    int y;
} Point;

int distance(Point a, Point b) {
    int dx = a.x - b.x;
    int dy = a.y - b.y;
    return dx * dx + dy * dy;
}

int main() {
    Point p1 = {0, 0};
    Point p2 = {3, 4};
    printf("Distance: %d\\n", distance(p1, p2));
    return 0;
}
"""
        frags = fragment_file(Path("main.c"), code)
        _assert_all_fragments_bracket_balanced(frags)


class TestPySBDLineTracking:
    @pytest.fixture(autouse=True)
    def _require_pysbd(self):
        pytest.importorskip("pysbd")

    def _assert_fragment_lines_match_content(self, frags, content):
        all_lines = content.splitlines()
        for frag in frags:
            assert 1 <= frag.start_line <= len(all_lines), f"start_line {frag.start_line} out of range [1, {len(all_lines)}]"
            assert (
                frag.start_line <= frag.end_line <= len(all_lines)
            ), f"end_line {frag.end_line} out of range [{frag.start_line}, {len(all_lines)}]"
            expected = "\n".join(all_lines[frag.start_line - 1 : frag.end_line])
            if not expected.endswith("\n"):
                expected += "\n"
            assert frag.content == expected, (
                f"Fragment lines {frag.start_line}-{frag.end_line}: "
                f"content does not match original at those positions.\n"
                f"Expected:\n{expected!r}\n"
                f"Got:\n{frag.content!r}"
            )

    def test_fragment_content_matches_reported_line_positions(self):
        content = (
            "This is the first paragraph. It has multiple sentences here.\n"
            "Each sentence in this paragraph stays on one line. Another follows.\n"
            "\n"
            "This is the second paragraph that appears after a blank line.\n"
            "It spans multiple lines and has several sentences that\n"
            "cross line boundaries here. This is an important detail to test.\n"
            "\n"
            "Third paragraph also has enough words to meet the minimum threshold.\n"
            "It includes a second line to provide additional testing content here.\n"
        )
        strategy = PySBDTextStrategy()
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1
        self._assert_fragment_lines_match_content(frags, content)

    def test_multiple_sentences_on_single_line(self):
        content = (
            "First sentence here. Second sentence follows. Third sentence now. "
            "Fourth here. Fifth sentence added.\n"
            "Sixth sentence on a new line. Seventh sentence here too. "
            "Eighth sentence completes this.\n"
            "\n"
            "New paragraph starts here with plenty of words. "
            "It also has many sentences inline. More words follow after.\n"
            "Another line in the second paragraph. Extra words for minimum count.\n"
        )
        strategy = PySBDTextStrategy()
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1
        self._assert_fragment_lines_match_content(frags, content)

    def test_sentences_spanning_line_boundaries(self):
        content = (
            "This sentence starts on line one and\n"
            "continues to line two before ending here. Then another sentence\n"
            "also spans across the line boundary here. More words to reach threshold.\n"
            "\n"
            "A new paragraph starts here with enough words to pass the minimum.\n"
            "It has additional content on this line for the word count threshold.\n"
        )
        strategy = PySBDTextStrategy()
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1
        self._assert_fragment_lines_match_content(frags, content)

    def test_many_paragraphs_no_cumulative_drift(self):
        paragraphs = []
        for i in range(10):
            paragraphs.append(
                f"Paragraph number {i} has enough words to pass the minimum word count. "
                f"It also has a second sentence here. And a third for good measure."
            )
        content = "\n\n".join(paragraphs) + "\n"
        strategy = PySBDTextStrategy()
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1
        self._assert_fragment_lines_match_content(frags, content)

    def test_first_paragraph_starts_at_line_one(self):
        content = (
            "The very first paragraph should start at line one in the output.\n"
            "This is a second sentence to make the fragment long enough here.\n"
            "\n"
            "Second paragraph starts after the blank line separator here now.\n"
            "It has another sentence for the minimum word count requirement.\n"
        )
        strategy = PySBDTextStrategy()
        frags = strategy.fragment(Path("doc.txt"), content)
        assert len(frags) >= 1
        assert frags[0].start_line == 1
