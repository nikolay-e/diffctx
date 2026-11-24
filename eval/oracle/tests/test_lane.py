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
from corpus import load_corpus


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

    prepared = jvmtoolchains.prepare(job, gradle_home, job / "m2", running, fake_jdk_pins(), Downloads(), env={})

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
    jvmtoolchains.prepare(
        tmp_path / "job", tmp_path / "gh", tmp_path / "m2", tmp_path / "jdk", fake_jdk_pins(), downloads, env={}
    )
    first = len(downloads.calls)

    jvmtoolchains.prepare(
        tmp_path / "job", tmp_path / "gh", tmp_path / "m2", tmp_path / "jdk", fake_jdk_pins(), downloads, env={}
    )

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


def test_a_runner_that_has_every_jdk_gets_its_toolchain_files_and_downloads_nothing(tmp_path: Path):
    homes = {major: tmp_path / "tool-cache" / f"jdk{major}" for major in ("8", "11", "21")}
    for home in homes.values():
        home.mkdir(parents=True)
    env = {f"JAVA_HOME_{major}_X64": str(home) for major, home in homes.items()}
    pins = fake_jdk_pins()
    downloads = Downloads()

    prepared = jvmtoolchains.prepare(
        tmp_path / "job", tmp_path / "gh", tmp_path / "m2", tmp_path / "unused", pins, downloads, env=env, maven=False
    )

    assert downloads.calls == [] and prepared.maven_bin is None
    assert prepared.gradle_properties.read_text() == f"org.gradle.java.installations.paths={homes['8']},{homes['11']}\n"
    xml = prepared.toolchains_xml.read_text()
    assert f"<version>1.8</version><vendor>temurin</vendor></provides><configuration><jdkHome>{homes['8']}</jdkHome>" in xml
    assert f"<version>21</version><vendor>temurin</vendor></provides><configuration><jdkHome>{homes['21']}</jdkHome>" in xml
    assert prepared.jdk_homes == [homes["8"], homes["11"], homes["21"]]
    assert not (tmp_path / "job").exists()  # nothing was fetched or unpacked


def test_a_runner_missing_one_jdk_is_not_taken_for_one_that_has_them_all(tmp_path: Path):
    (tmp_path / "jdk8").mkdir()
    env = {"JAVA_HOME_8_X64": str(tmp_path / "jdk8"), "JAVA_HOME_11_X64": str(tmp_path / "missing")}

    assert jvmtoolchains.runner_jdks(fake_jdk_pins(), env) is None


def test_the_throwaway_container_mounts_the_runner_jdks_read_only(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    import runners

    launcher = tmp_path / "scip-java"
    launcher.write_bytes(b"#!/bin/sh\n")
    monkeypatch.setattr(runners, "tool_path", lambda tool: launcher)
    monkeypatch.setattr(runners, "CACHE", tmp_path / "cache")
    monkeypatch.setattr(runners, "sandboxed", lambda: False)
    monkeypatch.setattr(runners.shutil, "which", lambda name: "/usr/bin/docker")
    monkeypatch.setenv(runners.JDK_HOMES_VARIABLE, os.pathsep.join(["/opt/jdk8", "/opt/jdk11"]))
    ctx = runners.Context(
        load_corpus(),
        next(r for r in load_corpus().repos if r.language == "java"),
        "a" * 40,
        tmp_path / "t",
        tmp_path / "o" / "i.scip",
        tmp_path,
        {},
        True,
        5,
    )
    ctx.tree.mkdir()
    (ctx.tree / "pom.xml").write_text("")

    command = runners.plan_java(ctx).command

    assert [
        command[i + 1]
        for i, arg in enumerate(command)
        if arg == "-v" and command[i + 1].endswith(":ro") and "jdk" in command[i + 1]
    ] == [
        "/opt/jdk8:/opt/jdk8:ro",
        "/opt/jdk11:/opt/jdk11:ro",
    ]


def test_the_plan_names_every_corpus_repository_once_and_the_fast_scope_one_per_language():
    import plan

    corpus = load_corpus()
    full = plan.plan(corpus, "full")["include"]
    fast = plan.plan(corpus, "fast")["include"]

    assert [entry["repo"] for entry in full] == [repo.name for repo in corpus.repos]
    assert {entry["repo"] for entry in fast} == {repo.name for repo in corpus.repos if repo.fast} and all(e["fast"] for e in fast)
    assert all(entry["lane"] == entry["language"] for entry in full)


def test_merge_joins_the_parts_and_refuses_parts_of_two_engines(tmp_path: Path):
    import merge

    for name, engine, row in (("a", "diffctx 9", "1"), ("b", "diffctx 9", "2"), ("c", "diffctx 8", "3")):
        (tmp_path / name).mkdir()
        (tmp_path / name / "run.json").write_text(json.dumps({"engine": engine, "budget": None, "fresh": True}))
        (tmp_path / name / "commits.jsonl").write_text(json.dumps({"repo": row}))  # no trailing newline, as a part may end

    merge.merge([tmp_path / "a", tmp_path / "b"], tmp_path / "merged")

    assert [json.loads(line)["repo"] for line in (tmp_path / "merged" / "commits.jsonl").read_text().splitlines()] == ["1", "2"]
    with pytest.raises(SystemExit, match="one engine"):
        merge.merge([tmp_path / "a", tmp_path / "c"], tmp_path / "bad")


def test_a_fast_lane_asks_for_the_fast_commit_count_and_a_retry_only_on_the_first_budget(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setattr(lane, "CACHE", tmp_path / "cache")
    script = tmp_path / "measure_stub.py"
    script.write_text("import os, sys, json\nopen(os.environ['STUB_LOG'], 'a').write(json.dumps(sys.argv[1:]) + '\\n')\n")
    env = {**os.environ, "STUB_LOG": str(tmp_path / "calls.jsonl")}

    lane.run_lane("python", ["r"], tmp_path / "out", env, script, fast=True, retry_failed=True)

    calls = [json.loads(line) for line in (tmp_path / "calls.jsonl").read_text().splitlines()]
    assert ["--fast" in call for call in calls] == [True, True]
    assert ["--retry-failed" in call for call in calls] == [True, False]


@pytest.mark.parametrize(("lane_name", "prepared"), [("kotlin", True), ("java", True), ("jvm", True), ("python", False)])
def test_a_jvm_lane_named_after_its_language_prepares_the_runners_jdks_for_the_build_container(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, lane_name: str, prepared: bool
):
    homes = {}
    for major in ("8", "11", "17", "21"):
        homes[major] = tmp_path / "tool-cache" / major
        homes[major].mkdir(parents=True)
        monkeypatch.setenv(f"JAVA_HOME_{major}_X64", str(homes[major]))
    monkeypatch.setattr(lane, "CACHE", tmp_path / "cache")
    monkeypatch.setattr(lane, "in_container", lambda: False)

    env = lane.prepare_environment(lane_name, tmp_path / "job", None)

    assert (lane.JDK_HOMES_VARIABLE in env) is prepared
    if prepared:
        assert env[lane.JDK_HOMES_VARIABLE].split(os.pathsep) == [str(homes[major]) for major in ("8", "11", "17", "21")]
        assert (tmp_path / "cache" / "gradle-home" / "gradle.properties").is_file()
