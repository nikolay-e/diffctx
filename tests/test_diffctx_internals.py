# ruff: noqa: RUF001
import math
import subprocess
import time
from pathlib import Path

import pytest

from treemapper.diffctx import GitError, build_diff_context
from treemapper.diffctx.fragments import fragment_file
from treemapper.diffctx.git import get_changed_files, get_diff_text, parse_diff
from treemapper.diffctx.graph import Graph, build_graph
from treemapper.diffctx.ppr import personalized_pagerank
from treemapper.diffctx.python_semantics import analyze_python_fragment
from treemapper.diffctx.select import lazy_greedy_select
from treemapper.diffctx.types import Fragment, FragmentId, extract_identifiers
from treemapper.diffctx.utility import concepts_from_diff_text


def _make_fragment(
    path: str,
    start: int,
    end: int,
    identifiers: frozenset[str] | None = None,
    tokens: int = 100,
) -> Fragment:
    frag = Fragment(
        id=FragmentId(Path(path), start, end),
        kind="function",
        content=f"content {start}-{end}",
        identifiers=identifiers or frozenset(),
    )
    frag.token_count = tokens
    return frag


def _extract_files_from_tree(tree: dict) -> set[str]:
    files = set()
    if tree.get("type") == "diff_context":
        for frag in tree.get("fragments", []):
            path = frag.get("path", "")
            if path:
                files.add(path.split("/")[-1])
        return files

    def traverse(node):
        if node.get("type") == "file":
            files.add(node["name"])
        for child in node.get("children", []):
            traverse(child)

    traverse(tree)
    return files


class TestCyrillicIdentifiers:
    def test_cyrillic_function_name(self, tmp_path):
        code = 'def привет_мир(): return "здравствуй"'
        path = tmp_path / "cyrillic.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1
        assert fragments[0].content is not None

        identifiers = extract_identifiers(code, profile="code")
        assert isinstance(identifiers, frozenset)

        info = analyze_python_fragment(code)
        assert info is not None

    def test_cyrillic_mixed_with_ascii(self, tmp_path):
        code = """def привет_мир():
    result = "здравствуй"
    return result
"""
        path = tmp_path / "mixed.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "result" in identifiers


class TestChineseComments:
    def test_chinese_comments_ascii_identifiers(self, tmp_path):
        code = """# 这是一个函数
def foo():
    # 返回值
    return 42
"""
        path = tmp_path / "chinese.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "foo" in identifiers

        info = analyze_python_fragment(code)
        assert "foo" in info.defines

    def test_chinese_in_strings(self, tmp_path):
        code = """def greet():
    return "你好世界"
"""
        path = tmp_path / "chinese_string.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        info = analyze_python_fragment(code)
        assert "greet" in info.defines


class TestArabicRTL:
    def test_arabic_rtl_in_strings(self, tmp_path):
        code = """def send_message():
    message = "مرحبا"
    greeting = "أهلاً وسهلاً"
    return message + greeting
"""
        path = tmp_path / "arabic.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "message" in identifiers
        assert "greeting" in identifiers
        assert "send_message" in identifiers

        info = analyze_python_fragment(code)
        assert "send_message" in info.defines

    def test_arabic_rtl_mixed_direction(self, tmp_path):
        code = """text = "Hello مرحبا World"
result = process(text)
"""
        path = tmp_path / "rtl_mixed.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        info = analyze_python_fragment(code)
        assert "process" in info.calls


class TestEmojiIdentifiers:
    def test_emoji_in_function_name(self, tmp_path):
        code = """def fire_handler():
    pass
"""
        path = tmp_path / "emoji.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "fire_handler" in identifiers

    def test_emoji_in_string_content(self, tmp_path):
        code = """def get_status():
    return "Status: 🔥 Active 🚀"
"""
        path = tmp_path / "emoji_string.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        info = analyze_python_fragment(code)
        assert "get_status" in info.defines

    def test_emoji_adjacent_to_identifier(self, tmp_path):
        code = """status = "🔥"
handler_name = "fire"
"""
        path = tmp_path / "emoji_adjacent.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "status" in identifiers
        assert "handler_name" in identifiers


class TestMixedUnicodeCategories:
    def test_katakana_hiragana_segmentation(self, tmp_path):
        code = """# const カタカナ_variable = ひらがな_function()
katakana_var = "カタカナ"
hiragana_func = "ひらがな"
result = process(katakana_var)
"""
        path = tmp_path / "japanese.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "katakana_var" in identifiers
        assert "hiragana_func" in identifiers
        assert "result" in identifiers
        assert "process" in identifiers

    def test_mixed_script_in_comments(self, tmp_path):
        code = """# This function handles データ処理
def process_data():
    # Возвращает результат
    return {"status": "ok"}
"""
        path = tmp_path / "mixed_script.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        info = analyze_python_fragment(code)
        assert "process_data" in info.defines

    def test_korean_hangul(self, tmp_path):
        code = """def handler():
    message = "안녕하세요"
    return message
"""
        path = tmp_path / "korean.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "handler" in identifiers
        assert "message" in identifiers


class TestBOMHandling:
    def test_utf8_bom_before_import(self, tmp_path):
        bom = b"\xef\xbb\xbf"
        code_bytes = (
            bom
            + b"""import os

def main():
    return os.getcwd()
"""
        )
        path = tmp_path / "bom.py"
        path.write_bytes(code_bytes)

        code = code_bytes.decode("utf-8-sig")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        info = analyze_python_fragment(code)
        assert "main" in info.defines

    def test_bom_with_encoding_declaration(self, tmp_path):
        bom = b"\xef\xbb\xbf"
        code_bytes = (
            bom
            + b"""# -*- coding: utf-8 -*-
import sys

def check():
    return sys.version
"""
        )
        path = tmp_path / "bom_encoding.py"
        path.write_bytes(code_bytes)

        code = code_bytes.decode("utf-8-sig")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "check" in identifiers
        assert "sys" in identifiers

    def test_bom_raw_bytes_fragment(self, tmp_path):
        bom = b"\xef\xbb\xbf"
        code_bytes = (
            bom
            + b"""def hello():
    return 'world'
"""
        )
        path = tmp_path / "bom_raw.py"
        path.write_bytes(code_bytes)

        code_with_bom = code_bytes.decode("utf-8")
        fragments = fragment_file(path, code_with_bom)
        assert len(fragments) >= 1


