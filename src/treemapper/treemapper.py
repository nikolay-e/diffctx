from .cli import parse_args
from .ignore import get_ignore_specs
from .logger import setup_logging
from .tree import build_tree
from .writer import write_tree_to_file


def main() -> None:
    """Main function to run the TreeMapper tool."""

    root_dir, ignore_file, output_file, no_default_ignores, verbosity = parse_args()

    setup_logging(verbosity)

    combined_spec, gitignore_specs = get_ignore_specs(root_dir, ignore_file, no_default_ignores, output_file)

    directory_tree = {
        "name": root_dir.name,
        "type": "directory",
        "children": build_tree(root_dir, root_dir, combined_spec, gitignore_specs),
    }

    write_tree_to_file(directory_tree, output_file)


if __name__ == "__main__":
    main()
