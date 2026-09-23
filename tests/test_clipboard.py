import shutil
import subprocess
import sys

import pytest

from diffctx.clipboard import ClipboardError, clipboard_available, copy_to_clipboard, detect_clipboard_command
from diffctx.tokens import _format_size
from tests.conftest import run_diffctx_subprocess


@pytest.fixture
def temp_project(tmp_path):
    project = tmp_path / "test_project"
    project.mkdir()
    (project / "file.txt").write_text("hello", encoding="utf-8")
    (project / "src").mkdir()
    (project / "src" / "main.py").write_text("def main():\n    print('hello')\n", encoding="utf-8")
    return project


class TestDetectClipboardCommand:
    def test_returns_list_or_none(self):
        result = detect_clipboard_command()
        assert result is None or isinstance(result, list)

    def test_result_contains_executable(self):
        result = detect_clipboard_command()
        if result is not None:
            assert len(result) >= 1
            assert isinstance(result[0], str)

    @pytest.mark.skipif(sys.platform != "darwin", reason="macOS only")
    def test_macos_detects_pbcopy(self):
        result = detect_clipboard_command()
        assert result is not None
        assert result[0] == "pbcopy"

    @pytest.mark.skipif(sys.platform != "win32", reason="Windows only")
    def test_windows_detects_clip(self):
        result = detect_clipboard_command()
        assert result is not None
        assert result[0] == "clip"


class TestCopyToClipboard:
    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available")
    def test_copies_to_clipboard(self):
        copy_to_clipboard("integration test")

    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available")
    def test_empty_string(self):
        copy_to_clipboard("")

    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available")
    def test_unicode_content(self):
        copy_to_clipboard("\u65e5\u672c\u8a9e\u30c6\u30b9\u30c8")

    def test_raises_when_no_clipboard_command(self, monkeypatch):
        monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
        with pytest.raises(ClipboardError, match="No clipboard tool found"):
            copy_to_clipboard("test")


class TestClipboardAvailable:
    def test_returns_bool(self):
        assert isinstance(clipboard_available(), bool)

    def test_consistent_with_detect(self):
        assert clipboard_available() == (detect_clipboard_command() is not None)


class TestFormatSize:
    def test_bytes_under_1kb(self):
        assert _format_size(0) == "0 B"
        assert _format_size(512) == "512 B"
        assert _format_size(1023) == "1023 B"

    def test_kilobytes(self):
        assert _format_size(1024) == "1.0 KB"
        assert _format_size(2048) == "2.0 KB"
        assert _format_size(1536) == "1.5 KB"
        assert _format_size(1024 * 1024 - 1) == "1024.0 KB"

    def test_megabytes(self):
        assert _format_size(1024 * 1024) == "1.0 MB"
        assert _format_size(2 * 1024 * 1024) == "2.0 MB"
        assert _format_size(1536 * 1024) == "1.5 MB"
        assert _format_size(100 * 1024 * 1024) == "100.0 MB"
        assert _format_size(1024 * 1024 * 1024) == "1024.0 MB"


class TestCliCopyFlags:
    def test_help_shows_copy_options(self, temp_project):
        result = run_diffctx_subprocess(["--help"], cwd=temp_project)
        assert "-c" in result.stdout
        assert "--copy" in result.stdout

    def test_copy_flag_accepted(self, temp_project):
        result = run_diffctx_subprocess([".", "-c"], cwd=temp_project)
        assert result.returncode == 0

    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available (no display or clipboard tool)")
    def test_copy_suppresses_stdout(self, temp_project):
        result = run_diffctx_subprocess([".", "-c"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout == ""

    def test_copy_with_file_still_writes_file(self, temp_project):
        result = run_diffctx_subprocess([".", "-c", "-o", "out.yaml"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout == ""
        assert (temp_project / "out.yaml").exists()


def _read_clipboard():
    if sys.platform == "darwin":
        return subprocess.run(["pbpaste"], capture_output=True, text=True, check=True).stdout
    if sys.platform == "win32":
        script = "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw"
        try:
            return subprocess.run(
                ["powershell", "-NoProfile", "-Command", script],
                capture_output=True,
                encoding="utf-8",
                check=True,
                timeout=20,
            ).stdout
        except subprocess.TimeoutExpired:
            # Observed on the windows-11-arm runner: Get-Clipboard never answers
            # there although clip.exe accepted the copy.
            pytest.skip("PowerShell Get-Clipboard did not answer on this machine")
    for reader in (["wl-paste", "--no-newline"], ["xclip", "-selection", "clipboard", "-o"]):
        if shutil.which(reader[0]):
            return subprocess.run(reader, capture_output=True, text=True, check=True).stdout
    pytest.skip("no clipboard reader on this machine")


class TestCopyRoundTrip:
    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available (no display or clipboard tool)")
    def test_copied_text_equals_the_stdout_artifact(self, temp_project):
        expected = run_diffctx_subprocess(["src", "-f", "txt", "-o", "-"], cwd=temp_project).stdout
        for _attempt in range(3):
            copied = run_diffctx_subprocess(["src", "-c", "-f", "txt"], cwd=temp_project)
            assert copied.returncode == 0, copied.stderr
            assert copied.stdout == ""
            assert "Copied to clipboard" in copied.stderr
            pasted = _read_clipboard()
            if pasted.replace("\r\n", "\n").rstrip("\n") == expected.rstrip("\n"):
                return
        # Other workers of this suite write the clipboard too; three
        # consecutive mismatches are the tool, not a race.
        assert pasted.replace("\r\n", "\n").rstrip("\n") == expected.rstrip("\n")


class TestClipboardWarnings:
    def test_no_clipboard_prints_warning(self, temp_project, capsys, monkeypatch):
        monkeypatch.setattr("diffctx.clipboard.detect_clipboard_command", lambda: None)
        monkeypatch.chdir(temp_project)
        monkeypatch.setattr("sys.argv", ["diffctx", ".", "-c"])
        from diffctx.cli import main

        main()
        captured = capsys.readouterr()
        assert "clipboard unavailable" in captured.err
        assert "writing to stdout instead" in captured.err
        assert captured.out.strip() != ""


class TestForceStdout:
    def test_explicit_stdout_with_copy_flag(self, temp_project):
        result = run_diffctx_subprocess([".", "-c", "-o", "-"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout != ""
        assert "file.txt" in result.stdout

    def test_explicit_stdout_without_copy(self, temp_project):
        result = run_diffctx_subprocess([".", "-o", "-"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout != ""
        assert "file.txt" in result.stdout

    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available (no display or clipboard tool)")
    def test_copy_without_explicit_stdout_suppresses_output(self, temp_project):
        result = run_diffctx_subprocess([".", "-c"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout == ""