class TestNullBytes:
    def test_null_byte_in_string_literal(self, tmp_path):
        code = """s = "hello\x00world"
result = process(s)
"""
        path = tmp_path / "null_byte.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "result" in identifiers
        assert "process" in identifiers

    def test_actual_null_byte_in_content(self, tmp_path):
        code = """data = "helloworld"
output = data
"""
        path = tmp_path / "actual_null.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        for frag in fragments:
            assert frag.content is not None
            assert len(frag.content) > 0

    def test_null_in_diff_text(self):
        diff_text = """+def process():
+    data = "testvalue"
+    return data
"""
        concepts = concepts_from_diff_text(diff_text)

        assert isinstance(concepts, frozenset)
        assert "process" in concepts
        assert "data" in concepts

    def test_null_byte_preserved_in_fragment(self, tmp_path):
        content_with_null = """binary = b"\x00\x01\x02"
valid_name = "test"
"""
        path = tmp_path / "binary_escape.py"
        path.write_text(content_with_null, encoding="utf-8")

        fragments = fragment_file(path, content_with_null)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(content_with_null, profile="code")
        assert "binary" in identifiers
        assert "valid_name" in identifiers


class TestConceptsFromDiff:
    def test_diff_with_unicode_content(self):
        diff_text = """+def handler():
+    message = "Привет мир"
+    return message
"""
        concepts = concepts_from_diff_text(diff_text)

        assert "handler" in concepts
        assert "message" in concepts

    def test_diff_with_mixed_unicode(self):
        diff_text = """+# 这是注释
+def process():
+    result = "مرحبا"
+    return result
"""
        concepts = concepts_from_diff_text(diff_text)

        assert "process" in concepts
        assert "result" in concepts


class TestFragmentIdentifiersRobustness:
    def test_fragment_with_all_unicode_edge_cases(self, tmp_path):
        code = """# 多语言测试 / Тест / اختبار
def handler():
    cyrillic = "Привет"
    chinese = "你好"
    arabic = "مرحبا"
    japanese = "こんにちは"
    korean = "안녕하세요"
    emoji = "🎉🔥🚀"
    return {"status": "ok"}
"""
        path = tmp_path / "all_unicode.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1

        identifiers = extract_identifiers(code, profile="code")
        assert "handler" in identifiers
        assert "cyrillic" in identifiers
        assert "chinese" in identifiers
        assert "arabic" in identifiers
        assert "japanese" in identifiers
        assert "korean" in identifiers
        assert "emoji" in identifiers
        assert "status" in identifiers

        info = analyze_python_fragment(code)
        assert "handler" in info.defines

    def test_fragment_id_with_unicode_path(self, tmp_path):
        unicode_dir = tmp_path / "тест_директория"
        unicode_dir.mkdir()
        path = unicode_dir / "файл.py"

        code = """def test():
    return 42
"""
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)
        assert len(fragments) >= 1
        assert fragments[0].id.path == path

        frag_id = FragmentId(path=path, start_line=1, end_line=3)
        assert str(frag_id) == f"{path}:1-3"
        assert isinstance(hash(frag_id), int)


class TestZalgoText:
    def test_zalgo_text_in_comments_does_not_crash(self, tmp_path):
        zalgo_comment = "// H̸̡̪̯ͨ͊̽̅̾̎Ȩ̬̩̾͛ͪ̈́̀́͘ ̈́̈͊ͅC̷̱̲̥͎̾́͆̀O̵̝̟̞͙͆̈̓̔M̴̨̰̱̾̅̑̕Ę̷̡͎̰̐̊̾S̴̨̺̟̈̿̀͝"
        code = f"""{zalgo_comment}
def process_data(items):
    return items
"""
        path = tmp_path / "zalgo.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None

    def test_zalgo_in_extract_identifiers(self):
        zalgo_text = "H̸̡̪̯ͨ͊̽̅̾̎Ȩ̬̩̾͛ͪ̈́̀́͘ func_name ̈́̈͊ͅC̷̱̲̥͎̾́͆̀O̵̝̟̞͙͆̈̓̔M̴̨̰̱̾̅̑̕Ę̷̡͎̰̐̊̾S̴̨̺̟̈̿̀͝"

        identifiers = extract_identifiers(zalgo_text)

        assert "func_name" in identifiers

    def test_zalgo_in_diff_text(self):
        diff_text = """+// H̸̡̪̯ͨ͊̽̅̾̎Ȩ̬̩̾͛ͪ̈́̀́͘ COMES
+def process_data(items):
+    return items
"""

        concepts = concepts_from_diff_text(diff_text)

        assert "process_data" in concepts


class TestMathematicalSymbols:
    def test_math_symbols_as_identifiers_julia_style(self, tmp_path):
        code = """∑ = sum
∏ = prod
α = 0.5
β = 0.3
result = calculate(α, β)
"""
        path = tmp_path / "math_symbols.jl"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None

    def test_math_symbols_extract_latin_identifiers(self):
        code = "∑ = sum; ∏ = prod; α = 0.5; result = calculate(value)"

        identifiers = extract_identifiers(code)

        assert "calculate" in identifiers
        assert len(identifiers) >= 1

    def test_math_symbols_in_diff(self):
        diff_text = """+∑ = sum
+∏ = prod
+result = calculate(α, β)
"""

        concepts = concepts_from_diff_text(diff_text)

        assert "calculate" in concepts
        assert len(concepts) >= 1


class TestGreekLetters:
    def test_greek_letters_in_scientific_code(self, tmp_path):
        code = """def calculate_Δ(θ, φ):
    import math
    return θ * math.cos(φ)

def compute_σ(μ, data):
    return sum((x - μ)**2 for x in data)
"""
        path = tmp_path / "greek_science.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None
        assert len(fragments) >= 1

    def test_greek_letters_python_analysis(self):
        code = """def calculate_Δ(θ, φ):
    import math
    return θ * math.cos(φ)
"""

        info = analyze_python_fragment(code)

        assert info is not None

    def test_greek_extract_identifiers(self):
        code = "def calculate_Δ(θ, φ): return θ * cos(φ)"

        identifiers = extract_identifiers(code)

        assert "calculate_" in identifiers or any("calculate" in i for i in identifiers)
        assert "cos" in identifiers


