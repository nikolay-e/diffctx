def main() -> None:
    """Main function to run the TreeMapper tool."""
    # Import here to allow argparse to handle --help/-h before importing heavy modules
    from .cli import parse_args
    from .ignore import get_ignore_specs
    from .logger import setup_logging
    from .tree import build_tree
    from .writer import write_tree_to_file

    (
        root_dir,
        ignore_file,
        output_file,
        no_default_ignores,
        verbosity,
        output_format,
        max_depth,
        no_content,
        max_file_bytes,
    ) = parse_args()

    setup_logging(verbosity)

    combined_spec = get_ignore_specs(root_dir, ignore_file, no_default_ignores, output_file)

    directory_tree = {
        "name": root_dir.name,
        "type": "directory",
        "children": build_tree(root_dir, root_dir, combined_spec, output_file, max_depth, 0, no_content, max_file_bytes),
    }

    write_tree_to_file(directory_tree, output_file, output_format)


if __name__ == "__main__":
    main()
