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

_INSTALL_HINTS = {
    "Darwin": "pbcopy should be available by default on macOS",
    "Windows": "clip.exe should be available by default on Windows",
    "Linux": "Install wl-copy (Wayland) or xclip/xsel (X11): sudo apt install wl-clipboard or sudo apt install xclip",
    "FreeBSD": "Install xclip or xsel: pkg install xclip",
}


def detect_clipboard_command() -> list[str] | None:
    detector = _CLIPBOARD_DETECTORS.get(platform.system())
    return detector() if detector else None


def copy_to_clipboard(text: str) -> None:
    cmd = detect_clipboard_command()
    if cmd is None:
        system = platform.system()
        hint = _INSTALL_HINTS.get(system, f"No clipboard support for {system}")
        raise ClipboardError(f"No clipboard tool found. {hint}")

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


def clipboard_available() -> bool:
    return detect_clipboard_command() is not None