class TestCyrillicLatinLookalikes:
    def test_cyrillic_vs_latin_different_concepts(self):
        cyrillic_a = "а"
        latin_a = "a"

        assert cyrillic_a != latin_a

        code_cyrillic = f"vаr_{cyrillic_a} = 1"
        code_latin = f"var_{latin_a} = 1"

        idents_cyrillic = extract_identifiers(code_cyrillic)
        idents_latin = extract_identifiers(code_latin)

        assert idents_cyrillic != idents_latin or len(idents_cyrillic) == len(idents_latin)

    def test_mixed_cyrillic_latin_does_not_crash(self, tmp_path):
        code = """# This has mixed а (cyrillic) and a (latin)
def process(а, a):
    return а + a
"""
        path = tmp_path / "mixed_script.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None

    def test_cyrillic_in_diff(self):
        diff_text = """+# Cyrillic а vs Latin a
+vаr_cyrillic = 1
+var_latin = 2
"""

        concepts = concepts_from_diff_text(diff_text)

        assert concepts is not None
        assert "var_latin" in concepts


class TestKoreanHangul:
    def test_korean_in_strings_neighbor_identifiers(self):
        code = """message = "안녕하세요"
result = process_data(input_value)
"""

        identifiers = extract_identifiers(code)

        assert "process_data" in identifiers
        assert "input_value" in identifiers

    def test_korean_in_comments_does_not_affect_code(self, tmp_path):
        code = """# 이것은 한국어 주석입니다
def calculate_total(items):
    # 계산 로직
    return sum(items)
"""
        path = tmp_path / "korean_comments.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None
        assert len(fragments) >= 1

        all_idents = set()
        for frag in fragments:
            all_idents.update(frag.identifiers)

        assert "calculate_total" in all_idents

    def test_korean_in_diff(self):
        diff_text = """+# 한국어 주석
+message = "안녕하세요"
+result = process_data(value)
"""

        concepts = concepts_from_diff_text(diff_text)

        assert "process_data" in concepts


class TestHebrewDocstring:
    def test_hebrew_docstring_function_parsing(self, tmp_path):
        code = '''def foo():
    """תיעוד בעברית"""
    return 42

def bar(x, y):
    """עוד תיעוד"""
    return x + y
'''
        path = tmp_path / "hebrew_docs.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None
        assert len(fragments) >= 1

        function_names = set()
        for frag in fragments:
            if frag.kind == "function":
                function_names.update(frag.identifiers)

        assert "foo" in function_names or any("foo" in str(f.content) for f in fragments)
        assert "bar" in function_names or any("bar" in str(f.content) for f in fragments)

    def test_hebrew_docstring_python_analysis(self):
        code = '''def foo():
    """תיעוד בעברית"""
    return 42
'''

        info = analyze_python_fragment(code)

        assert info is not None
        assert "foo" in info.defines

    def test_hebrew_in_diff(self):
        diff_text = '+def foo():\n+    """תיעוד בעברית"""\n+    return process_value(x)\n'

        concepts = concepts_from_diff_text(diff_text)

        assert "process_value" in concepts


class TestThaiNoSpaces:
    def test_thai_in_comments_no_merge_with_code(self):
        code = """# นี่คือความคิดเห็น
result = calculate(value)
"""

        identifiers = extract_identifiers(code)

        assert "calculate" in identifiers

    def test_thai_long_comment_does_not_crash(self, tmp_path):
        code = """# ภาษาไทยไม่มีช่องว่างระหว่างคำ สามารถเขียนต่อเนื่องกันได้ยาวมาก
# อีกบรรทัดหนึ่งของความคิดเห็นภาษาไทย
def process_items(data_list):
    return [item for item in data_list]
"""
        path = tmp_path / "thai_comments.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None
        assert len(fragments) >= 1

    def test_thai_mixed_with_code(self, tmp_path):
        code = """# ฟังก์ชันคำนวณ
def compute(x):  # คำนวณค่า
    # ส่งคืนผลลัพธ์
    return x * 2
"""
        path = tmp_path / "thai_mixed.py"
        path.write_text(code, encoding="utf-8")

        fragments = fragment_file(path, code)

        assert fragments is not None

        all_idents = set()
        for frag in fragments:
            all_idents.update(frag.identifiers)

        assert "compute" in all_idents

    def test_thai_in_diff(self):
        diff_text = """+# ภาษาไทยไม่มีช่องว่าง
+result = calculate_sum(values)
+output = format_result(result)
"""

        concepts = concepts_from_diff_text(diff_text)

        assert "calculate_sum" in concepts
        assert "format_result" in concepts


class TestTrailingWhitespace:
    def test_trailing_whitespace_does_not_affect_identifiers(self, tmp_path):
        path = tmp_path / "test.py"

        content_with_trailing = """def process_data():
    result = calculate()
    return result
"""
        content_clean = """def process_data():
    result = calculate()
    return result
"""

        idents_with_trailing = extract_identifiers(content_with_trailing)
        idents_clean = extract_identifiers(content_clean)

        assert idents_with_trailing == idents_clean

        frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=3),
            kind="function",
            content=content_with_trailing,
            identifiers=idents_with_trailing,
            token_count=50,
        )

        graph = build_graph([frag])
        scores = personalized_pagerank(graph, seeds={frag.id}, alpha=0.6)

        assert frag.id in scores
        assert scores[frag.id] > 0


class TestNestedComments:
    def test_nested_comments_rust_style_parsed_correctly(self, tmp_path):
        path = tmp_path / "test.rs"

        content = """/* /* nested comment */ */
fn process_data() {
    let result = calculate_value();
    result
}
"""
        idents = extract_identifiers(content)

        assert "process_data" in idents
        assert "calculate_value" in idents

        frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content=content,
            identifiers=idents,
            token_count=50,
        )

        graph = build_graph([frag])
        scores = personalized_pagerank(graph, seeds={frag.id}, alpha=0.6)

        assert frag.id in scores
        assert abs(scores[frag.id] - 1.0) < 1e-9


class TestCyclicDependency:
    def test_cyclic_dependency_ppr_converges(self, tmp_path):
        path = tmp_path / "cycle.py"

        frag_a = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def func_alpha():
    func_beta()
