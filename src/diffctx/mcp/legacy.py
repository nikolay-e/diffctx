"""Pre-v3 MCP tool surface (`get_tree_map`, `get_file_context`), opt-in via
`DIFFCTX_MCP_LEGACY_TOOLS=1`. Kept out of `server.py` so the default surface
reads as what it is: one tool, `diffctx_context`."""

from __future__ import annotations

from pathlib import Path

from mcp.server.fastmcp import FastMCP

from .fetch import withheld_set
from .security import validate_dir_path, validate_repo_path
from .server import (
    _DEFAULT_MAX_FILE_BYTES,
    _DEFAULT_MAX_TOKENS,
    _UNTRUSTED_NOTICE,
    _copy_or_degrade,
    _over_token_budget_notice,
    _read_only,
    _run_with_deadline,
)

_TREE_MAP_DESCRIPTION = (
    "Get a structured map of a codebase — directory tree with file contents "
    "in YAML or Markdown, optimized for LLM comprehension. "
    "Respects .gitignore, skips binaries/build artifacts.\n\n"
    "PREFER this over reading files one-by-one when you need:\n"
    "- Project structure overview\n"
    "- Multiple file contents from a subdirectory\n"
    "- Full codebase context for analysis\n\n"
    "Set clipboard=true to copy to clipboard without flooding context.\n"
    "Use no_content=true for structure-only view." + _UNTRUSTED_NOTICE
)


async def get_tree_map(
    repo_path: str,
    subdirectory: str = "",
    output_format: str = "yaml",
    no_content: bool = False,
    max_depth: int | None = None,
    max_file_bytes: int = _DEFAULT_MAX_FILE_BYTES,
    clipboard: bool = False,
    max_tokens: int = _DEFAULT_MAX_TOKENS,
) -> str:
    from diffctx.ignore import get_ignore_specs, get_whitelist_spec
    from diffctx.tokens import count_tokens
    from diffctx.tree import TreeBuildContext, build_tree
    from diffctx.writer import tree_to_string

    validated_path = validate_repo_path(repo_path)
    target = validated_path / subdirectory if subdirectory else validated_path
    if subdirectory and not _is_contained(target, validated_path):
        raise ValueError(f"subdirectory escapes repo_path: {subdirectory}")
    if not target.is_dir():
        raise ValueError(f"Not a directory: {subdirectory or repo_path}")

    def _build() -> str:
        ctx = TreeBuildContext(
            base_dir=target,
            combined_spec=get_ignore_specs(target, None, False, None),
            output_file=None,
            max_depth=max_depth,
            no_content=no_content,
            max_file_bytes=max_file_bytes,
            whitelist_spec=get_whitelist_spec(None, target),
        )
        tree = {"name": target.name or str(target), "type": "directory", "children": build_tree(target, ctx)}
        return tree_to_string(tree, output_format)

    content: str = await _run_with_deadline("get_tree_map", _build)
    token_info = count_tokens(content)

    if clipboard:
        degraded_notice = await _copy_or_degrade(content)
        if degraded_notice is None:
            return f"Copied to clipboard ({token_info.count:,} tokens, {token_info.encoding})"
        content = degraded_notice + content
        token_info = count_tokens(content)

    if token_info.count > max_tokens:
        return _over_token_budget_notice(
            "get_tree_map",
            token_info.count,
            max_tokens,
            "use subdirectory, no_content=true, or max_depth",
        )

    return f"<!-- {token_info.count:,} tokens ({token_info.encoding}) -->\n{content}"


_FILE_CONTEXT_DESCRIPTION = (
    "Read files by glob pattern, formatted for LLM consumption. "
    "Works on ANY directory (no git required).\n\n"
    "PREFER this over reading files individually when you need 2+ files. "
    "Use clipboard=true to copy to clipboard without flooding context.\n\n"
    "Examples:\n"
    '- patterns=["src/**/*.py"] — all Python files\n'
    '- patterns=["eval/*.py", "tests/conftest.py"] — specific sets\n'
    '- patterns=["*.md"] with dry_run=true — preview what matches\n\n'
    "Honours .gitignore and .diffctx/ignore: excluded files are never returned "
    "and are not reported as truncated, so a glob cannot read what the repo "
    "withholds from the other tools." + _UNTRUSTED_NOTICE
)


def _is_contained(child: Path, root: Path) -> bool:
    try:
        return child.resolve().is_relative_to(root.resolve())
    except (OSError, ValueError):
        return False


def _contained_match(match: str, root: Path) -> tuple[Path, str] | None:
    """The resolved path and its root-relative form if this glob hit lies inside `root`.

    Resolved rather than as-globbed because containment is established on the
    resolved path and `root` is resolved too, so only that form is guaranteed to
    be expressible relative to it. The reporters call `relative_to` with no
    fallback, and an unresolved match — reachable when a pattern is absolute and
    arrives via a symlink — is lexically outside the root even though it points
    inside, turning an accepted file into an opaque ValueError.
    """
    p = Path(match)
    if not p.is_file() or not _is_contained(p, root):
        return None
    resolved = p.resolve()
    try:
        rel = resolved.relative_to(root.resolve()).as_posix()
    except ValueError:
        return None
    return resolved, rel


