"""Whether the oracle's indexers can index a repository, judged from its root listing and a few build files alone.

No clone: `seeds.py` reads the files named by `files_to_read` through the hosting API and asks `reject` for the reason
a repository is out, or None. A check that cannot be settled from those files rejects the repository (a set drawn from
what is provable is smaller and cannot burn on an index nobody could build). The JVM rules are the strictest: JVM
indexing broke three times, each on a build the oracle's toolchains do not serve (see `jvm-toolchains.toml`).
"""

from __future__ import annotations

import re
from collections.abc import Mapping

GO_MAX_MINOR = 24  # the `go` directive the runner's own toolchain serves (GOTOOLCHAIN=local, no download)
GRADLE_MIN = (7, 3)  # the first Gradle that runs on JDK 17, the oldest runtime the oracle gives an older wrapper
SUPPORTED_JDKS = frozenset({8, 11, 17, 21})  # jvm-toolchains.toml's three, and the JDK 21 the container itself runs
GRADLE_FILES = ("build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts")
GRADLE_WRAPPER = "gradle/wrapper/gradle-wrapper.properties"
VERSION_CATALOG = "gradle/libs.versions.toml"
JVM_LANGUAGES = ("kotlin", "java")

# A build that needs more than a JDK: Android and native targets, a compiler that fails on a warning, a plugin that
# refuses the oracle's flags (gson: Error Prone; javapoet: enforcer rules), an ASF parent whose build produced no index.
JVM_BANNED = (
    "errorprone",
    "error_prone",
    "maven-enforcer-plugin",
    "-Werror",
    "allWarningsAsErrors = true",
    "com.android",
    "org.jetbrains.intellij",
)
JVM_BANNED_PATTERNS = (r"allWarningsAsErrors\.set\(true\)", r"multiplatform", r"kotlin\(\"native\"\)")
JVM_BANNED_DIRECTORIES = ("android", "androidApp", "iosApp", "ios", "native")
BANNED_PARENTS = ("commons-parent",)
MODULE_LIMIT = 12  # the Gradle subprojects whose build files are read beside the root's

LEVEL_PATTERNS = (
    r"jvmToolchain\(\s*(\d+)\s*\)",
    r"JavaLanguageVersion\.of\(\s*(\d+)\s*\)",
    r"JavaVersion\.VERSION_(\d+(?:_\d+)?)",
    r"(?:sourceCompatibility|targetCompatibility)\s*=?\s*['\"]?(\d+(?:\.\d+)?)",
    r"<maven\.compiler\.(?:source|target|release)>\s*(\d+(?:\.\d+)?)\s*<",
    r"<(?:release|source|target)>\s*(\d+(?:\.\d+)?)\s*<",
    r"<(?:java|jdk|javaVersion)\.version>\s*(\d+(?:\.\d+)?)\s*<",
    r"<jdkToolchain>\s*<version>\s*(\d+(?:\.\d+)?)\s*<",
    r"^\s*(?:jdk|java|jvm)\w*\s*=\s*\"(\d+(?:\.\d+)?)\"",
)


def java_level(text: str) -> int:
    """`1.8` and `1_8` are 8; `17` is 17."""
    parts = re.split(r"[._]", text)
    return int(parts[1]) if parts[0] == "1" and len(parts) > 1 else int(parts[0])


def levels(text: str) -> set[int]:
    return {java_level(m) for pattern in LEVEL_PATTERNS for m in re.findall(pattern, text, flags=re.MULTILINE)}


def gradle_modules(files: Mapping[str, str | None]) -> list[str]:
    """The build files of the subprojects `settings.gradle(.kts)` includes (an Android app or a multiplatform library
    names its plugin there, not in the root build), at most `MODULE_LIMIT` projects, one `include` line at a time."""
    settings = "\n".join(text for name, text in files.items() if name.startswith("settings.gradle") and text)
    lines = re.findall(r"^\s*include\s*\(?([^\n]*)", settings, flags=re.MULTILINE)
    projects = [m.strip(":").replace(":", "/") for line in lines for m in re.findall(r"[\"']([^\"']+)[\"']", line)]
    return [
        f"{name}/{build}"
        for name in list(dict.fromkeys(projects))[:MODULE_LIMIT]
        for build in ("build.gradle.kts", "build.gradle")
    ]