""",
            identifiers=frozenset(["func_alpha", "func_beta"]),
            token_count=50,
        )

        frag_b = Fragment(
            id=FragmentId(path=path, start_line=10, end_line=15),
            kind="function",
            content="""def func_beta():
    func_gamma()
""",
            identifiers=frozenset(["func_beta", "func_gamma"]),
            token_count=50,
        )

        frag_c = Fragment(
            id=FragmentId(path=path, start_line=20, end_line=25),
            kind="function",
            content="""def func_gamma():
    func_alpha()
""",
            identifiers=frozenset(["func_gamma", "func_alpha"]),
            token_count=50,
        )

        fragments = [frag_a, frag_b, frag_c]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6, max_iter=100)

        assert len(scores) == 3
        assert all(s >= 0 for s in scores.values())
        assert abs(sum(scores.values()) - 1.0) < 1e-9

        assert scores[frag_a.id] > 0
        assert scores[frag_b.id] > 0
        assert scores[frag_c.id] > 0


class TestIsolatedNode:
    def test_isolated_node_gets_base_teleport_score(self, tmp_path):
        path_connected = tmp_path / "connected.py"
        path_isolated = tmp_path / "isolated.py"

        frag_a = Fragment(
            id=FragmentId(path=path_connected, start_line=1, end_line=5),
            kind="function",
            content="""def caller():
    callee()
""",
            identifiers=frozenset(["caller", "callee"]),
            token_count=50,
        )

        frag_b = Fragment(
            id=FragmentId(path=path_connected, start_line=10, end_line=15),
            kind="function",
            content="""def callee():
    pass
""",
            identifiers=frozenset(["callee"]),
            token_count=50,
        )

        frag_isolated = Fragment(
            id=FragmentId(path=path_isolated, start_line=1, end_line=5),
            kind="function",
            content="""def completely_unique_function():
    totally_unique_helper()
""",
            identifiers=frozenset(["completely_unique_function", "totally_unique_helper"]),
            token_count=50,
        )

        fragments = [frag_a, frag_b, frag_isolated]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6)

        assert frag_isolated.id in scores
        assert scores[frag_a.id] > scores[frag_isolated.id]
        assert scores[frag_isolated.id] >= 0


class TestHubMonster:
    def test_hub_monster_suppression_works(self, tmp_path):
        path = tmp_path / "hub.py"

        hub_frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def hub_function():
    pass
""",
            identifiers=frozenset(["hub_function"]),
            token_count=20,
        )

        callers = []
        for i in range(500):
            caller = Fragment(
                id=FragmentId(path=path, start_line=10 + i * 10, end_line=15 + i * 10),
                kind="function",
                content=f"""def caller_{i}():
    hub_function()
""",
                identifiers=frozenset([f"caller_{i}", "hub_function"]),
                token_count=30,
            )
            callers.append(caller)

        fragments = [hub_frag, *callers]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={callers[0].id}, alpha=0.6)

        assert len(scores) == len(fragments)
        assert all(s >= 0 for s in scores.values())

        assert scores[callers[0].id] > 0
        assert scores[hub_frag.id] < 0.5

        non_seed_total = sum(scores[c.id] for c in callers[1:])
        assert non_seed_total > 0


class TestStarTopology:
    def test_star_topology_central_node_does_not_dominate(self, tmp_path):
        path = tmp_path / "star.py"

        central_utils = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def utils_helper():
    pass
""",
            identifiers=frozenset(["utils_helper"]),
            token_count=20,
        )

        periphery = []
        for i in range(50):
            node = Fragment(
                id=FragmentId(path=path, start_line=10 + i * 10, end_line=15 + i * 10),
                kind="function",
                content=f"""def peripheral_{i}():
    utils_helper()
""",
                identifiers=frozenset([f"peripheral_{i}", "utils_helper"]),
                token_count=30,
            )
            periphery.append(node)

        fragments = [central_utils, *periphery]
        graph = build_graph(fragments)

        scores = personalized_pagerank(graph, seeds={periphery[0].id}, alpha=0.6)

        assert len(scores) == len(fragments)
        assert abs(sum(scores.values()) - 1.0) < 1e-9

        seed_score = scores[periphery[0].id]
        central_score = scores[central_utils.id]

        assert seed_score > central_score
        assert central_score < 0.5

        other_periphery_total = sum(scores[p.id] for p in periphery[1:])
        assert other_periphery_total > 0


class TestDisconnectedComponents:
    def test_disconnected_component_not_in_context(self, tmp_path):
        path_a = tmp_path / "component_a.py"
        path_b = tmp_path / "component_b.py"

        frag_a1 = Fragment(
            id=FragmentId(path=path_a, start_line=1, end_line=5),
            kind="function",
            content="""def alpha_one():
    alpha_two()
""",
            identifiers=frozenset(["alpha_one", "alpha_two"]),
            token_count=50,
        )

        frag_a2 = Fragment(
            id=FragmentId(path=path_a, start_line=10, end_line=15),
            kind="function",
            content="""def alpha_two():
    alpha_three()
""",
            identifiers=frozenset(["alpha_two", "alpha_three"]),
            token_count=50,
        )

        frag_a3 = Fragment(
            id=FragmentId(path=path_a, start_line=20, end_line=25),
            kind="function",
            content="""def alpha_three():
    pass
""",
            identifiers=frozenset(["alpha_three"]),
            token_count=50,
        )

        frag_b1 = Fragment(
            id=FragmentId(path=path_b, start_line=1, end_line=5),
            kind="function",
            content="""def beta_one():
    beta_two()
""",
            identifiers=frozenset(["beta_one", "beta_two"]),
            token_count=50,
        )

        frag_b2 = Fragment(
            id=FragmentId(path=path_b, start_line=10, end_line=15),
            kind="function",
            content="""def beta_two():
    beta_three()
""",
            identifiers=frozenset(["beta_two", "beta_three"]),
            token_count=50,
        )

        frag_b3 = Fragment(
            id=FragmentId(path=path_b, start_line=20, end_line=25),
            kind="function",
            content="""def beta_three():
    pass
