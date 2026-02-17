# tests/test_tokens.py
import sys
from io import StringIO

from treemapper.tokens import TokenCountResult, count_tokens, print_token_summary


class TestCountTokens:
    def test_basic_text_with_tiktoken(self):
        result = count_tokens("Hello, world!")
        assert result.count > 0
        assert isinstance(result.count, int)

    def test_empty_string(self):
        result = count_tokens("")
        assert result.count == 0
        assert result.is_exact or result.encoding == "approximation"

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
        result = count_tokens("test", encoding="o200k_base")
        assert result.encoding in ("o200k_base", "approximation")

    def test_cl100k_base_encoding(self):
        result = count_tokens("test", encoding="cl100k_base")
        assert result.encoding in ("cl100k_base", "approximation")

    def test_o200k_harmony_encoding(self):
        result = count_tokens("test", encoding="o200k_harmony")
        assert result.encoding in ("o200k_harmony", "approximation")

    def test_invalid_encoding_falls_back_to_approximation(self):
        result = count_tokens("test text here", encoding="nonexistent_encoding")
        assert result.encoding == "approximation"
        assert result.is_exact is False
        assert result.count == len("test text here") // 4

    def test_approximation_formula(self):
        text = "a" * 100
        result = count_tokens(text, encoding="nonexistent_encoding")
        assert result.count == 25
        assert result.is_exact is False

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
            # When tiktoken is available, exact count has no tilde
            # When tiktoken is not available, approximation uses tilde
        finally:
            sys.stderr = old_stderr

    def test_summary_format_approximate(self):
        old_stderr = sys.stderr
        sys.stderr = StringIO()
        try:
            print_token_summary("hello world", encoding="nonexistent")
            output = sys.stderr.getvalue()
            assert "~" in output
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


class TestEncoderCaching:
    def test_encoder_cached_on_repeated_calls(self):
        count_tokens("test1", encoding="o200k_base")
        count_tokens("test2", encoding="o200k_base")
        count_tokens("test3", encoding="o200k_base")

    def test_different_encodings_cached_separately(self):
        r1 = count_tokens("test", encoding="o200k_base")
        r2 = count_tokens("test", encoding="cl100k_base")
        assert r1.encoding != r2.encoding or r1.encoding == "approximation"


class TestChunkedCounting:
    def test_chunked_counting_for_large_text(self):
        from treemapper.tokens import CHUNK_THRESHOLD

        large_text = "word " * (CHUNK_THRESHOLD // 5 + 1000)
        result = count_tokens(large_text)
        assert result.count > 0
        # Chunked counting is not exact due to BPE context sensitivity
        # is_exact=False with real encoding, or approximation fallback
        assert result.is_exact is False

    def test_chunked_count_close_to_exact(self, monkeypatch):
        import treemapper.tokens as tokens_module
        from treemapper.tokens import _get_encoder

        encoder = _get_encoder("o200k_base")
        if encoder is None:
            return

        text = "word " * 5_000
        exact_count = len(encoder.encode(text))

        monkeypatch.setattr(tokens_module, "CHUNK_THRESHOLD", 1_000)
        chunked_result = count_tokens(text)

        assert abs(chunked_result.count - exact_count) / exact_count < 0.05

    def test_small_text_not_chunked(self):
        small_text = "hello world"
        result = count_tokens(small_text)
        assert result.count > 0


class TestSampledCounting:
    def test_sampling_threshold_is_reasonable(self):
        from treemapper.tokens import SAMPLE_CHAR_THRESHOLD

        assert SAMPLE_CHAR_THRESHOLD >= 1_000_000

    def test_very_large_text_uses_sampling(self, monkeypatch):
        import treemapper.tokens as tokens_module
        from treemapper.tokens import _count_tokens_sampled, _get_encoder

        encoder = _get_encoder("o200k_base")
        if encoder is None:
            return

        monkeypatch.setattr(tokens_module, "SAMPLE_CHAR_THRESHOLD", 10_000)
        large_text = "x" * 15_000
        result = _count_tokens_sampled(large_text, len(large_text), encoder, "o200k_base")
        assert result.is_exact is False
        assert result.count > 0

    def test_sampled_result_is_approximate(self, monkeypatch):
        import treemapper.tokens as tokens_module

        monkeypatch.setattr(tokens_module, "SAMPLE_CHAR_THRESHOLD", 10_000)
        monkeypatch.setattr(tokens_module, "CHUNK_THRESHOLD", 1_000)
        text = "word " * 5_000
        result = count_tokens(text)
        if result.encoding != "approximation":
            assert result.is_exact is False
