import logging
import sys


def main() -> None:
    # Force UTF-8 for stdout on Windows (default cp1252 can't handle Unicode)
    if sys.platform == "win32" and hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    from .cli import parse_args
    from .clipboard import ClipboardError, copy_to_clipboard
    from .ignore import get_ignore_specs
    from .logger import setup_logging
    from .tokens import print_token_summary
    from .tree import TreeBuildContext, build_tree
    from .writer import tree_to_string, write_string_to_file

    try:
        args = parse_args()
        setup_logging(args.verbosity)

        ctx = TreeBuildContext(
            base_dir=args.root_dir,
            combined_spec=get_ignore_specs(args.root_dir, args.ignore_file, args.no_default_ignores, args.output_file),
            output_file=args.output_file,
            max_depth=args.max_depth,
            no_content=args.no_content,
            max_file_bytes=args.max_file_bytes,
        )

        directory_tree = {
            "name": args.root_dir.name,
            "type": "directory",
            "children": build_tree(args.root_dir, ctx),
        }

        output_content = tree_to_string(directory_tree, args.output_format)
        print_token_summary(output_content)

        clipboard_ok = False
        if args.copy:
            try:
                copy_to_clipboard(output_content)
                print("Copied to clipboard", file=sys.stderr)
                clipboard_ok = True
            except ClipboardError as e:
                print(f"Clipboard unavailable: {e}", file=sys.stderr)

        if args.output_file:
            write_string_to_file(output_content, args.output_file, args.output_format)
        elif args.force_stdout or not args.copy or not clipboard_ok:
            # force_stdout: -o - was explicitly passed (forces stdout even with --copy)
            sys.stdout.write(output_content)
            logging.info(f"Directory tree written to stdout in {args.output_format} format")

    except KeyboardInterrupt:
        print("\nInterrupted", file=sys.stderr)
        sys.exit(130)
    except BrokenPipeError:
        sys.stderr.close()
        sys.exit(141)


if __name__ == "__main__":
    main()