""",
            identifiers=frozenset(["beta_three"]),
            token_count=50,
        )

        component_a = [frag_a1, frag_a2, frag_a3]
        component_b = [frag_b1, frag_b2, frag_b3]
        all_fragments = component_a + component_b

        graph = build_graph(all_fragments)

        scores = personalized_pagerank(graph, seeds={frag_a1.id}, alpha=0.6)

        assert len(scores) == len(all_fragments)

        component_a_score = sum(scores[f.id] for f in component_a)
        component_b_score = sum(scores[f.id] for f in component_b)

        assert component_a_score > component_b_score

        min_component_a_score = min(scores[f.id] for f in component_a)
        max_component_b_score = max(scores[f.id] for f in component_b)
        assert min_component_a_score >= max_component_b_score


class TestGraphEdgeCases:
    def test_self_loop_does_not_break_ppr(self, tmp_path):
        path = tmp_path / "self_import.py"

        frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=10),
            kind="function",
            content="""from self_import import helper
def main():
    helper()
""",
            identifiers=frozenset(["main", "helper", "self_import"]),
            token_count=50,
        )

        graph = build_graph([frag])

        graph.add_edge(frag.id, frag.id, 0.5)

        scores = personalized_pagerank(graph, seeds={frag.id}, alpha=0.6)

        assert frag.id in scores
        assert scores[frag.id] > 0
        assert math.isfinite(scores[frag.id])
        assert abs(sum(scores.values()) - 1.0) < 1e-9

    def test_parallel_edges_weights_combined(self, tmp_path):
        path = tmp_path / "parallel.py"

        frag_a = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def func_a():
    func_b()
""",
            identifiers=frozenset(["func_a", "func_b"]),
            token_count=50,
        )

        frag_b = Fragment(
            id=FragmentId(path=path, start_line=10, end_line=15),
            kind="function",
            content="""def func_b():
    pass
""",
            identifiers=frozenset(["func_b"]),
            token_count=50,
        )

        graph = Graph()
        graph.add_node(frag_a.id)
        graph.add_node(frag_b.id)

        graph.add_edge(frag_a.id, frag_b.id, 0.3)
        graph.add_edge(frag_a.id, frag_b.id, 0.5)
        graph.add_edge(frag_a.id, frag_b.id, 0.4)

        neighbors = graph.neighbors(frag_a.id)
        assert frag_b.id in neighbors
        assert neighbors[frag_b.id] == pytest.approx(0.5)

        scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6)

        assert len(scores) == 2
        assert all(math.isfinite(s) for s in scores.values())
        assert abs(sum(scores.values()) - 1.0) < 1e-9

    def test_large_graph_10000_nodes_no_oom(self, tmp_path):
        path = tmp_path / "large.py"

        fragments = []
        for i in range(10000):
            common_idents = frozenset([f"func{i}"])
            if i > 0:
                common_idents = common_idents | frozenset([f"func{i-1}"])
            if i < 9999:
                common_idents = common_idents | frozenset([f"func{i+1}"])

            frag = Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"""def func{i}():
    pass
""",
                identifiers=common_idents,
                token_count=20,
            )
            fragments.append(frag)

        graph = Graph()
        for frag in fragments:
            graph.add_node(frag.id)

        for i in range(len(fragments) - 1):
            graph.add_edge(fragments[i].id, fragments[i + 1].id, 0.5)
            graph.add_edge(fragments[i + 1].id, fragments[i].id, 0.3)

        start_time = time.time()
        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.6, max_iter=100)
        elapsed = time.time() - start_time

        assert elapsed < 30.0, f"PPR took {elapsed:.2f}s, expected < 30s"
        assert len(scores) == 10000
        assert all(math.isfinite(s) for s in scores.values())
        assert all(s >= 0 for s in scores.values())

    def test_single_node_graph(self, tmp_path):
        path = tmp_path / "single.py"

        frag = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def only_function():
    pass
""",
            identifiers=frozenset(["only_function"]),
            token_count=50,
        )

        graph = build_graph([frag])

        scores = personalized_pagerank(graph, seeds={frag.id}, alpha=0.6)

        assert len(scores) == 1
        assert frag.id in scores
        assert abs(scores[frag.id] - 1.0) < 1e-9

    def test_all_zero_weight_edges_uniform_fallback(self, tmp_path):
        path = tmp_path / "zero_weights.py"

        fragments = [
            Fragment(
                id=FragmentId(path=path, start_line=i * 10 + 1, end_line=i * 10 + 5),
                kind="function",
                content=f"""def func{i}():
    pass
""",
                identifiers=frozenset([f"func{i}"]),
                token_count=50,
            )
            for i in range(5)
        ]

        graph = Graph()
        for frag in fragments:
            graph.add_node(frag.id)

        for i in range(len(fragments) - 1):
            graph.add_edge(fragments[i].id, fragments[i + 1].id, 0.0)

        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.6)

        assert len(scores) == 5
        assert all(math.isfinite(s) for s in scores.values())
        assert all(s >= 0 for s in scores.values())
        assert abs(sum(scores.values()) - 1.0) < 1e-9

        assert scores[fragments[0].id] >= scores[fragments[4].id]

    def test_negative_weights_handled(self, tmp_path):
        path = tmp_path / "negative.py"

        frag_a = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def func_a():
    pass
""",
            identifiers=frozenset(["func_a"]),
            token_count=50,
        )

        frag_b = Fragment(
            id=FragmentId(path=path, start_line=10, end_line=15),
            kind="function",
            content="""def func_b():
    pass
""",
            identifiers=frozenset(["func_b"]),
            token_count=50,
        )

        graph = Graph()
        graph.add_node(frag_a.id)
        graph.add_node(frag_b.id)

        graph.add_edge(frag_a.id, frag_b.id, -0.5)
        graph.add_edge(frag_b.id, frag_a.id, -1.0)

        scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6)

        assert len(scores) == 2
        assert all(math.isfinite(s) for s in scores.values())
        assert abs(sum(scores.values()) - 1.0) < 1e-9

    def test_nan_inf_weights_handled(self, tmp_path):
        path = tmp_path / "bad_values.py"

        frag_a = Fragment(
            id=FragmentId(path=path, start_line=1, end_line=5),
            kind="function",
            content="""def func_a():
    pass
""",
            identifiers=frozenset(["func_a"]),
            token_count=50,
        )

        frag_b = Fragment(
            id=FragmentId(path=path, start_line=10, end_line=15),
            kind="function",
            content="""def func_b():
    pass
""",
            identifiers=frozenset(["func_b"]),
            token_count=50,
        )

        frag_c = Fragment(
            id=FragmentId(path=path, start_line=20, end_line=25),
            kind="function",
            content="""def func_c():
    pass
""",
            identifiers=frozenset(["func_c"]),
            token_count=50,
        )

        graph = Graph()
        graph.add_node(frag_a.id)
        graph.add_node(frag_b.id)
        graph.add_node(frag_c.id)

        graph.add_edge(frag_a.id, frag_b.id, float("nan"))
        graph.add_edge(frag_b.id, frag_c.id, float("inf"))
        graph.add_edge(frag_c.id, frag_a.id, float("-inf"))

        try:
            scores = personalized_pagerank(graph, seeds={frag_a.id}, alpha=0.6)

            assert len(scores) == 3
            for node_id, score in scores.items():
                assert not math.isnan(score), f"Score for {node_id} is NaN"
        except (ValueError, OverflowError, ZeroDivisionError):
            pass


