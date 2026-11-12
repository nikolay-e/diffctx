import gzip
import json
import os
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

import pytest

import gitrepo
import indexing
import runners
from corpus import Tool, load_corpus
from indexing import EXCLUDED_PARTS, scrubbed_env
from toolbin import _place


def context(tmp_path: Path, language: str, name: str = "x") -> runners.Context:
    corpus = load_corpus()
    repo = next(r for r in corpus.repos if r.language == language)
    return runners.Context(
        corpus, repo, "a" * 40, tmp_path / name, tmp_path / "index.scip", tmp_path, scrubbed_env(tmp_path), True, 5
    )


def test_the_environment_handed_to_indexers_carries_no_tokens_and_two_distinct_npm_configs(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    monkeypatch.setenv("GITHUB_TOKEN", "secret")
    monkeypatch.setenv("NPM_TOKEN", "secret")

    env = scrubbed_env(tmp_path)

    assert not any("TOKEN" in key or key.startswith("AWS_") for key in env)
    assert env["npm_config_ignore_scripts"] == "true"
    assert env["npm_config_userconfig"] != env["npm_config_globalconfig"]
    assert env["HOME"] == str(tmp_path)


def test_build_output_directories_are_not_code_files_of_the_snapshot():
    assert {"target", "node_modules", "build", "vendor", ".gradle"} <= set(EXCLUDED_PARTS)


def test_release_archives_are_unpacked_to_the_binary(tmp_path: Path):
    def tool(archive: str) -> Tool:
        return Tool("scip", "1", "https://x/{version}", "any", archive, {"any": "0" * 64})

    packed = tmp_path / "in"
    packed.mkdir()
    (packed / "scip").write_bytes(b"binary")
    with tarfile.open(tmp_path / "a.tar.gz", "w:gz") as archive:
        archive.add(packed / "scip", arcname="release/scip")
    (tmp_path / "a.gz").write_bytes(gzip.compress(b"binary"))
    (tmp_path / "plain").write_bytes(b"binary")

    for kind, payload in (("tar.gz", "a.tar.gz"), ("gz", "a.gz"), ("file", "plain")):
        target = tmp_path / kind / "scip"
        target.parent.mkdir()
        _place(tool(kind), tmp_path / payload, tmp_path / kind, target)
        assert target.read_bytes() == b"binary"


def test_a_container_is_detected_by_its_marker_file_or_its_cgroup(tmp_path: Path):
    marker, cgroup = tmp_path / "dockerenv", tmp_path / "cgroup"
    cgroup.write_text("0::/user.slice/session-3.scope\n")
    assert not runners.in_container(marker, cgroup)

    cgroup.write_text("12:cpu:/docker/3f2a\n")
    assert runners.in_container(marker, cgroup)

    cgroup.write_text("0::/\n")
    marker.write_text("")
    assert runners.in_container(marker, cgroup)
    assert not runners.in_container(tmp_path / "none", tmp_path / "also-none")


def test_a_kubernetes_pod_is_a_container_even_under_a_cgroup_namespace(tmp_path: Path):
    cgroup = tmp_path / "cgroup"
    cgroup.write_text("0::/\n")
    assert not runners.in_container(tmp_path / "none", cgroup, {})
    assert runners.in_container(tmp_path / "none", cgroup, {"KUBERNETES_SERVICE_HOST": "192.0.2.1"})


def test_the_sandbox_variable_alone_vouches_for_nothing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(runners, "in_container", lambda: False)
    monkeypatch.setenv(runners.SANDBOX_VARIABLE, "1")

    assert not runners.sandboxed()
    with pytest.raises(SystemExit, match="no container is detected"):
        runners.refuse_unfounded_sandbox_claim()

    monkeypatch.setattr(runners, "in_container", lambda: True)
    assert runners.sandboxed()
    runners.refuse_unfounded_sandbox_claim()
    monkeypatch.delenv(runners.SANDBOX_VARIABLE)
    assert not runners.sandboxed()


def test_gradle_never_runs_on_the_host_not_even_when_the_variable_is_set(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    ctx = context(tmp_path, "kotlin")
    launcher = tmp_path / "scip-java"
    launcher.write_bytes(b"#!/bin/sh\n")
    monkeypatch.setattr(runners, "tool_path", lambda tool: launcher)
    monkeypatch.setattr(runners, "CACHE", tmp_path / "cache")  # nothing of the shared cache is trimmed or created
    monkeypatch.setattr(runners, "in_container", lambda: False)
    monkeypatch.setenv(runners.SANDBOX_VARIABLE, "1")
    monkeypatch.setattr(runners.shutil, "which", lambda name: "/usr/bin/docker")
    contained = runners.plan_kotlin(ctx)
    monkeypatch.setattr(runners.shutil, "which", lambda name: None)
    blocked = runners.plan_kotlin(ctx)
    monkeypatch.setattr(runners, "in_container", lambda: True)
    in_place = runners.plan_kotlin(ctx)

    assert (
        contained.command and contained.command[:2] == ["/usr/bin/docker", "run"] and runners.SANDBOX_IMAGE in contained.command
    )
    assert not contained.runs_third_party_code
    mounts = [arg for arg in contained.command if ":" in arg and arg.startswith("/")]
    assert not any(str(Path.home()) == arg.split(":")[0] for arg in mounts)
    assert set(contained.env) <= {"PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT"}
    assert blocked.command is None and blocked.blocked.startswith(runners.NEEDS_SANDBOX)
    assert in_place.command and in_place.command[0] != "/usr/bin/docker" and in_place.runs_third_party_code


def test_a_plan_that_runs_third_party_code_here_is_refused_outside_a_container(tmp_path: Path, monkeypatch: pytest.MonkeyPatch):
    corpus = load_corpus()
    repo = next(r for r in corpus.repos if r.language == "kotlin")
    ran = tmp_path / "ran"
    plan = runners.Plan(["touch", str(ran)], {"PATH": os.environ["PATH"]}, runs_third_party_code=True)
    monkeypatch.setitem(runners.PLANNERS, "kotlin", lambda ctx: plan)
    monkeypatch.setattr(runners, "sandboxed", lambda: False)
    monkeypatch.setattr(indexing, "sandboxed", lambda: False)

    refused = indexing._run_indexer(corpus, repo, "a" * 40, tmp_path, tmp_path / "x.scip", tmp_path, {}, False)

    assert not refused.ok and refused.reason.startswith(runners.NEEDS_SANDBOX) and not ran.exists()


def test_run_py_refuses_the_sandbox_variable_where_no_container_is_detected():
    here = Path(__file__).resolve().parents[1]
    env = {"PATH": os.environ["PATH"], "ORACLE_SANDBOX": "1"}
    done = subprocess.run(
        [sys.executable, str(here / "run.py"), "report", "--help"], cwd=here, env=env, capture_output=True, text=True
    )
    in_container = runners.in_container()

    assert (done.returncode == 0) == in_container
    assert in_container or "no container is detected" in done.stderr


def test_gradle_outputs_are_dropped_from_a_snapshot_but_a_source_directory_named_build_is_kept(tmp_path: Path):
    (tmp_path / ".gradle").mkdir()
    (tmp_path / "mod" / "build").mkdir(parents=True)
    (tmp_path / "mod" / "build.gradle.kts").write_text("", encoding="utf-8")
    (tmp_path / "tools" / "build").mkdir(parents=True)
    (tmp_path / "tools" / "build" / "Main.kt").write_text("", encoding="utf-8")

    runners.clean_gradle_outputs(tmp_path)

    assert not (tmp_path / ".gradle").exists() and not (tmp_path / "mod" / "build").exists()
    assert (tmp_path / "tools" / "build" / "Main.kt").exists()


def test_a_missing_tsconfig_is_replaced_by_one_that_includes_javascript(tmp_path: Path):
    runners.cover_everything(tmp_path)

    written = json.loads((tmp_path / "tsconfig.json").read_text())
    assert written["compilerOptions"]["allowJs"] is True and written["include"] == ["**/*"] and "extends" not in written
    assert written["compilerOptions"]["moduleResolution"] == "bundler"


def test_a_narrow_tsconfig_is_kept_beside_the_oracles_and_extended(tmp_path: Path):
    (tmp_path / "tsconfig.json").write_text('{"compilerOptions": {"jsx": "react"}, "include": ["src"]}')

    runners.cover_everything(tmp_path)
    runners.cover_everything(tmp_path)  # a second run leaves the first one's files alone

    assert json.loads((tmp_path / "tsconfig.repo.json").read_text())["include"] == ["src"]
    written = json.loads((tmp_path / "tsconfig.json").read_text())
    assert written["extends"] == "./tsconfig.repo.json" and written["include"] == ["**/*"]
    assert "moduleResolution" not in written["compilerOptions"]


def test_a_solution_style_tsconfig_is_not_extended(tmp_path: Path):
    (tmp_path / "tsconfig.json").write_text('{"files": [], "references": [{"path": "./tsconfig.build.json"}]}')

    runners.cover_everything(tmp_path)

    written = json.loads((tmp_path / "tsconfig.json").read_text())
    assert "extends" not in written and written["compilerOptions"]["moduleResolution"] == "bundler"
    assert (tmp_path / "tsconfig.repo.json").exists()


def test_python_is_indexed_under_a_pyright_config_that_includes_every_file(tmp_path: Path):
    ctx = context(tmp_path, "python")
    ctx.tree.mkdir()
    (ctx.tree / "pyproject.toml").write_text('[tool.pyright]\ninclude = ["src/click"]\n')

    plan = runners.plan_python(ctx)

    assert json.loads((ctx.tree / "pyrightconfig.json").read_text()) == {"include": ["."]}
    assert plan.command is not None and "--environment=" + str(tmp_path / "environment.json") in plan.command


def test_planning_an_index_writes_into_the_exported_tree_and_never_into_the_repository(tmp_path: Path):
    source = tmp_path / "source"
    source.mkdir()
    env = {"PATH": os.environ["PATH"], "GIT_CONFIG_GLOBAL": "/dev/null"}
    for args in (
        ["init", "-q", "-b", "main"],
        ["add", "m.py"],
        ["-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-qm", "c"],
    ):
        if args[0] == "add":
            (source / "m.py").write_text("a = 1\n")
        subprocess.run(["git", *args], cwd=source, env=env, check=True)
    sha = subprocess.run(
        ["git", "rev-parse", "HEAD"], cwd=source, env=env, capture_output=True, text=True, check=True
    ).stdout.strip()
    mirror = source / ".git"
    tree = gitrepo.export_tree(mirror, sha, tmp_path / "trees" / sha)
    ctx = context(tmp_path, "python", name="trees/" + sha)

    runners.plan_python(ctx)

    assert (tree / "pyrightconfig.json").exists()
    listed = subprocess.run(
        ["git", "--git-dir", str(mirror), "ls-tree", "-r", "--name-only", sha],
        env=env,
        capture_output=True,
        text=True,
        check=True,
    )
    assert listed.stdout.split() == ["m.py"]


def test_per_version_gradle_files_are_dropped_past_the_budget_and_the_dependency_cache_stays(tmp_path: Path):
    for path in ("wrapper/dists/gradle-9.1-bin/x", "caches/9.1/y", "caches/modules-2/z", "jdks/j"):
        (tmp_path / path).parent.mkdir(parents=True, exist_ok=True)
        (tmp_path / path).write_bytes(b"0" * 100)

    runners.trim_gradle_home(tmp_path, budget=1000)
    assert (tmp_path / "wrapper/dists/gradle-9.1-bin/x").exists()

    runners.trim_gradle_home(tmp_path, budget=150)
    assert not (tmp_path / "wrapper/dists/gradle-9.1-bin").exists() and not (tmp_path / "caches/9.1").exists()
    assert (tmp_path / "caches/modules-2/z").exists() and (tmp_path / "jdks/j").exists()


@pytest.mark.skipif(shutil.which("go") is None, reason="needs the Go toolchain")
def test_go_fetches_modules_offline_of_the_repository_code_and_indexes_with_the_proxy_off(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(runners, "CACHE", tmp_path / "cache")
    ctx = context(tmp_path, "go")
    ctx.tree.mkdir()
    (ctx.tree / "go.mod").write_text("module example.invalid/m\n\ngo 1.21\n")
    (ctx.tree / "m.go").write_text("package m\n\nfunc F() {}\n")

    plan = runners.plan_go(ctx)

    assert plan.command is not None and plan.command[1] == "index" and "--module-version" in plan.command
    assert plan.env["GOPROXY"] == "off" and plan.env["CGO_ENABLED"] == "0" and plan.env["GOFLAGS"] == "-mod=readonly"
    assert not plan.runs_third_party_code
    broken = context(tmp_path, "go", name="broken")
    broken.tree.mkdir()
    (broken.tree / "go.mod").write_text("module example.invalid/b\n\ngo 1.21\n\nrequire example.invalid/missing v1.0.0\n")
    refused = runners.plan_go(broken)
    assert refused.command is None and refused.blocked.startswith("dependency fetch failed: go mod download")


def test_a_java_project_is_built_by_maven_in_the_maven_image_and_by_gradle_in_the_jdk_image_never_on_the_host(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    launcher = tmp_path / "scip-java"
    launcher.write_bytes(b"#!/bin/sh\n")
    monkeypatch.setattr(runners, "tool_path", lambda tool: launcher)
    monkeypatch.setattr(runners, "CACHE", tmp_path / "cache")
    monkeypatch.setattr(runners, "in_container", lambda: False)
    monkeypatch.setattr(runners.shutil, "which", lambda name: "/usr/bin/docker")
    plans = {}
    for name, files in (("maven", ["pom.xml"]), ("gradle", ["build.gradle.kts"]), ("both", ["pom.xml", "settings.gradle"])):
        ctx = context(tmp_path, "java", name=name)
        ctx.tree.mkdir()
        for file in files:
            (ctx.tree / file).write_text("")
        plans[name] = runners.plan_java(ctx)

    assert runners.MAVEN_IMAGE in plans["maven"].command and "--build-tool=maven" in plans["maven"].command
    for name in ("gradle", "both"):
        assert runners.SANDBOX_IMAGE in plans[name].command and "--build-tool=gradle" in plans[name].command
    assert all(plan.command[:2] == ["/usr/bin/docker", "run"] and not plan.runs_third_party_code for plan in plans.values())


@pytest.mark.skipif(shutil.which("go") is None, reason="needs the Go toolchain")
def test_a_go_module_with_no_requirement_has_nothing_to_download_and_is_not_a_failed_fetch(tmp_path: Path, monkeypatch):
    monkeypatch.setattr(runners, "CACHE", tmp_path / "cache")
    ctx = context(tmp_path, "go", name="bare")
    ctx.tree.mkdir()
    (ctx.tree / "go.mod").write_text("module example.invalid/bare\n\ngo 1.21\n")
    (ctx.tree / "m.go").write_text("package m\n")

    assert runners.plan_go(ctx).command is not None


def test_go_files_in_directories_the_go_tool_ignores_are_not_present_code_files(tmp_path: Path):
    from indexing import present_code_files

    for path in ("a.go", "pkg/b.go", "_examples/main.go", "pkg/_gen/c.go", "testdata/d.go", ".hidden/e.go", "pkg/_x.go"):
        (tmp_path / path).parent.mkdir(parents=True, exist_ok=True)
        (tmp_path / path).write_text("package a\n")

    assert present_code_files(tmp_path, "go") == {"a.go", "pkg/b.go", "pkg/_x.go"}


def test_a_failed_indexer_leaves_its_output_beside_the_slot_not_only_the_last_400_characters(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
):
    corpus = load_corpus()
    repo = next(r for r in corpus.repos if r.language == "go")
    script = "echo first-line >&2; i=0; while [ $i -lt 100 ]; do echo padding-$i; i=$((i+1)); done; echo last-line >&2; exit 3"
    plan = runners.Plan(["sh", "-c", script], {"PATH": os.environ["PATH"]})
    monkeypatch.setitem(runners.PLANNERS, "go", lambda ctx: plan)
    slot = tmp_path / "slot"

    failed = indexing._run_indexer(corpus, repo, "a" * 40, tmp_path, slot / "index.scip", tmp_path, {}, False)

    assert not failed.ok and failed.reason.startswith("indexer exited 3") and "first-line" not in failed.reason
    assert "first-line" in (slot / indexing.FAILURE_LOG).read_text()
