"""Long-lived helper that runs inside an isolated venv with `aider-chat`
installed, talks JSON over stdin/stdout to the parent benchmark process.

The parent (`benchmarks.baselines.aider_baseline`) launches this with
`uv tool run --from aider-chat==0.86.2 python <path-to-this-file>` so
aider's heavy dep tree (litellm, prompt-toolkit, fastapi, etc.) never
contaminates the main treemapper venv.

Protocol (NDJSON, one request → one response):

    request:  {"repo_root": str, "chat_files": [str], "other_files": [str],
               "mentioned_fnames": [str], "mentioned_idents": [str],
               "map_tokens": int}
    response: {"ok": bool, "files": [str], "map_text": str, "error": str|null,
               "elapsed": float}

Files are absolute paths. The parent strips them back to repo-relative.
"""

from __future__ import annotations

import json
import sys
import tempfile
import time
import traceback


class _TiktokenModel:
    """Duck-typed `main_model` substitute for `RepoMap.token_count`.

    Avoids pulling in `litellm` by short-circuiting Aider's tokenizer
    lookup. Uses the same o200k BPE the rest of the benchmark uses, so
    `map_tokens` lines up with the diffctx token budget.
    """

    def __init__(self) -> None:
        import tiktoken

        self._enc = tiktoken.get_encoding("o200k_base")

    def token_count(self, text: str) -> int:
        if not text:
            return 0
        # Aider's reference impl samples every 10th line for long text;
        # we just count exactly — slightly slower, but reproducible.
        return len(self._enc.encode(text, disallowed_special=()))


class _SilentIO:
    """Duck-typed `io` substitute for RepoMap. No prompts, no console."""

    def read_text(self, fname, silent=False):
        try:
            with open(fname, encoding="utf-8", errors="replace") as f:
                return f.read()
        except Exception:
            return None

    def tool_output(self, *args, **kwargs):
        pass

    def tool_warning(self, *args, **kwargs):
        pass

    def tool_error(self, *args, **kwargs):
        pass


_CODE_EXT_RE = __import__("re").compile(
    r"\.(py|pyi|js|jsx|ts|tsx|java|kt|go|rs|c|cc|cpp|cxx|h|hpp|hxx|"
    r"rb|cs|php|swift|m|mm|sh|bash|zsh|yaml|yml|json|toml|ini|md|"
    r"html|htm|css|scss|sass|less|sql|proto|graphql|gql|lua|r|jl|"
    r"ex|exs|erl|elm|clj|cljs|hs|ml|mli|fs|fsi|dart|vue|svelte)$",
    __import__("re").IGNORECASE,
)


def _parse_files_from_map(map_text: str) -> list[str]:
    """RepoMap output is `<rel_path>:\\n<grep_ast tree lines indented>` per file.

    File-header lines are unindented, end with `:`, and contain either a
    path separator or a recognized code extension. Reject lines that look
    like Python `class Foo:` / `def bar():` headers.
    """
    if not map_text:
        return []
    out: list[str] = []
    for line in map_text.splitlines():
        if not line or line[0] in " \t|#":
            continue
        s = line.rstrip()
        if not s.endswith(":"):
            continue
        cand = s[:-1].strip()
        if not cand or " " in cand or "(" in cand or "<" in cand or "{" in cand:
            continue
        if "/" in cand or _CODE_EXT_RE.search(cand):
            out.append(cand)
    return out


def _handle_request(req: dict) -> dict:
    from aider.repomap import RepoMap

    t0 = time.perf_counter()
    cache_dir = tempfile.mkdtemp(prefix="aider-bench-")
    try:
        rm = RepoMap(
            map_tokens=int(req["map_tokens"]),
            root=req["repo_root"],
            main_model=_TiktokenModel(),
            io=_SilentIO(),
            verbose=False,
        )
        # Override cache to per-instance temp dir so repeated runs on the
        # same repo with different patches don't poison each other.
        try:
            rm.TAGS_CACHE_DIR = cache_dir
            rm.load_tags_cache()
        except Exception:
            pass

        map_text = (
            rm.get_repo_map(
                chat_files=req.get("chat_files", []),
                other_files=req["other_files"],
                mentioned_fnames=set(req.get("mentioned_fnames", [])),
                mentioned_idents=set(req.get("mentioned_idents", [])),
            )
            or ""
        )
        return {
            "ok": True,
            "map_text": map_text,
            "files": _parse_files_from_map(map_text),
            "error": None,
            "elapsed": time.perf_counter() - t0,
        }
    except Exception as e:
        return {
            "ok": False,
            "map_text": "",
            "files": [],
            "error": f"{type(e).__name__}: {e}\n{traceback.format_exc()}",
            "elapsed": time.perf_counter() - t0,
        }
    finally:
        # Best-effort cleanup of the per-call tags cache.
        try:
            import shutil

            shutil.rmtree(cache_dir, ignore_errors=True)
        except Exception:
            pass


def main() -> int:
    sys.stdout.write(json.dumps({"ready": True}) + "\n")
    sys.stdout.flush()
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError as e:
            sys.stdout.write(json.dumps({"ok": False, "error": f"bad-json: {e}"}) + "\n")
            sys.stdout.flush()
            continue
        if req.get("op") == "shutdown":
            break
        resp = _handle_request(req)
        sys.stdout.write(json.dumps(resp) + "\n")
        sys.stdout.flush()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
