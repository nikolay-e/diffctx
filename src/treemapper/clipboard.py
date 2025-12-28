from __future__ import annotations

import os
import platform
import shutil
import subprocess


class ClipboardError(Exception):
    pass


def detect_clipboard_command() -> list[str] | None:
    system = platform.system()
    if system == "Darwin":
        # pbcopy uses locale env vars for encoding; UTF-8 recommended
        if shutil.which("pbcopy"):
            return ["pbcopy"]
        return None
    if system == "Windows":
        if shutil.which("clip"):
            return ["clip"]
        return None
    if system in ("Linux", "FreeBSD"):
        if os.environ.get("WAYLAND_DISPLAY") and shutil.which("wl-copy"):
            # Force text/plain to avoid xdg-mime inference issues in minimal/headless setups
            return ["wl-copy", "--type", "text/plain"]
        if os.environ.get("DISPLAY"):
            if shutil.which("xclip"):
                return ["xclip", "-selection", "clipboard"]
            if shutil.which("xsel"):
                return ["xsel", "--clipboard", "--input"]
        return None
    return None


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