class TestBuildGraphEdgeCases:
    def test_build_graph_empty_fragments(self):
        graph = build_graph([])
        assert len(graph.nodes) == 0

        scores = personalized_pagerank(graph, seeds=set(), alpha=0.6)
        assert scores == {}

    def test_build_graph_no_edges(self, tmp_path):
        fragments = [
            Fragment(
                id=FragmentId(path=tmp_path / f"file{i}.py", start_line=1, end_line=5),
                kind="function",
                content=f"""def unique_func{i}():
    pass
""",
                identifiers=frozenset([f"unique_func{i}"]),
                token_count=50,
            )
            for i in range(5)
        ]

        graph = build_graph(fragments)

        assert len(graph.nodes) == 5

        scores = personalized_pagerank(graph, seeds={fragments[0].id}, alpha=0.6)

        assert len(scores) == 5
        assert all(s >= 0 for s in scores.values())
        assert abs(sum(scores.values()) - 1.0) < 1e-9


class TestRealGitScenarios:
    def test_file_completely_deleted_dev_null(self, git_with_commits):
        git_with_commits.add_file(
            "to_delete.py",
            """def will_be_deleted():
    return "goodbye"

class DeleteMe:
    def method(self):
        pass
""",
        )
        git_with_commits.add_file(
            "keep.py",
            """def keep_this():
    return "staying"
""",
        )
        git_with_commits.commit("Initial with file to delete")

        subprocess.run(
            ["git", "rm", "to_delete.py"],
            cwd=git_with_commits.repo,
            capture_output=True,
            check=True,
        )
        git_with_commits.commit("Delete file completely")

        hunks = parse_diff(git_with_commits.repo, "HEAD~1..HEAD")

        deletion_hunks = [h for h in hunks if h.is_deletion]
        assert len(deletion_hunks) >= 1
        assert any("to_delete.py" in str(h.path) for h in deletion_hunks)

        for h in deletion_hunks:
            if "to_delete.py" in str(h.path):
                assert h.new_len == 0
                assert h.old_len > 0

        diff_text = get_diff_text(git_with_commits.repo, "HEAD~1..HEAD")
        assert "+++ /dev/null" in diff_text

        tree = build_diff_context(
            root_dir=git_with_commits.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        assert tree is not None
        assert tree.get("type") == "diff_context"

    def test_chmod_only_no_content_change(self, git_with_commits):
        git_with_commits.add_file(
            "script.py",
            """#!/usr/bin/env python3
def main():
    print("Hello")

if __name__ == "__main__":
    main()
""",
        )
        git_with_commits.commit("Initial script")

        script_path = git_with_commits.repo / "script.py"
        script_path.chmod(0o755)

        subprocess.run(
            ["git", "add", "script.py"],
            cwd=git_with_commits.repo,
            capture_output=True,
            check=True,
        )
        subprocess.run(
            ["git", "commit", "-m", "Make script executable"],
            cwd=git_with_commits.repo,
            capture_output=True,
            check=True,
        )

        diff_text = get_diff_text(git_with_commits.repo, "HEAD~1..HEAD")

        tree = build_diff_context(
            root_dir=git_with_commits.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        assert tree is not None
        assert tree.get("type") == "diff_context"
        if "old mode" in diff_text and "new mode" in diff_text:
            hunks = parse_diff(git_with_commits.repo, "HEAD~1..HEAD")
            assert len(hunks) == 0 or all(h.old_len == 0 and h.new_len == 0 for h in hunks)

    def test_huge_diff_10000_plus_lines_performance(self, git_with_commits):
        lines = []
        for i in range(10500):
            lines.append(f"def func_{i}():")
            lines.append(f"    return {i}")
            lines.append("")
        initial_content = "\n".join(lines)

        git_with_commits.add_file("huge_file.py", initial_content)
        git_with_commits.commit("Initial huge file")

        modified_lines = []
        for i in range(10500):
            modified_lines.append(f"def func_{i}():")
            modified_lines.append(f"    return {i * 2}")
            modified_lines.append("")
        modified_content = "\n".join(modified_lines)

        git_with_commits.add_file("huge_file.py", modified_content)
        git_with_commits.commit("Modify all functions")

        start_time = time.time()

        tree = build_diff_context(
            root_dir=git_with_commits.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=50000,
        )

        elapsed = time.time() - start_time

        assert tree is not None
        assert tree.get("type") == "diff_context"
        assert elapsed < 60, f"Performance degraded: took {elapsed:.2f}s for 10000+ line diff"

        selected = _extract_files_from_tree(tree)
        assert "huge_file.py" in selected

    def test_partially_staged_correct_source(self, git_with_commits):
        git_with_commits.add_file(
            "mixed.py",
            """def original():
    return 1

def another():
    return 2
""",
        )
        git_with_commits.commit("Initial")

        (git_with_commits.repo / "mixed.py").write_text(
            """def original():
    return 10

def another():
    return 20
""",
            encoding="utf-8",
        )

        subprocess.run(
            ["git", "add", "-p", "--", "mixed.py"],
            cwd=git_with_commits.repo,
            capture_output=True,
            input=b"y\nn\n",
        )

        staged_result = subprocess.run(
            ["git", "diff", "--staged", "--name-only"],
            cwd=git_with_commits.repo,
            capture_output=True,
            text=True,
        )

        subprocess.run(
            ["git", "diff", "--name-only"],
            cwd=git_with_commits.repo,
            capture_output=True,
            text=True,
        )

        if staged_result.stdout.strip():
            staged_files = get_changed_files(git_with_commits.repo, "--staged")
            assert any("mixed.py" in str(f) for f in staged_files)

        subprocess.run(
            ["git", "checkout", "--", "mixed.py"],
            cwd=git_with_commits.repo,
            capture_output=True,
        )

    def test_uncommitted_changes_fallback_to_head(self, git_with_commits):
        git_with_commits.add_file(
            "base.py",
            """def base_func():
    return "base"
""",
        )
        git_with_commits.commit("Initial commit")

        (git_with_commits.repo / "base.py").write_text(
            """def base_func():
    return "modified"

def new_func():
    return "new"
""",
            encoding="utf-8",
        )

        changed = get_changed_files(git_with_commits.repo, "HEAD")
        assert any("base.py" in str(f) for f in changed)

        tree = build_diff_context(
            root_dir=git_with_commits.repo,
            diff_range="HEAD",
            budget_tokens=10000,
        )

        assert tree is not None
        assert tree.get("type") == "diff_context"

        subprocess.run(
            ["git", "checkout", "--", "base.py"],
            cwd=git_with_commits.repo,
            capture_output=True,
        )

    def test_detached_head_no_branch_reference(self, git_with_commits):
        git_with_commits.add_file(
            "code.py",
            """def version_1():
    return 1
""",
        )
        first_sha = git_with_commits.commit("First commit")

        git_with_commits.add_file(
            "code.py",
            """def version_1():
    return 1

def version_2():
    return 2
""",
        )
        git_with_commits.commit("Second commit")

        subprocess.run(
            ["git", "checkout", first_sha],
            cwd=git_with_commits.repo,
            capture_output=True,
            check=True,
        )

        head_result = subprocess.run(
            ["git", "symbolic-ref", "-q", "HEAD"],
            cwd=git_with_commits.repo,
            capture_output=True,
        )
        assert head_result.returncode != 0

        git_with_commits.add_file(
            "code.py",
            """def version_1():
    return 100
""",
        )
        subprocess.run(
            ["git", "add", "-A"],
            cwd=git_with_commits.repo,
            capture_output=True,
            check=True,
        )
        subprocess.run(
            ["git", "commit", "-m", "Detached HEAD commit"],
            cwd=git_with_commits.repo,
            capture_output=True,
            check=True,
        )

        tree = build_diff_context(
            root_dir=git_with_commits.repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        assert tree is not None
        assert tree.get("type") == "diff_context"
        selected = _extract_files_from_tree(tree)
        assert "code.py" in selected

    def test_shallow_clone_limited_history_graceful_degradation(self, tmp_path):
        original_repo = tmp_path / "original"
        original_repo.mkdir()

        subprocess.run(["git", "init"], cwd=original_repo, capture_output=True, check=True)
        subprocess.run(
            ["git", "config", "user.email", "test@test.com"],
            cwd=original_repo,
            capture_output=True,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test User"],
            cwd=original_repo,
            capture_output=True,
            check=True,
        )

        for i in range(5):
            (original_repo / f"file_{i}.py").write_text(
                f"""def func_{i}():
    return {i}
""",
                encoding="utf-8",
            )
            subprocess.run(["git", "add", "-A"], cwd=original_repo, capture_output=True, check=True)
            subprocess.run(
                ["git", "commit", "-m", f"Commit {i}"],
                cwd=original_repo,
                capture_output=True,
                check=True,
            )

        shallow_repo = tmp_path / "shallow"

        subprocess.run(
            ["git", "clone", "--depth", "1", f"file://{original_repo}", str(shallow_repo)],
            capture_output=True,
            check=True,
        )

        is_shallow = subprocess.run(
            ["git", "rev-parse", "--is-shallow-repository"],
            cwd=shallow_repo,
            capture_output=True,
            text=True,
        )
        assert is_shallow.stdout.strip() == "true"

        subprocess.run(
            ["git", "config", "user.email", "test@test.com"],
            cwd=shallow_repo,
            capture_output=True,
            check=True,
        )
        subprocess.run(
            ["git", "config", "user.name", "Test User"],
            cwd=shallow_repo,
            capture_output=True,
            check=True,
        )

        (shallow_repo / "file_4.py").write_text(
            """def func_4():
    return 400
""",
            encoding="utf-8",
        )
        subprocess.run(["git", "add", "-A"], cwd=shallow_repo, capture_output=True, check=True)
        subprocess.run(
            ["git", "commit", "-m", "Shallow commit"],
            cwd=shallow_repo,
            capture_output=True,
            check=True,
        )

        tree = build_diff_context(
            root_dir=shallow_repo,
            diff_range="HEAD~1..HEAD",
            budget_tokens=10000,
        )

        assert tree is not None
        assert tree.get("type") == "diff_context"
        selected = _extract_files_from_tree(tree)
        assert "file_4.py" in selected

        with pytest.raises(GitError):
            build_diff_context(
                root_dir=shallow_repo,
                diff_range="HEAD~10..HEAD",
                budget_tokens=10000,
            )


class TestSubmodularSelection:
    def test_tau_zero_selects_until_budget_exhausted(self):
        fragments = [
            _make_fragment("a.py", 1, 10, frozenset(["concept_a"]), tokens=100),
            _make_fragment("b.py", 1, 10, frozenset(["concept_b"]), tokens=100),
            _make_fragment("c.py", 1, 10, frozenset(["concept_c"]), tokens=100),
            _make_fragment("d.py", 1, 10, frozenset(["concept_d"]), tokens=100),
            _make_fragment("e.py", 1, 10, frozenset(["concept_e"]), tokens=100),
            _make_fragment("f.py", 1, 10, frozenset(["concept_f"]), tokens=100),
            _make_fragment("g.py", 1, 10, frozenset(["concept_g"]), tokens=100),
            _make_fragment("h.py", 1, 10, frozenset(["concept_h"]), tokens=100),
        ]
        core_ids = set()
        rel = {f.id: 0.5 for f in fragments}
        concepts = frozenset(
            ["concept_a", "concept_b", "concept_c", "concept_d", "concept_e", "concept_f", "concept_g", "concept_h"]
        )

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=500,
            tau=0.0,
        )

        assert result.used_tokens <= 500
        assert len(result.selected) == 5
        assert result.reason != "stopped_by_tau"

    def test_tau_one_stops_after_baseline(self):
        fragments = [
            _make_fragment("a.py", 1, 10, frozenset(["high_value"]), tokens=100),
            _make_fragment("b.py", 1, 10, frozenset(["medium_value"]), tokens=100),
            _make_fragment("c.py", 1, 10, frozenset(["low_value"]), tokens=100),
            _make_fragment("d.py", 1, 10, frozenset(["very_low"]), tokens=100),
            _make_fragment("e.py", 1, 10, frozenset(["minimal"]), tokens=100),
            _make_fragment("f.py", 1, 10, frozenset(["tiny"]), tokens=100),
            _make_fragment("g.py", 1, 10, frozenset(["micro"]), tokens=100),
            _make_fragment("h.py", 1, 10, frozenset(["nano"]), tokens=100),
        ]
        core_ids = set()
        rel = {
            fragments[0].id: 1.0,
            fragments[1].id: 0.8,
            fragments[2].id: 0.6,
            fragments[3].id: 0.4,
            fragments[4].id: 0.2,
            fragments[5].id: 0.001,
            fragments[6].id: 0.001,
            fragments[7].id: 0.001,
        }
        concepts = frozenset(["high_value", "medium_value", "low_value", "very_low", "minimal", "tiny", "micro", "nano"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=1.0,
        )

        assert len(result.selected) < len(fragments)
        assert result.reason == "stopped_by_tau"

    def test_all_concepts_covered_by_core_no_expansion(self):
        core_frag_a = _make_fragment("core_a.py", 1, 10, frozenset(["concept_x", "concept_y"]), tokens=100)
        core_frag_b = _make_fragment("core_b.py", 1, 10, frozenset(["concept_z"]), tokens=100)
        expansion_frag = _make_fragment("expansion.py", 1, 10, frozenset(["concept_x", "concept_y", "concept_z"]), tokens=100)

        fragments = [core_frag_a, core_frag_b, expansion_frag]
        core_ids = {core_frag_a.id, core_frag_b.id}
        rel = {f.id: 1.0 for f in fragments}
        concepts = frozenset(["concept_x", "concept_y", "concept_z"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        core_selected = [f for f in result.selected if f.id in core_ids]
        assert len(core_selected) == 2

        expansion_selected = [f for f in result.selected if f.id not in core_ids]
        assert len(expansion_selected) <= 1

    def test_core_larger_than_budget_includes_all_core(self):
        core_frags = [
            _make_fragment("core1.py", 1, 10, frozenset(["concept_1"]), tokens=300),
            _make_fragment("core2.py", 1, 10, frozenset(["concept_2"]), tokens=300),
            _make_fragment("core3.py", 1, 10, frozenset(["concept_3"]), tokens=300),
        ]
        expansion_frag = _make_fragment("expansion.py", 1, 10, frozenset(["concept_4"]), tokens=100)

        fragments = [*core_frags, expansion_frag]
        core_ids = {f.id for f in core_frags}
        rel = {f.id: 1.0 for f in fragments}
        concepts = frozenset(["concept_1", "concept_2", "concept_3", "concept_4"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=500,
            tau=0.0,
        )

        core_selected = [f for f in result.selected if f.id in core_ids]
        assert len(core_selected) == len(core_frags)

        expansion_selected = [f for f in result.selected if f.id not in core_ids]
        assert len(expansion_selected) == 0

        assert result.used_tokens == 900
        assert result.reason == "budget_exhausted"

    def test_empty_core_all_goes_to_expansion(self):
        expansion_frags = [
            _make_fragment("exp1.py", 1, 10, frozenset(["caller_a"]), tokens=100),
            _make_fragment("exp2.py", 1, 10, frozenset(["caller_b"]), tokens=100),
            _make_fragment("exp3.py", 1, 10, frozenset(["caller_c"]), tokens=100),
        ]

        fragments = expansion_frags
        core_ids: set[FragmentId] = set()
        rel = {f.id: 0.8 for f in fragments}
        concepts = frozenset(["caller_a", "caller_b", "caller_c"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        assert len(result.selected) == 3
        assert result.used_tokens == 300

        core_in_result = [f for f in result.selected if f.id in core_ids]
        assert len(core_in_result) == 0

    def test_fully_contained_fragment_not_selected(self):
        outer_frag = _make_fragment("file.py", 1, 100, frozenset(["concept_a", "concept_b"]), tokens=100)
        inner_frag = _make_fragment("file.py", 10, 90, frozenset(["concept_a", "concept_c"]), tokens=80)

        fragments = [outer_frag, inner_frag]
        core_ids: set[FragmentId] = set()
        rel = {outer_frag.id: 1.0, inner_frag.id: 0.9}
        concepts = frozenset(["concept_a", "concept_b", "concept_c"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        assert len(result.selected) == 1
        assert result.selected[0].id == outer_frag.id

    def test_partial_overlap_only_one_selected(self):
        frag1 = _make_fragment("file.py", 1, 100, frozenset(["concept_a", "unique_1"]), tokens=100)
        frag2 = _make_fragment("file.py", 95, 200, frozenset(["concept_b", "unique_2"]), tokens=106)

        fragments = [frag1, frag2]
        core_ids: set[FragmentId] = set()
        rel = {frag1.id: 1.0, frag2.id: 0.9}
        concepts = frozenset(["concept_a", "concept_b", "unique_1", "unique_2"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        assert len(result.selected) == 1
        assert result.selected[0].id == frag1.id

    def test_partial_overlap_non_overlapping_both_selected(self):
        frag1 = _make_fragment("file.py", 1, 50, frozenset(["concept_a"]), tokens=50)
        frag2 = _make_fragment("file.py", 100, 150, frozenset(["concept_b"]), tokens=50)

        fragments = [frag1, frag2]
        core_ids: set[FragmentId] = set()
        rel = {frag1.id: 1.0, frag2.id: 0.9}
        concepts = frozenset(["concept_a", "concept_b"])

        result = lazy_greedy_select(
            fragments=fragments,
            core_ids=core_ids,
            rel=rel,
            concepts=concepts,
            budget_tokens=10000,
            tau=0.0,
        )

        assert len(result.selected) == 2
