import os
import subprocess
from unittest.mock import patch

import pytest

from tests.conftest import run_treemapper_subprocess
from treemapper.clipboard import ClipboardError, clipboard_available, copy_to_clipboard, detect_clipboard_command
from treemapper.tokens import _format_size


@pytest.fixture
def temp_project(tmp_path):
    project = tmp_path / "test_project"
    project.mkdir()
    (project / "file.txt").write_text("hello", encoding="utf-8")
    return project


class TestDetectClipboardCommand:
    @patch("platform.system", return_value="Darwin")
    @patch("shutil.which", return_value="/usr/bin/pbcopy")
    def test_macos_uses_pbcopy(self, mock_which, mock_system):
        assert detect_clipboard_command() == ["pbcopy"]

    @patch("platform.system", return_value="Darwin")
    @patch("shutil.which", return_value=None)
    def test_macos_no_pbcopy_returns_none(self, mock_which, mock_system):
        assert detect_clipboard_command() is None

    @patch("platform.system", return_value="Windows")
    @patch("shutil.which", return_value="C:\\Windows\\System32\\clip.exe")
    def test_windows_uses_clip(self, mock_which, mock_system):
        assert detect_clipboard_command() == ["clip"]

    @patch("platform.system", return_value="Windows")
    @patch("shutil.which", return_value=None)
    def test_windows_no_clip_returns_none(self, mock_which, mock_system):
        assert detect_clipboard_command() is None

    @patch("platform.system", return_value="Linux")
    @patch.dict(os.environ, {"WAYLAND_DISPLAY": "wayland-0"}, clear=False)
    @patch("shutil.which", return_value="/usr/bin/wl-copy")
    def test_linux_wayland_uses_wl_copy(self, mock_which, mock_system):
        assert detect_clipboard_command() == ["wl-copy", "--type", "text/plain"]

    @patch("platform.system", return_value="Linux")
    @patch.dict(os.environ, {"DISPLAY": ":0", "WAYLAND_DISPLAY": ""}, clear=False)
    @patch("shutil.which", side_effect=lambda x: "/usr/bin/xclip" if x == "xclip" else None)
    def test_linux_x11_uses_xclip(self, mock_which, mock_system):
        assert detect_clipboard_command() == ["xclip", "-selection", "clipboard"]

    @patch("platform.system", return_value="Linux")
    @patch.dict(os.environ, {"DISPLAY": ":0", "WAYLAND_DISPLAY": ""}, clear=False)
    @patch("shutil.which", side_effect=lambda x: "/usr/bin/xsel" if x == "xsel" else None)
    def test_linux_x11_uses_xsel_fallback(self, mock_which, mock_system):
        assert detect_clipboard_command() == ["xsel", "--clipboard", "--input"]

    @patch("platform.system", return_value="Linux")
    @patch.dict(os.environ, {"DISPLAY": "", "WAYLAND_DISPLAY": ""}, clear=False)
    def test_linux_no_display_returns_none(self, mock_system):
        assert detect_clipboard_command() is None

    @patch("platform.system", return_value="UnknownOS")
    def test_unsupported_os_returns_none(self, mock_system):
        assert detect_clipboard_command() is None


