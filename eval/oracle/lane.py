"""One command that prepares and measures a lane: `python run.py lane --lane jvm --repos A B --job DIR --out DIR`.

What the cluster used to spell in its workflow template (JDKs, Maven, the toolchain files, the loop over budgets and
repositories) lives here, so the template names only an image, resources and this command. The JVM lane prepares its
toolchains only inside a container, the one place its builds run in place.
"""

from __future__ import annotations

import argparse
import os
import pwd
import shutil
import subprocess
import sys
from pathlib import Path

import jvmtoolchains
from corpus import CACHE, load_corpus
from indexing import setup_tools
from runners import JDK_HOMES_VARIABLE, SANDBOX_VARIABLE, in_container

MEASURE = Path(__file__).with_name("measure.py")
BUDGETS = ("default", "uncapped")


def prepare_environment(lane: str, job: Path, shared_index: Path | None) -> dict[str, str]:
    """The environment `measure.py` runs in, after the lane's files are in place.

    Inside a container the JVM builds run in place, on toolchains under `job`. On a machine with Docker they run in a
    throwaway container, and a runner that already has the JDKs (`actions/setup-java`) hands them to it by mount.
    """
    env = dict(os.environ)
    if shared_index is not None and not (CACHE / "index").exists():
        CACHE.mkdir(parents=True, exist_ok=True)
        (CACHE / "index").symlink_to(shared_index)
    if lane != "jvm":
        return env
    if in_container():
        prepared = jvmtoolchains.prepare(job, CACHE / "gradle-home", job / "m2", jvmtoolchains.default_java_home(), env=env)
        jvmtoolchains.claim_local_repository(job / "m2", Path(pwd.getpwuid(os.getuid()).pw_dir))
        env[SANDBOX_VARIABLE] = "1"
        if prepared.maven_bin:
            env["PATH"] = f"{prepared.maven_bin}{os.pathsep}{env['PATH']}"
    elif jvmtoolchains.runner_jdks(jvmtoolchains.load_pins(), env) is not None:
        prepared = jvmtoolchains.prepare(
            job, CACHE / "gradle-home", CACHE / "maven-home", jvmtoolchains.default_java_home(), env=env, maven=False
        )
        env[JDK_HOMES_VARIABLE] = os.pathsep.join(str(home) for home in prepared.jdk_homes)
    else:
        print("no container and no JDKs on this machine: the JVM builds run in a container with the JDK it ships", flush=True)
    return env


def measure_command(repo: str, budget: str, out: Path, script: Path = MEASURE, fast: bool = False) -> list[str]:
    return [
        sys.executable,
        str(script),
        "--repo",
        repo,
        *(["--budget", "uncapped"] if budget == "uncapped" else []),
        "--append",
        "--out",
        str(out),
    ]


def run_lane(
    lane: str,
    repos: list[str],
    out: Path,
    env: dict[str, str],
    script: Path = MEASURE,
    budgets: tuple[str, ...] = BUDGETS,
    fast: bool = False,
) -> int:
    """Measure each repository once per budget into `out/<budget>/<lane>`; 1 when any repository failed, the rest still run."""
    failed = 0
    for budget in budgets:
        target = out / budget / lane
        (out / budget).mkdir(parents=True, exist_ok=True)
        for repo in repos:
            print(f"=== measure {budget} {repo} ===", flush=True)
            done = subprocess.run(
                measure_command(repo, budget, target, script, fast), env=env, stdin=subprocess.DEVNULL, check=False
            )
            if done.returncode != 0:
                print(f"FAILED {budget} {repo}", flush=True)
                failed = 1
            for scratch in ("trees", "repos"):
                shutil.rmtree(CACHE / scratch, ignore_errors=True)
    print(f"=== lane {lane} done, failed={failed} ===", flush=True)
    return failed


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="lane", description=__doc__)
    parser.add_argument("--lane", required=True, help="the language lane: its name is the directory under each budget")
    parser.add_argument("--repos", nargs="+", required=True)
    parser.add_argument("--job", type=Path, required=True, help="the claim that keeps downloaded toolchains between runs")
    parser.add_argument("--out", type=Path, required=True, help="the round's directory: <out>/<budget>/<lane>")
    parser.add_argument("--budgets", nargs="+", choices=BUDGETS, default=list(BUDGETS))
    parser.add_argument("--fast", action="store_true", help="the fast subset's commit count")
    parser.add_argument("--shared-index", type=Path, help="an index cache shared between rounds, linked into the oracle cache")
    options = parser.parse_args(argv)
    corpus = load_corpus()
    setup_tools(corpus, {repo.language for repo in corpus.select(options.repos)})
    env = prepare_environment(options.lane, options.job, options.shared_index)
    return run_lane(options.lane, options.repos, options.out, env, budgets=tuple(options.budgets), fast=options.fast)