def files_to_read(language: str, root: frozenset[str]) -> list[str]:
    """The root files and paths this language's verdict reads, among the ones the repository has."""
    if language == "python":
        return []
    if language == "typescript":
        return ["package.json"] * ("package.json" in root)
    if language == "go":
        return ["go.mod"] * ("go.mod" in root)
    if language == "rust":
        return [name for name in ("rust-toolchain", "rust-toolchain.toml") if name in root]
    if language not in JVM_LANGUAGES:
        raise ValueError(f"no feasibility rule for {language}")
    gradle = [name for name in GRADLE_FILES if name in root]
    if gradle:
        return [*gradle, GRADLE_WRAPPER, VERSION_CATALOG]
    return ["pom.xml"] * ("pom.xml" in root)


def reject(language: str, root: frozenset[str], files: Mapping[str, str | None]) -> str | None:
    """Why the oracle cannot index this repository, or None. `files` maps each path of `files_to_read` to its text, or
    None where the repository has no such file."""
    if language == "python":
        return None if root & {"pyproject.toml", "setup.py", "setup.cfg"} else "no Python package file at the root"
    if language == "typescript":
        text = files.get("package.json")
        if text is None:
            return "no package.json at the root"
        if "pnpm-workspace.yaml" in root or "workspace:" in text:
            return "pnpm workspace: `npm install` cannot resolve it"
        return None
    if language == "go":
        return _go(root, files.get("go.mod"))
    if language == "rust":
        if "Cargo.toml" not in root or "Cargo.lock" not in root:
            return "no Cargo.toml and Cargo.lock at the root (`cargo fetch --locked`)"
        if any("nightly" in (text or "") for text in files.values()):
            return "pins a nightly toolchain"
        return None
    return _jvm(language, root, files)


def _go(root: frozenset[str], text: str | None) -> str | None:
    if text is None:
        return "no go.mod at the root"
    if "go.work" in root:
        return "a Go workspace: GOWORK=off leaves its modules out"
    found = re.search(r"^go\s+1\.(\d+)", text, flags=re.MULTILINE)
    if found is None:
        return "go.mod names no Go 1.x version"
    return f"needs Go 1.{found.group(1)}, above the runner's 1.{GO_MAX_MINOR}" if int(found.group(1)) > GO_MAX_MINOR else None


def _jvm(language: str, root: frozenset[str], files: Mapping[str, str | None]) -> str | None:
    gradle = any(name in root for name in GRADLE_FILES)
    texts = {name: text for name, text in files.items() if text is not None}
    if not gradle and "pom.xml" not in texts:
        return "no Gradle or Maven build at the root"
    if language == "kotlin" and not gradle:
        return "a Kotlin repository without a Gradle build"
    blob = "\n".join(texts.values())
    banned = next((token for token in JVM_BANNED if token in blob), None)
    banned = banned or next((p for p in JVM_BANNED_PATTERNS if re.search(p, blob, flags=re.IGNORECASE)), None)
    if banned:
        return f"the build names `{banned}`"
    if root & set(JVM_BANNED_DIRECTORIES):
        return "an Android or native module at the root"
    wrong = sorted(level for level in levels(blob) if level not in SUPPORTED_JDKS)
    if wrong:
        return f"asks for Java {wrong}, which the oracle has no JDK for (8, 11, 17, 21)"
    if gradle:
        return _gradle(texts)
    return _maven(texts["pom.xml"])


def _gradle(texts: Mapping[str, str]) -> str | None:
    wrapper = texts.get(GRADLE_WRAPPER)
    found = re.search(r"gradle-(\d+)\.(\d+)", wrapper or "")
    if found is None:
        return "no Gradle wrapper naming its version"
    return "wrapper Gradle is older than 7.3, which cannot run on JDK 17" if (int(found[1]), int(found[2])) < GRADLE_MIN else None


def _maven(pom: str) -> str | None:
    parent = re.search(r"<parent>(.*?)</parent>", pom, flags=re.DOTALL)
    if parent:
        block = parent.group(1)
        if re.search(r"<version>[^<]*-SNAPSHOT</version>", block):
            return "a SNAPSHOT parent: it is not published"
        if any(f"<artifactId>{name}</artifactId>" in block for name in BANNED_PARENTS):
            return "an ASF commons parent: its build produced no index"
    return None
