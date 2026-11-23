"""What a JVM lane needs on a container's disk before its builds start: Maven, three more JDKs, and the two files that
tell Maven and Gradle where those JDKs are. Nothing here edits a measured project.

Every download is https and checked against the checksum pinned in `jvm-toolchains.toml`; the generated files are
plain text so a test reads them from a fake JDK directory.
"""

from __future__ import annotations

import hashlib
import os
import shutil
import tarfile
import tempfile
import tomllib
import urllib.request
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

PINS_FILE = Path(__file__).with_name("jvm-toolchains.toml")
RUNNING_JDK_TOOLCHAIN = "21"  # the JDK the container itself runs, named for Maven beside the downloaded ones


@dataclass(frozen=True)
class Jdk:
    toolchain: str
    directory: str
    url: str
    sha256: str


@dataclass(frozen=True)
class Pins:
    maven_version: str
    maven_url: str
    maven_sha512: str
    jdks: list[Jdk]


@dataclass(frozen=True)
class Prepared:
    maven_bin: Path
    gradle_properties: Path
    toolchains_xml: Path


Fetch = Callable[[str, str, str], bytes]  # url, algorithm, hex digest -> the verified bytes


def load_pins(path: Path = PINS_FILE) -> Pins:
    data = tomllib.loads(path.read_text(encoding="utf-8"))
    maven = data["maven"]
    return Pins(
        maven["version"],
        maven["url"].format(version=maven["version"]),
        maven["sha512"],
        [Jdk(j["toolchain"], j["directory"], j["url"], j["sha256"]) for j in data["jdk"]],
    )


def download_verified(url: str, algorithm: str, digest: str) -> bytes:
    if not url.startswith("https://"):
        raise ValueError(f"refusing a non-https download: {url}")
    with urllib.request.urlopen(url, timeout=600) as response:
        payload = response.read()
    actual = hashlib.new(algorithm, payload).hexdigest()
    if actual != digest:
        raise RuntimeError(f"{url}: {algorithm} mismatch, expected {digest}, got {actual}")
    return payload


def gradle_properties(jdk_homes: list[Path]) -> str:
    """Gradle's own installations property: toolchain requests are answered from these directories, offline."""
    return f"org.gradle.java.installations.paths={','.join(str(home) for home in jdk_homes)}\n"


def toolchains_xml(entries: list[tuple[str, Path]]) -> str:
    """`~/.m2/toolchains.xml`: one JDK per version a pom may ask maven-toolchains-plugin for."""
    lines = ['<?xml version="1.0" encoding="UTF-8"?><toolchains>']
    lines += [
        f"<toolchain><type>jdk</type><provides><version>{version}</version><vendor>temurin</vendor></provides>"
        f"<configuration><jdkHome>{home}</jdkHome></configuration></toolchain>"
        for version, home in entries
    ]
    return "\n".join([*lines, "</toolchains>"]) + "\n"


def _unpack(payload: bytes, into: Path, strip: int = 0) -> None:
    with tempfile.NamedTemporaryFile(suffix=".tar.gz") as archive:
        archive.write(payload)
        archive.flush()
        with tarfile.open(archive.name) as tar:
            members = []
            for member in tar.getmembers():
                member.name = "/".join(member.name.split("/")[strip:])
                if member.name:
                    members.append(member)
            into.mkdir(parents=True, exist_ok=True)
            tar.extractall(into, members=members, filter="data")


def prepare(
    job: Path, gradle_home: Path, java_home: Path, pins: Pins | None = None, fetch: Fetch = download_verified
) -> Prepared:
    """Fetch what is missing under `job` and write both toolchain files; a second call downloads nothing."""
    pins = pins or load_pins()
    maven = job / f"maven-{pins.maven_version}"
    if not (maven / "bin" / "mvn").exists():
        _unpack(fetch(pins.maven_url, "sha512", pins.maven_sha512), maven, strip=1)
        (maven / "bin" / "mvn").chmod(0o755)
    jdks = job / "jdks"
    for jdk in pins.jdks:
        if not (jdks / jdk.directory).is_dir():
            _unpack(fetch(jdk.url, "sha256", jdk.sha256), jdks)
    homes = [jdks / jdk.directory for jdk in pins.jdks]
    gradle_home.mkdir(parents=True, exist_ok=True)
    properties = gradle_home / "gradle.properties"
    properties.write_text(gradle_properties(homes), encoding="utf-8")
    m2 = job / "m2"
    m2.mkdir(parents=True, exist_ok=True)
    xml = m2 / "toolchains.xml"
    xml.write_text(
        toolchains_xml(
            [*((jdk.toolchain, home) for jdk, home in zip(pins.jdks, homes, strict=True)), (RUNNING_JDK_TOOLCHAIN, java_home)]
        ),
        encoding="utf-8",
    )
    return Prepared(maven / "bin", properties, xml)


def claim_local_repository(m2: Path, home: Path) -> None:
    """Maven's local repository is `.m2` of the JVM's passwd home, whatever $HOME says: point it at the claim `m2`."""
    home.mkdir(parents=True, exist_ok=True)
    link = home / ".m2"
    if link.is_symlink() or link.is_file():
        link.unlink()
    elif link.exists():
        shutil.rmtree(link)
    link.symlink_to(m2)


def default_java_home() -> Path:
    return Path(os.environ.get("JAVA_HOME") or "/opt/java/openjdk")