class TestCopyToClipboard:
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=None)
    def test_raises_error_when_no_clipboard(self, mock_detect):
        with pytest.raises(ClipboardError, match="No clipboard tool found"):
            copy_to_clipboard("test")

    @patch("platform.system", return_value="Darwin")
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["pbcopy"])
    @patch("subprocess.run")
    def test_uses_utf8_on_non_windows(self, mock_run, mock_detect, mock_system):
        copy_to_clipboard("test")
        mock_run.assert_called_once()
        assert mock_run.call_args.kwargs["input"] == b"test"

    @patch("platform.system", return_value="Windows")
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["clip"])
    @patch("subprocess.run")
    def test_uses_utf16le_on_windows(self, mock_run, mock_detect, mock_system):
        copy_to_clipboard("test")
        mock_run.assert_called_once()
        assert mock_run.call_args.kwargs["input"] == "test".encode("utf-16le")

    @patch("platform.system", return_value="Darwin")
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["pbcopy"])
    @patch("subprocess.run")
    def test_returns_byte_count(self, mock_run, mock_detect, mock_system):
        result = copy_to_clipboard("hello")
        assert result == 5

    @patch("platform.system", return_value="Windows")
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["clip"])
    @patch("subprocess.run")
    def test_returns_utf16_byte_count_on_windows(self, mock_run, mock_detect, mock_system):
        result = copy_to_clipboard("hello")
        assert result == 10  # UTF-16LE doubles the byte count

    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["pbcopy"])
    @patch("subprocess.run")
    def test_raises_on_timeout(self, mock_run, mock_detect):
        mock_run.side_effect = subprocess.TimeoutExpired("pbcopy", 5)
        with pytest.raises(ClipboardError, match="timed out"):
            copy_to_clipboard("test")

    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["pbcopy"])
    @patch("subprocess.run")
    def test_raises_on_command_failure(self, mock_run, mock_detect):
        mock_run.side_effect = subprocess.CalledProcessError(1, "pbcopy", stderr=b"error")
        with pytest.raises(ClipboardError, match="error"):
            copy_to_clipboard("test")

    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["pbcopy"])
    @patch("subprocess.run")
    def test_raises_on_oserror(self, mock_run, mock_detect):
        mock_run.side_effect = OSError("Command not found")
        with pytest.raises(ClipboardError, match="Failed to execute"):
            copy_to_clipboard("test")


class TestClipboardAvailable:
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=["pbcopy"])
    def test_returns_true_when_command_found(self, mock_detect):
        assert clipboard_available() is True

    @patch("treemapper.clipboard.detect_clipboard_command", return_value=None)
    def test_returns_false_when_no_command(self, mock_detect):
        assert clipboard_available() is False


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
        result = run_treemapper_subprocess(["--help"], cwd=temp_project)
        assert "-c" in result.stdout
        assert "--copy" in result.stdout

    def test_copy_flag_accepted(self, temp_project):
        result = run_treemapper_subprocess([".", "-c"], cwd=temp_project)
        assert result.returncode == 0

    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available (no display or clipboard tool)")
    def test_copy_suppresses_stdout(self, temp_project):
        result = run_treemapper_subprocess([".", "-c"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout == ""

    def test_copy_with_file_still_writes_file(self, temp_project):
        result = run_treemapper_subprocess([".", "-c", "-o", "out.yaml"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout == ""
        assert (temp_project / "out.yaml").exists()


class TestClipboardWarnings:
    @patch("treemapper.clipboard.detect_clipboard_command", return_value=None)
    def test_no_clipboard_prints_warning(self, mock_detect, temp_project, capsys, monkeypatch):
        monkeypatch.chdir(temp_project)
        monkeypatch.setattr("sys.argv", ["treemapper", ".", "-c"])
        from treemapper.treemapper import main

        main()
        captured = capsys.readouterr()
        assert "Clipboard unavailable" in captured.err
        assert "No clipboard tool found" in captured.err
        # When clipboard fails, output should go to stdout as fallback
        assert captured.out.strip() != ""


class TestForceStdout:
    def test_explicit_stdout_with_copy_flag(self, temp_project):
        """Test that -o - forces stdout output even when --copy succeeds."""
        result = run_treemapper_subprocess([".", "-c", "-o", "-"], cwd=temp_project)
        assert result.returncode == 0
        # With -o -, output should always go to stdout (force_stdout=True)
        assert result.stdout != ""
        assert "file.txt" in result.stdout

    def test_explicit_stdout_without_copy(self, temp_project):
        """Test that -o - outputs to stdout normally."""
        result = run_treemapper_subprocess([".", "-o", "-"], cwd=temp_project)
        assert result.returncode == 0
        assert result.stdout != ""
        assert "file.txt" in result.stdout

    @pytest.mark.skipif(not clipboard_available(), reason="Clipboard not available (no display or clipboard tool)")
    def test_copy_without_explicit_stdout_suppresses_output(self, temp_project):
        """Test that -c without -o - suppresses stdout when clipboard succeeds."""
        result = run_treemapper_subprocess([".", "-c"], cwd=temp_project)
        assert result.returncode == 0
        # Without -o -, stdout should be suppressed when clipboard succeeds
        assert result.stdout == ""
