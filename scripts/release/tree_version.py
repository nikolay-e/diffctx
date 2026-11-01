"""The version this checkout is, as opposed to the version users pin (#570).

A release patches the tree surfaces and the user pins together; once it has
shipped, the tree moves on to `X.Y.(Z+1).dev0` and the pins stay on the release.
Only the tree surfaces are written here: the version module, pyproject, the
manifest of every workspace crate (they all carry one version, and the
`=` requirement of each on its siblings), and the diffctx entries of Cargo.lock
and uv.lock.
"""

from __future__ import annotations

import argparse
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

_VERSION = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:(a|b|rc|\.dev|\.post)(\d+))?")
_SEMVER_LABEL = {"a": "a", "b": "b", "rc": "rc", ".dev": "dev", ".post": "post"}


def _parse(version: str) -> re.Match[str]:
    match = _VERSION.fullmatch(version)
    if match is None:
        raise ValueError(f"not a version this repository releases: {version!r}")
    return match


def cargo_spelling(version: str) -> str:
    """The PEP 440 version as Cargo writes it: `2.0.0.dev0` is `2.0.0-dev.0`."""
    major, minor, patch, label, number = _parse(version).groups()
    base = f"{major}.{minor}.{patch}"
    return base if label is None else f"{base}-{_SEMVER_LABEL[label]}.{number}"


def next_dev(released: str) -> str:
    major, minor, patch, label, _ = _parse(released).groups()
    if label is not None:
        raise ValueError(f"only a final release opens a development version: {released!r}")
    return f"{major}.{minor}.{int(patch) + 1}.dev0"


def _rewrite(path: Path, pattern: str, replacement: str, what: str, expected: int = 1) -> None:
    text = path.read_text(encoding="utf-8")
    text, count = re.subn(pattern, lambda m: m.group(1) + replacement + m.group(2), text, count=expected)
    if count != expected:
        raise SystemExit(f"{path.name}: {what} not found")
    path.write_text(text, encoding="utf-8")


def workspace_crates(root: Path = ROOT) -> dict[str, Path]:
    """Package name to manifest of every crate under crates/."""
    crates = {}
    for manifest in sorted((root / "crates").glob("*/Cargo.toml")):
        name = re.search(r'(?m)^name\s*=\s*"([^"]+)"', manifest.read_text(encoding="utf-8"))
        if name is None:
            raise SystemExit(f"{manifest}: no package name")
        crates[name.group(1)] = manifest
    return crates


def set_tree_version(version: str, root: Path = ROOT) -> None:
    cargo = cargo_spelling(version)
    _rewrite(root / "src/diffctx/version.py", r'(__version__\s*=\s*")[^"]*(")', version, "__version__ assignment")
    _rewrite(root / "pyproject.toml", r'(?m)(^version\s*=\s*")[^"]+(")', version, "[project].version")
    crates = workspace_crates(root)
    for name, manifest in crates.items():
        _rewrite(manifest, r'(?m)(^version\s*=\s*")[^"]+(")', cargo, "[package].version")
        siblings = "|".join(re.escape(other) for other in crates if other != name)
        text = manifest.read_text(encoding="utf-8")
        if siblings and re.search(rf"(?m)^(?:{siblings})\s*=", text):
            _rewrite(
                manifest,
                rf'(?m)(^(?:{siblings})\s*=\s*\{{[^}}\n]*version\s*=\s*"=)[^"]+(")',
                cargo,
                "sibling requirement",
                expected=len(re.findall(rf"(?m)^(?:{siblings})\s*=", text)),
            )
        _rewrite(
            root / "Cargo.lock",
            rf'(\[\[package\]\]\nname = "{re.escape(name)}"\nversion = ")[^"]+(")',
            cargo,
            f"{name} package entry",
        )
    _rewrite(
        root / "uv.lock",
        r'(\[\[package\]\]\nname = "diffctx"\nversion = ")[^"]+("\nsource = \{ editable = "\." \})',
        version,
        "editable diffctx entry",
    )


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("set", help="write this version on every tree surface").add_argument("version")
    commands.add_parser("open-dev", help="write the development version that follows this release").add_argument("released")
    args = parser.parse_args(argv)
    try:
        version = args.version if args.command == "set" else next_dev(args.released)
        set_tree_version(version)
    except ValueError as error:
        parser.error(str(error))
    print(version)


if __name__ == "__main__":
    main()
