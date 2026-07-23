from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

REPO_URL = "https://github.com/nikolay-e/diffctx"
DESCRIPTION = "Selects the minimum code an LLM needs to review a git diff"

WINDOWS_TARGET = "x86_64-pc-windows-msvc"
LINUX_TARGETS = {"x86_64": "x86_64-unknown-linux-gnu", "aarch64": "aarch64-unknown-linux-gnu"}


def asset_name(version: str, target: str) -> str:
    suffix = "zip" if target == WINDOWS_TARGET else "tar.gz"
    return f"diffctx-{version}-{target}.{suffix}"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def collect_checksums(assets_dir: Path, version: str) -> dict[str, str]:
    checksums: dict[str, str] = {}
    for target in [WINDOWS_TARGET, *LINUX_TARGETS.values(), "aarch64-apple-darwin"]:
        path = assets_dir / asset_name(version, target)
        if not path.is_file():
            raise SystemExit(f"missing release asset: {path}")
        checksums[path.name] = sha256_of(path)
    return checksums


def render_scoop(version: str, checksums: dict[str, str]) -> str:
    archive = asset_name(version, WINDOWS_TARGET)
    manifest = {
        "version": version,
        "description": DESCRIPTION,
        "homepage": REPO_URL,
        "license": "Apache-2.0",
        "architecture": {
            "64bit": {
                "url": f"{REPO_URL}/releases/download/v{version}/{archive}",
                "hash": checksums[archive],
            }
        },
        "bin": "diffctx.exe",
        "checkver": {"github": REPO_URL},
        "autoupdate": {
            "architecture": {"64bit": {"url": f"{REPO_URL}/releases/download/v$version/diffctx-$version-{WINDOWS_TARGET}.zip"}}
        },
    }
    return json.dumps(manifest, indent=4) + "\n"


def render_pkgbuild(version: str, checksums: dict[str, str]) -> str:
    lines = [
        "# Maintainer: Nikolay Eremeev <nikolay.eremeev@outlook.com>",
        "pkgname=diffctx-bin",
        f"pkgver={version}",
        "pkgrel=1",
        f'pkgdesc="{DESCRIPTION}"',
        "arch=('x86_64' 'aarch64')",
        f'url="{REPO_URL}"',
        "license=('Apache-2.0')",
        "depends=('git' 'gcc-libs')",
        "provides=('diffctx')",
        "conflicts=('diffctx')",
    ]
    for arch, target in LINUX_TARGETS.items():
        archive = asset_name(version, target)
        lines.append(f'source_{arch}=("{REPO_URL}/releases/download/v{version}/{archive}")')
        lines.append(f"sha256sums_{arch}=('{checksums[archive]}')")
    lines.extend(
        [
            "",
            "package() {",
            '    install -Dm755 diffctx "$pkgdir/usr/bin/diffctx"',
            '    install -Dm644 LICENSE "$pkgdir/usr/share/licenses/$pkgname/LICENSE"',
            "}",
        ]
    )
    return "\n".join(lines) + "\n"


def render_srcinfo(version: str, checksums: dict[str, str]) -> str:
    lines = [
        "pkgbase = diffctx-bin",
        f"\tpkgdesc = {DESCRIPTION}",
        f"\tpkgver = {version}",
        "\tpkgrel = 1",
        f"\turl = {REPO_URL}",
        "\tarch = x86_64",
        "\tarch = aarch64",
        "\tlicense = Apache-2.0",
        "\tdepends = git",
        "\tdepends = gcc-libs",
        "\tprovides = diffctx",
        "\tconflicts = diffctx",
    ]
    for arch, target in LINUX_TARGETS.items():
        archive = asset_name(version, target)
        lines.append(f"\tsource_{arch} = {REPO_URL}/releases/download/v{version}/{archive}")
        lines.append(f"\tsha256sums_{arch} = {checksums[archive]}")
    lines.extend(["", "pkgname = diffctx-bin"])
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True)
    parser.add_argument("--assets-dir", required=True, type=Path)
    parser.add_argument("--scoop", type=Path)
    parser.add_argument("--pkgbuild", type=Path)
    parser.add_argument("--srcinfo", type=Path)
    parser.add_argument("--npm-checksums", type=Path)
    args = parser.parse_args()

    checksums = collect_checksums(args.assets_dir, args.version)

    for path, content in (
        (args.scoop, render_scoop(args.version, checksums)),
        (args.pkgbuild, render_pkgbuild(args.version, checksums)),
        (args.srcinfo, render_srcinfo(args.version, checksums)),
        (args.npm_checksums, json.dumps(checksums, indent=2, sort_keys=True) + "\n"),
    ):
        if path is None:
            continue
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