def _collect_matched_files(validated_path: Path, patterns: list[str], max_files: int) -> tuple[list[Path], int]:
    """Files matching `patterns`, minus what this tool may not serve.

    Two filters, and they answer different questions. The **noise** spec is the
    one `get_tree_map` applies — `node_modules/`, `target/`, lock files: not
    secrets, just not what anyone globbing a repository means. The **engine**
    predicate is the security floor (`.diffctx/ignore`, gitignore, secret
    paths), applied for the same reason diff mode applies it, because this tool
    accepts `**/*`. Dropping the noise spec for the engine alone (#228) let
    `uv.lock` and every file under an ignored directory through; keeping only
    the noise spec, as before that, admitted `.netrc`. Both, in that order —
    the cheap local one first, so the engine is asked about a bounded list.

    Excluded files are dropped silently and NOT counted: `total_matched` drives
    the truncation notice, and reporting "3 more files" for files the repo
    refuses to expose would leak their existence.
    """
    import glob as globmod

    from diffctx.ignore import get_ignore_specs, should_ignore

    noise = get_ignore_specs(validated_path, None, False, None)
    contained: list[tuple[Path, str]] = []
    seen: set[Path] = set()
    for pattern in patterns:
        for match in sorted(globmod.glob(str(validated_path / pattern), recursive=True)):
            hit = _contained_match(match, validated_path)
            if hit is None or hit[0] in seen or should_ignore(hit[1], noise):
                continue
            seen.add(hit[0])
            contained.append(hit)
    withheld = withheld_set(validated_path, [rel for _, rel in contained])
    admitted = [resolved for resolved, rel in contained if rel not in withheld]
    return admitted[:max_files], len(admitted)


def _truncation_notice(shown: int, total_matched: int, max_files: int) -> str | None:
    if total_matched <= shown:
        return None
    return f"TRUNCATED: showing {shown} of {total_matched} matched files (max_files={max_files}). Narrow patterns or raise max_files to see the rest."


def _build_dry_run_report(matched: list[Path], total_matched: int, validated_path: Path, max_files: int) -> str:
    total_bytes = sum(p.stat().st_size for p in matched if p.exists())
    lines = [f"Would match {total_matched} files (~{total_bytes:,} bytes for the {len(matched)} shown below):"]
    notice = _truncation_notice(len(matched), total_matched, max_files)
    if notice:
        lines.append(notice)
    for p in matched:
        rel = p.relative_to(validated_path)
        lines.append(f"  {rel} ({p.stat().st_size:,}b)")
    return "\n".join(lines)


def _build_file_content_report(
    matched: list[Path], total_matched: int, validated_path: Path, max_file_bytes: int, max_files: int
) -> tuple[str, int, int]:
    header = f"# {len(matched)} files matched"
    notice = _truncation_notice(len(matched), total_matched, max_files)
    if notice:
        header += f" ({total_matched} total)\n{notice}"
    parts = [header + "\n"]
    total_lines = 0
    included_count = 0
    for p in matched:
        rel = p.relative_to(validated_path)
        try:
            size = p.stat().st_size
            if size > max_file_bytes:
                parts.append(f"## {rel}\n*Skipped: {size:,} bytes exceeds limit*\n")
                continue
            content = p.read_text(encoding="utf-8", errors="replace")
            total_lines += content.count("\n") + 1
            included_count += 1
            suffix = p.suffix.lstrip(".")
            parts.append(f"## {rel}\n```{suffix}\n{content}\n```\n")
        except OSError as e:
            # `{e}` on an OSError carries the absolute filename; the path is
            # already stated as `rel` above it, so only the reason is added.
            parts.append(f"## {rel}\n*Error: {e.strerror or type(e).__name__}*\n")
    return "\n".join(parts), included_count, total_lines


async def get_file_context(
    repo_path: str,
    patterns: list[str],
    max_files: int = 50,
    max_file_bytes: int = _DEFAULT_MAX_FILE_BYTES,
    clipboard: bool = False,
    dry_run: bool = False,
    max_tokens: int = _DEFAULT_MAX_TOKENS,
) -> str:
    validated_path = validate_dir_path(repo_path)

    def _read() -> tuple[str, int, int]:
        matched, total_matched = _collect_matched_files(validated_path, patterns, max_files)
        if not matched:
            return f"No files matched patterns: {patterns}", 0, 0
        if dry_run:
            return _build_dry_run_report(matched, total_matched, validated_path, max_files), 0, 0
        return _build_file_content_report(matched, total_matched, validated_path, max_file_bytes, max_files)

    raw_result: tuple[str, int, int] = await _run_with_deadline("get_file_context", _read)
    content, n_files, n_lines = raw_result

    if dry_run:
        return content

    if clipboard and n_files > 0:
        degraded_notice = await _copy_or_degrade(content)
        if degraded_notice is None:
            return f"Copied {n_files} files ({n_lines:,} lines) to clipboard"
        content = degraded_notice + content

    from diffctx.tokens import count_tokens

    token_count = count_tokens(content).count
    if token_count > max_tokens:
        return _over_token_budget_notice(
            "get_file_context",
            token_count,
            max_tokens,
            "tighten patterns, lower max_files, or use dry_run=true first",
        )

    return content


def register(server: FastMCP) -> None:
    server.tool(description=_TREE_MAP_DESCRIPTION, annotations=_read_only("Get tree map"))(get_tree_map)
    server.tool(description=_FILE_CONTEXT_DESCRIPTION, annotations=_read_only("Get file context"))(get_file_context)
