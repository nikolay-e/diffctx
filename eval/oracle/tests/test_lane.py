import hashlib
import io
import json
import os
import sys
import tarfile
from pathlib import Path

import pytest

import jvmtoolchains
import lane


def fake_jdk_pins() -> jvmtoolchains.Pins:
    jdks = [
        jvmtoolchains.Jdk("1.8", "jdk8u512-b09", "https://example.test/8.tar.gz", "8"),
        jvmtoolchains.Jdk("11", "jdk-11.0.33+10", "https://example.test/11.tar.gz", "11"),
    ]
    return jvmtoolchains.Pins("3.9.16", "https://example.test/mvn.tar.gz", "m", jdks)


def tarball(members: dict[str, bytes]) -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as tar:
        for name, content in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(content)
            tar.addfile(info, io.BytesIO(content))
    return buffer.getvalue()


class Downloads:
    """What the network would serve for each pinned URL; a call is recorded, an unknown URL is an error."""

    def __init__(self) -> None:
        self.served = {
            "https://example.test/mvn.tar.gz": tarball({"apache-maven-3.9.16/bin/mvn": b"#!/bin/sh\n"}),
            "https://example.test/8.tar.gz": tarball({"jdk8u512-b09/release": b"8"}),
            "https://example.test/11.tar.gz": tarball({"jdk-11.0.33+10/release": b"11"}),
        }
        self.calls: list[str] = []

    def __call__(self, url: str, algorithm: str, digest: str) -> bytes:
        self.calls.append(url)
        return self.served[url]


def test_the_pins_file_names_three_jdks_and_maven_by_a_checksum_each():
    pins = jvmtoolchains.load_pins()

    assert [(jdk.toolchain, len(jdk.sha256)) for jdk in pins.jdks] == [("1.8", 64), ("11", 64), ("17", 64)]
    assert len(pins.maven_sha512) == 128 and pins.maven_url.endswith("apache-maven-3.9.16-bin.tar.gz")
    assert all(jdk.url.startswith("https://") for jdk in pins.jdks)


def test_prepare_writes_the_gradle_installations_and_one_maven_toolchain_per_jdk_from_a_fake_jdk_directory(tmp_path: Path):
    job, gradle_home, running = tmp_path / "job", tmp_path / "gradle-home", tmp_path / "running-jdk"

    prepared = jvmtoolchains.prepare(job, gradle_home, running, fake_jdk_pins(), Downloads())

    assert prepared.gradle_properties.read_text() == (
        f"org.gradle.java.installations.paths={job}/jdks/jdk8u512-b09,{job}/jdks/jdk-11.0.33+10\n"
    )
    xml = prepared.toolchains_xml.read_text()
    assert xml.startswith('<?xml version="1.0" encoding="UTF-8"?><toolchains>\n') and xml.endswith("</toolchains>\n")
    assert xml.count("<toolchain>") == 3
    assert (
        "<toolchain><type>jdk</type><provides><version>1.8</version><vendor>temurin</vendor></provides>"
        f"<configuration><jdkHome>{job}/jdks/jdk8u512-b09</jdkHome></configuration></toolchain>"
    ) in xml
    assert f"<version>21</version><vendor>temurin</vendor></provides><configuration><jdkHome>{running}</jdkHome>" in xml
    assert prepared.maven_bin == job / "maven-3.9.16" / "bin" and os.access(prepared.maven_bin / "mvn", os.X_OK)
    assert (job / "jdks" / "jdk-11.0.33+10" / "release").read_bytes() == b"11"  # the archive's own top directory is kept


def test_a_second_prepare_downloads_nothing_and_a_download_that_misses_its_checksum_is_refused(tmp_path: Path):
    downloads = Downloads()
    jvmtoolchains.prepare(tmp_path / "job", tmp_path / "gh", tmp_path / "jdk", fake_jdk_pins(), downloads)
    first = len(downloads.calls)

    jvmtoolchains.prepare(tmp_path / "job", tmp_path / "gh", tmp_path / "jdk", fake_jdk_pins(), downloads)

    assert first == 3 and len(downloads.calls) == first
    with pytest.raises(ValueError, match="non-https"):
        jvmtoolchains.download_verified("http://example.test/x", "sha256", hashlib.sha256(b"").hexdigest())


def test_the_maven_local_repository_is_claimed_over_whatever_the_home_held(tmp_path: Path):
    home, claim = tmp_path / "home", tmp_path / "claim"
    (home / ".m2").mkdir(parents=True)
    (home / ".m2" / "stale").write_text("x")
    claim.mkdir()

    jvmtoolchains.claim_local_repository(claim, home)

    assert (home / ".m2").is_symlink() and (home / ".m2").resolve() == claim.resolve()


def test_a_lane_measures_each_repository_once_per_budget_into_the_lane_directory_and_a_failure_does_not_stop_the_rest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(lane, "CACHE", tmp_path / "cache")
    (tmp_path / "cache" / "trees").mkdir(parents=True)
    script = tmp_path / "measure_stub.py"
    script.write_text(
        "import os, sys, json\n"
        "with open(os.environ['STUB_LOG'], 'a') as log:\n"
        "    log.write(json.dumps(sys.argv[1:]) + '\\n')\n"
        "sys.exit(1 if sys.argv[2] == 'bad' else 0)\n"
    )
    env = {**os.environ, "STUB_LOG": str(tmp_path / "calls.jsonl")}

    status = lane.run_lane("jvm", ["good", "bad", "last"], tmp_path / "out", env, script)

    calls = [json.loads(line) for line in (tmp_path / "calls.jsonl").read_text().splitlines()]
    out = tmp_path / "out"
    assert status == 1
    assert calls == [
        [
            "--repo",
            repo,
            *(["--budget", "uncapped"] if budget == "uncapped" else []),
            "--append",
            "--out",
            str(out / budget / "jvm"),
        ]
        for budget in ("default", "uncapped")
        for repo in ("good", "bad", "last")
    ]
    assert not (tmp_path / "cache" / "trees").exists()  # each repository's trees are dropped before the next


def test_the_lane_is_a_command_of_the_oracle_entry_point():
    import run

    assert run.COMMANDS["lane"] is lane.main
    assert lane.measure_command("hono", "default", Path("o"))[:2] == [sys.executable, str(lane.MEASURE)]
