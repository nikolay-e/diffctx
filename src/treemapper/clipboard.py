from __future__ import annotations

import os
import platform
import shutil
import subprocess


class ClipboardError(Exception):
    pass


def _detect_darwin_clipboard() -> list[str] | None:
    return ["pbcopy"] if shutil.which("pbcopy") else None


def _detect_windows_clipboard() -> list[str] | None:
    return ["clip"] if shutil.which("clip") else None


def _detect_linux_clipboard() -> list[str] | None:
    if os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-copy"):
        return ["wl-copy", "--type", "text/plain"]
    if os.environ.get("DISPLAY"):
        if shutil.which("xclip"):
            return ["xclip", "-selection", "clipboard"]
        if shutil.which("xsel"):
            return ["xsel", "--clipboard", "--input"]
    return None


_CLIPBOARD_DETECTORS = {
    "Darwin": _detect_darwin_clipboard,
    "Windows": _detect_windows_clipboard,
    "Linux": _detect_linux_clipboard,
    "FreeBSD": _detect_linux_clipboard,
}


def detect_clipboard_command() -> list[str] | None:
    detector = _CLIPBOARD_DETECTORS.get(platform.system())
    return detector() if detector else None


def copy_to_clipboard(text: str) -> int:
    cmd = detect_clipboard_command()
    if cmd is None:
        raise ClipboardError("No clipboard tool found")

    # Windows clip.exe requires UTF-16LE without BOM for proper Unicode support
    encoding = "utf-16le" if platform.system() == "Windows" else "utf-8"
    encoded = text.encode(encoding)

    try:
        subprocess.run(
            cmd,
            input=encoded,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=5,
            check=True,
        )
    except subprocess.TimeoutExpired as e:
        raise ClipboardError("Clipboard operation timed out") from e
    except subprocess.CalledProcessError as e:
        stderr_msg = e.stderr.decode(errors="replace").strip() if e.stderr else ""
        raise ClipboardError(stderr_msg or f"Command failed with code {e.returncode}") from e
    except OSError as e:
        raise ClipboardError(f"Failed to execute clipboard command: {e}") from e

    return len(encoded)


def clipboard_available() -> bool:
    return detect_clipboard_command() is not None
