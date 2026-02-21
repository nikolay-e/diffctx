from __future__ import annotations

import logging
import sys
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from .cli import ParsedArgs


def _configure_windows_utf8() -> None:
    if sys.platform == "win32" and hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")


def _build_diff_tree(args: ParsedArgs) -> dict[str, Any]:
    from .diffctx import GitError, build_diff_context

    assert args.diff_range is not None
    try:
        return build_diff_context(
            root_dir=args.root_dir,
            diff_range=args.diff_range,
            budget_tokens=args.budget,
            alpha=args.alpha,
            tau=args.tau,
            no_content=args.no_content,
            ignore_file=args.ignore_file,
            no_default_ignores=args.no_default_ignores,
            full=args.full_diff,
            whitelist_file=args.whitelist_file,
        )
    except GitError as e:
        print(f"Error: {e}", file=sys.stderr)
        sys.exit(1)


def _build_standard_tree(args: ParsedArgs) -> dict[str, Any]:
    from .ignore import get_ignore_specs, get_whitelist_spec
    from .tree import TreeBuildContext, build_tree

    ctx = TreeBuildContext(
        base_dir=args.root_dir,
        combined_spec=get_ignore_specs(args.root_dir, args.ignore_file, args.no_default_ignores, args.output_file),
        output_file=args.output_file,
        max_depth=args.max_depth,
        no_content=args.no_content,
        max_file_bytes=args.max_file_bytes,
        whitelist_spec=get_whitelist_spec(args.whitelist_file, args.root_dir),
    )
    return {
        "name": args.root_dir.name,
        "type": "directory",
        "children": build_tree(args.root_dir, ctx),
    }


def _handle_clipboard(output_content: str, args: ParsedArgs) -> bool:
    from .clipboard import ClipboardError, copy_to_clipboard

    if not args.copy:
        return False
    try:
        copy_to_clipboard(output_content)
        print("Copied to clipboard", file=sys.stderr)
        return True
    except ClipboardError as e:
        print(f"Clipboard unavailable: {e}", file=sys.stderr)
        return False


def _handle_output_file(output_content: str, args: ParsedArgs) -> None:
    from .writer import write_string_to_file

    if not args.output_file:
        return
    try:
        write_string_to_file(output_content, args.output_file, args.output_format)
        print(f"Saved to {args.output_file}", file=sys.stderr)
    except IsADirectoryError:
        print(f"Error: {args.output_file} is a directory", file=sys.stderr)
        sys.exit(1)
    except OSError as e:
        print(f"Error: cannot write {args.output_file}: {e}", file=sys.stderr)
        sys.exit(1)


def _run() -> None:
    from .cli import parse_args
    from .logger import setup_logging
    from .tokens import print_token_summary
    from .writer import tree_to_string

    args = parse_args()
    setup_logging(args.verbosity)

    directory_tree = _build_diff_tree(args) if args.diff_range else _build_standard_tree(args)

    output_content = tree_to_string(directory_tree, args.output_format)
    print_token_summary(output_content)

    clipboard_ok = _handle_clipboard(output_content, args)
    _handle_output_file(output_content, args)

    should_write_stdout = args.force_stdout or not args.copy or not clipboard_ok
    if not args.output_file and should_write_stdout:
        sys.stdout.write(output_content)
        logging.info("Directory tree written to stdout in %s format", args.output_format)


def main() -> None:
    _configure_windows_utf8()
    try:
        _run()
    except KeyboardInterrupt:
        print("\nInterrupted", file=sys.stderr)
        sys.exit(130)
    except BrokenPipeError:
        sys.stderr.close()
        sys.exit(141)


if __name__ == "__main__":
    main()
