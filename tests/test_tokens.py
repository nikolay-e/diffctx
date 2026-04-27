# tests/test_tokens.py
import sys
from io import StringIO

from treemapper.tokens import TokenCountResult, count_tokens, print_token_summary


class TestCountTokens:
    def test_basic_text_with_tiktoken(self):
        result = count_tokens("Hello, world!")
        assert result.count > 0
        assert isinstance(result.count, int)
        assert result.is_exact is True
        assert result.encoding == "o200k_base"

    def test_empty_string(self):
        result = count_tokens("")
        assert result.count == 0
        assert result.is_exact is True

    def test_unicode_text(self):
        result = count_tokens("Привет мир! 你好世界 🎉")
        assert result.count > 0

    def test_code_tokenization(self):
        code = """def hello():
    print("Hello, World!")
    return 42
"""
        result = count_tokens(code)
        assert result.count > 0

    def test_long_text(self):
        long_text = "word " * 1000
        result = count_tokens(long_text)
        assert result.count > 0

    def test_o200k_base_encoding(self):
        # The encoding kwarg is accepted for API stability; the Rust backend
        # always uses o200k_base.
        result = count_tokens("test", encoding="o200k_base")
        assert result.encoding == "o200k_base"
        assert result.is_exact is True

    def test_result_dataclass_fields(self):
        result = count_tokens("test")
        assert hasattr(result, "count")
        assert hasattr(result, "is_exact")
        assert hasattr(result, "encoding")

    def test_whitespace_only(self):
        result = count_tokens("   \n\t\r\n   ")
        assert result.count >= 0

    def test_special_characters(self):
        result = count_tokens("!@#$%^&*()_+-=[]{}|;':\",./<>?")
        assert result.count > 0

    def test_newlines_tabs(self):
        result = count_tokens("line1\nline2\tline3\r\nline4")
        assert result.count > 0

    def test_large_text_exact(self):
        large_text = "word " * 500_000
        result = count_tokens(large_text)
        assert result.count > 0
        assert result.is_exact is True


class TestPrintTokenSummary:
    def test_prints_to_stderr(self):
        old_stderr = sys.stderr
        sys.stderr = StringIO()
        try:
            print_token_summary("test text")
            output = sys.stderr.getvalue()
            assert "tokens" in output
        finally:
            sys.stderr = old_stderr

    def test_summary_format_exact(self):
        old_stderr = sys.stderr
        sys.stderr = StringIO()
        try:
            print_token_summary("hello world", encoding="o200k_base")
            output = sys.stderr.getvalue()
            assert "tokens" in output
        finally:
            sys.stderr = old_stderr


class TestTokenCountResult:
    def test_dataclass_creation(self):
        result = TokenCountResult(count=100, is_exact=True, encoding="o200k_base")
        assert result.count == 100
        assert result.is_exact is True
        assert result.encoding == "o200k_base"

    def test_dataclass_equality(self):
        r1 = TokenCountResult(count=10, is_exact=True, encoding="test")
        r2 = TokenCountResult(count=10, is_exact=True, encoding="test")
        assert r1 == r2

    def test_dataclass_inequality(self):
        r1 = TokenCountResult(count=10, is_exact=True, encoding="test")
        r2 = TokenCountResult(count=20, is_exact=True, encoding="test")
        assert r1 != r2
