import base64
import datetime
import tomllib
from pathlib import Path

import pytest

import corpus
import feasibility
import seeds
from corpus import LANGUAGES, load_corpus
from seeds import Response

AS_OF = datetime.date(2026, 11, 27)


def entry(name: str, feasible: bool = True) -> dict:
    return {
        "full_name": name,
        "url": f"https://github.com/{name}.git",
        "license": "MIT",
        "stars": 1,
        "size_kb": 1,
        "pushed_at": "2026-10-01T00:00:00Z",
        "feasible": feasible,
        "reason": "",
    }


def pools(per_language: int = 12) -> dict:
    return {
        "as_of": "2026-11-27",
        "languages": {lang: [entry(f"o{n}/{lang}-{n}", n % 5 != 0) for n in range(per_language)] for lang in LANGUAGES},
    }


COUNTS = {lang: 2 for lang in LANGUAGES}


def names(chosen: dict) -> set[str]:
    return {e["full_name"] for group in chosen.values() for e in group}


def test_the_draw_is_a_function_of_the_seed_and_the_names_alone():
    first = seeds.draw_set(pools(), 4242, COUNTS, set())

    assert first == seeds.draw_set(pools(), 4242, COUNTS, set())
    assert first != seeds.draw_set(pools(), 4243, COUNTS, set())
    reordered = pools()
    for group in reordered["languages"].values():
        group.reverse()
    assert seeds.draw_set(reordered, 4242, COUNTS, set()) == first


def test_the_order_is_the_documented_hash_of_the_seed_and_the_full_name():
    chosen = seeds.draw_set(pools(), 7, {"python": 3}, set())["python"]

    expected = sorted(
        (e["full_name"] for e in pools()["languages"]["python"] if e["feasible"]), key=lambda n: seeds.order_key(7, n)
    )
    assert [e["full_name"] for e in chosen] == expected[:3]


def test_sets_drawn_in_turn_never_share_a_repository_and_only_feasible_ones_are_drawn():
    taken: set[str] = set()
    drawn = []
    for seed in (11, 22, 33):
        chosen = seeds.draw_set(pools(), seed, COUNTS, taken)
        assert all(e["feasible"] for group in chosen.values() for e in group)
        drawn.append(names(chosen))
        taken |= drawn[-1]

    assert sum(len(d) for d in drawn) == len(set().union(*drawn)) == 3 * 2 * len(LANGUAGES)


def test_a_pool_that_cannot_fill_a_set_stops_the_draw_instead_of_shrinking_it():
    with pytest.raises(SystemExit, match="feasible repositories left"):
        seeds.draw_set(pools(per_language=2), 1, COUNTS, set())


class Github:
    """A fake of the few GETs the pool makes; a path it was not given answers 404, the way the API does."""

    def __init__(self, search: list[dict], routes: dict[str, Response] | None = None) -> None:
        self.search, self.routes, self.paths = search, routes or {}, []

    def __call__(self, path: str) -> Response:
        self.paths.append(path)
        if path.startswith("search/repositories"):
            page = int(path.rsplit("page=", 1)[1])
            return Response({"items": self.search[(page - 1) * seeds.PAGE : page * seeds.PAGE]}, {})
        return self.routes.get(path, Response(None, {"status": "404"}))


def repository(name: str, stars: int, **changes) -> dict:
    item = {
        "full_name": name,
        "owner": {"login": name.split("/")[0]},
        "fork": False,
        "archived": False,
        "language": "Python",
        "size": 500,
        "pushed_at": "2026-06-01T00:00:00Z",
        "stargazers_count": stars,
        "license": {"spdx_id": "MIT"},
    }
    return item | changes


def test_the_pool_keeps_what_the_rule_keeps_and_ranks_it_by_stars():
    items = [
        repository("a/small", 10),
        repository("b/big", 99),
        repository("c/forked", 500, fork=True),
        repository("d/archived", 500, archived=True),
        repository("e/stale", 500, pushed_at="2025-11-26T00:00:00Z"),
        repository("f/huge", 500, size=seeds.SIZE_MAX_KB + 1),
        repository("g/copyleft", 500, license={"spdx_id": "GPL-3.0"}),
        repository("h/other-language", 500, language="Go"),
        repository("pallets/in-corpus", 500),
        repository("i/pushed-on-the-edge", 50, pushed_at="2025-11-27T00:00:00Z"),
    ]

    found = seeds.pool(Github(items), "python", AS_OF, {"pallets"})

    assert [c["full_name"] for c in found] == ["b/big", "i/pushed-on-the-edge", "a/small"]


def test_a_dual_licence_is_read_from_the_licence_files_when_the_api_names_none():
    listing = [{"name": "LICENSE-MIT", "type": "file"}, {"name": "LICENSE-APACHE", "type": "file"}]
    api = Github([], {"repos/x/dual/contents/": Response(listing, {}), "repos/x/odd/contents/": Response([], {})})

    assert seeds.licence_of(api, {"full_name": "x/dual", "license": {"spdx_id": "NOASSERTION"}}) == "Apache-2.0 OR MIT"
    assert seeds.licence_of(api, {"full_name": "x/odd", "license": None}) is None
    assert seeds.permissive("Apache-2.0 OR MIT") and not seeds.permissive("MIT OR GPL-2.0") and not seeds.permissive(None)


def test_the_pool_is_cut_at_its_size():
    items = [repository(f"o{n}/r{n}", 1000 - n) for n in range(seeds.POOL_SIZE + 25)]

    found = seeds.pool(Github(items), "python", AS_OF, set())

    assert len(found) == seeds.POOL_SIZE and found[0]["full_name"] == "o0/r0"


def test_a_language_reads_further_tranches_until_enough_are_feasible(monkeypatch):
    monkeypatch.setattr(seeds, "POOL_SIZE", 10)
    monkeypatch.setattr(seeds, "POOL_LIMIT", 40)
    items = [repository(f"o{n}/r{n}", 1000 - n, language="Kotlin") for n in range(35)]
    judged: list[str] = []

    def judge(api, language, candidate, history):
        judged.append(candidate["full_name"])
        return None if candidate["stars"] in (990, 975, 971) else "out"

    monkeypatch.setattr(seeds, "judge", judge)

    found = seeds.read_pool(Github(items), "kotlin", AS_OF, set(), 800, wanted=3)

    assert [e["full_name"] for e in found if e["feasible"]] == ["o10/r10", "o25/r25", "o29/r29"]
    assert len(found) == 30 and sum(e["feasible"] for e in found) == 3
    assert len(judged) == len(set(judged)) == 30


def test_a_language_that_never_has_enough_stops_at_the_limit(monkeypatch):
    monkeypatch.setattr(seeds, "POOL_SIZE", 10)
    monkeypatch.setattr(seeds, "POOL_LIMIT", 30)
    monkeypatch.setattr(seeds, "judge", lambda *args: "out")

    found = seeds.read_pool(
        Github([repository(f"o{n}/r{n}", 1000 - n, language="Go") for n in range(100)]), "go", AS_OF, set(), 800, 6
    )

    assert len(found) == 30 and not any(e["feasible"] for e in found)


def test_the_commit_count_is_the_last_page_of_a_listing_of_one_per_page():
    link = '<https://api.github.com/r/commits?per_page=1&page=2>; rel="next", <https://api.github.com/r/commits?per_page=1&page=2381>; rel="last"'
    api = Github(
        [], {"repos/x/y/commits?per_page=1": Response([{}], {"link": link}), "repos/x/z/commits?per_page=1": Response([{}], {})}
    )

    assert seeds.commit_count(api, "x/y") == 2381
    assert seeds.commit_count(api, "x/z") == 1
    assert seeds.commit_count(api, "x/missing") is None


def text_response(text: str) -> Response:
    return Response({"encoding": "base64", "content": base64.b64encode(text.encode()).decode()}, {})


def test_a_repository_with_too_little_history_is_out_before_its_files_are_read():
    api = Github([], {"repos/x/y/commits?per_page=1": Response([{}], {})})

    assert "1 commits" in seeds.judge(api, "go", entry("x/y"), 800)
    assert api.paths == ["repos/x/y/commits?per_page=1"]


def test_judge_reads_the_root_and_the_files_the_language_needs():
    link = '<https://x/c?per_page=1&page=900>; rel="last"'
    api = Github(
        [],
        {
            "repos/x/y/commits?per_page=1": Response([{}], {"link": link}),
            "repos/x/y/contents/": Response([{"name": "go.mod"}], {}),
            "repos/x/y/contents/go.mod": text_response("module x\n\ngo 1.30\n"),
        },
    )

    assert "Go 1.30" in seeds.judge(api, "go", entry("x/y"), 800)


def test_python_needs_a_package_file_and_typescript_a_package_json_without_a_pnpm_workspace():
    assert feasibility.reject("python", frozenset({"setup.py"}), {}) is None
    assert feasibility.reject("python", frozenset({"README.md"}), {}) is not None
    assert feasibility.reject("typescript", frozenset({"package.json"}), {"package.json": "{}"}) is None
    assert feasibility.reject("typescript", frozenset(), {"package.json": None}) is not None
    assert feasibility.reject("typescript", frozenset({"package.json"}), {"package.json": '{"a": "workspace:*"}'}) is not None


def test_go_needs_a_root_module_within_the_runner_toolchain_and_no_workspace():
    ok = f"module x\n\ngo 1.{feasibility.GO_MAX_MINOR}\n"
    assert feasibility.reject("go", frozenset({"go.mod"}), {"go.mod": ok}) is None
    assert feasibility.reject("go", frozenset({"go.mod"}), {"go.mod": ok.replace("1.24", "1.25")}) is not None
    assert feasibility.reject("go", frozenset({"go.mod", "go.work"}), {"go.mod": ok}) is not None
    assert feasibility.reject("go", frozenset(), {"go.mod": None}) is not None


def test_rust_needs_a_committed_lock_file_and_no_nightly_pin():
    assert feasibility.reject("rust", frozenset({"Cargo.toml", "Cargo.lock"}), {}) is None
    assert feasibility.reject("rust", frozenset({"Cargo.toml"}), {}) is not None
    pinned = {"rust-toolchain.toml": '[toolchain]\nchannel = "nightly-2026-01-01"'}
    assert feasibility.reject("rust", frozenset({"Cargo.toml", "Cargo.lock", "rust-toolchain.toml"}), pinned) is not None


GRADLE_ROOT = frozenset({"build.gradle.kts", "settings.gradle.kts", "gradle"})
WRAPPER = "distributionUrl=https\\://services.gradle.org/distributions/gradle-8.14-bin.zip"


def gradle_files(build: str, wrapper: str | None = WRAPPER) -> dict:
    return {
        "build.gradle.kts": build,
        "settings.gradle.kts": "",
        feasibility.GRADLE_WRAPPER: wrapper,
        feasibility.VERSION_CATALOG: None,
    }


@pytest.mark.parametrize(
    ("build", "wrapper", "refused"),
    [
        ("kotlin { jvmToolchain(17) }", WRAPPER, False),
        ("kotlin { jvmToolchain(11) }", WRAPPER, False),
        ("kotlin { jvmToolchain(25) }", WRAPPER, True),
        ("kotlin { jvmToolchain(19) }", WRAPPER, True),
        ("java { sourceCompatibility = JavaVersion.VERSION_1_8 }", WRAPPER, False),
        ("java { toolchain { languageVersion.set(JavaLanguageVersion.of(23)) } }", WRAPPER, True),
        ("tasks { compileKotlin { kotlinOptions.allWarningsAsErrors = true } }", WRAPPER, True),
        ('plugins { id("net.ltgt.errorprone") }', WRAPPER, True),
        ('plugins { kotlin("multiplatform") }', WRAPPER, True),
        ('plugins { id("com.android.library") }', WRAPPER, True),
        ("kotlin { jvmToolchain(17) }", None, True),
        ("kotlin { jvmToolchain(17) }", "distributionUrl=gradle-6.9-bin.zip", True),
    ],
)
def test_a_gradle_kotlin_build_is_refused_for_what_the_oracles_toolchains_do_not_serve(build, wrapper, refused):
    reason = feasibility.reject("kotlin", GRADLE_ROOT, gradle_files(build, wrapper))

    assert (reason is not None) is refused, reason


def test_the_version_catalog_can_name_the_jdk():
    files = gradle_files("kotlin { }") | {feasibility.VERSION_CATALOG: '[versions]\njdk = "24"\n'}

    assert "24" in feasibility.reject("kotlin", GRADLE_ROOT, files)


def test_a_kotlin_repository_must_build_with_gradle():
    assert feasibility.reject("kotlin", frozenset({"pom.xml"}), {"pom.xml": "<project/>"}) is not None


def pom(properties: str = "", parent: str = "", plugins: str = "") -> dict:
    return {"pom.xml": f"<project>{parent}<properties>{properties}</properties>{plugins}</project>"}


@pytest.mark.parametrize(
    ("files", "refused"),
    [
        (pom("<maven.compiler.release>17</maven.compiler.release>"), False),
        (pom("<maven.compiler.source>1.8</maven.compiler.source><maven.compiler.target>1.8</maven.compiler.target>"), False),
        (pom(), False),
        (pom("<maven.compiler.release>24</maven.compiler.release>"), True),
        (pom("<maven.compiler.source>1.7</maven.compiler.source>"), True),
        (pom(plugins="<artifactId>maven-enforcer-plugin</artifactId>"), True),
        (pom(plugins="<compilerArgs><arg>-Werror</arg></compilerArgs>"), True),
        (pom(parent="<parent><artifactId>p</artifactId><version>3.0-SNAPSHOT</version></parent>"), True),
        (pom(parent="<parent><artifactId>p</artifactId><version>3.0</version></parent>"), False),
        (pom(parent="<parent><artifactId>commons-parent</artifactId><version>90</version></parent>"), True),
        (pom(plugins="<jdkToolchain><version>25</version></jdkToolchain>"), True),
        (pom(plugins="<jdkToolchain><version>11</version></jdkToolchain>"), False),
    ],
)
def test_a_maven_java_build_is_refused_for_what_broke_the_oracle_before(files, refused):
    reason = feasibility.reject("java", frozenset({"pom.xml"}), files)

    assert (reason is not None) is refused, reason


def test_a_java_repository_with_neither_build_is_out():
    assert feasibility.reject("java", frozenset({"README.md"}), {}) is not None


def test_the_levels_a_build_names_are_normalised():
    assert feasibility.levels("sourceCompatibility = '1.8'\nJavaLanguageVersion.of(21)\nJavaVersion.VERSION_11") == {8, 21, 11}


def test_a_seed_file_loads_through_extends_with_its_own_seed_and_repositories(tmp_path: Path):
    chosen = {lang: [entry(f"o/{lang}-1"), entry(f"o/{lang}-2")] for lang in LANGUAGES}
    pins = {e["full_name"]: "a" * 40 for group in chosen.values() for e in group}
    text = seeds.seed_file(2, 99, "2026-11-27", True, "2026-11-27", chosen, "2026-11-27", pins)
    written = corpus.ROOT / "seeds" / "seed-test-fixture.toml"
    written.parent.mkdir(exist_ok=True)
    written.write_text(text, encoding="utf-8")
    try:
        loaded = load_corpus(written)
    finally:
        written.unlink()

    base = load_corpus(corpus.ROOT / "corpus.toml")
    assert tomllib.loads(text)["holdout"] is True
    assert loaded.holdout and not base.holdout and loaded.label == "seed-test-fixture"
    assert loaded.sampling.seed == 99 and loaded.sampling.language_commits == base.sampling.language_commits
    assert loaded.tools == base.tools and loaded.spent == {}
    assert [r.name for r in loaded.repos if r.language == "go"] == ["o--go-1", "o--go-2"]
    assert [r.name for r in loaded.repos if r.fast] == [f"o--{lang}-1" for lang in LANGUAGES]
    assert all(r.sha == "a" * 40 and r.url.endswith(".git") for r in loaded.repos)


def test_the_subprojects_a_settings_file_includes_are_read_for_the_plugins_the_root_build_does_not_name():
    settings = {"settings.gradle.kts": 'rootProject.name = "x"\ninclude(":app", ":libs:core")\ninclude "docs"\n'}

    assert feasibility.gradle_modules(settings) == [
        "app/build.gradle.kts",
        "app/build.gradle",
        "libs/core/build.gradle.kts",
        "libs/core/build.gradle",
        "docs/build.gradle.kts",
        "docs/build.gradle",
    ]
    many = {"settings.gradle": "\n".join(f"include '{n}'" for n in range(30))}
    assert len(feasibility.gradle_modules(many)) == 2 * feasibility.MODULE_LIMIT
    assert feasibility.gradle_modules({"settings.gradle": None}) == []


def test_an_android_app_module_or_an_intellij_plugin_refuses_a_kotlin_build():
    root = gradle_files("kotlin { jvmToolchain(17) }")

    assert feasibility.reject("kotlin", GRADLE_ROOT, root | {"app/build.gradle.kts": 'plugins { id("com.android.application") }'})
    assert feasibility.reject("kotlin", GRADLE_ROOT, root | {"build.gradle.kts": 'id("org.jetbrains.intellij.platform")'})
    assert feasibility.reject("kotlin", GRADLE_ROOT, root | {"core/build.gradle.kts": "plugins { `java-library` }"}) is None


def test_judge_reads_the_included_subprojects_of_a_jvm_build():
    link = '<https://x/c?per_page=1&page=900>; rel="last"'
    root = [{"name": name} for name in ("build.gradle.kts", "settings.gradle.kts")]
    api = Github(
        [],
        {
            "repos/x/y/commits?per_page=1": Response([{}], {"link": link}),
            "repos/x/y/contents/": Response(root, {}),
            "repos/x/y/contents/build.gradle.kts": text_response("kotlin { jvmToolchain(17) }"),
            "repos/x/y/contents/settings.gradle.kts": text_response('include(":app")'),
            "repos/x/y/contents/gradle/wrapper/gradle-wrapper.properties": text_response(WRAPPER),
            "repos/x/y/contents/app/build.gradle.kts": text_response('plugins { id("com.android.application") }'),
        },
    )

    assert "com.android" in seeds.judge(api, "kotlin", entry("x/y"), 800)


def test_a_replacement_needs_a_pool_name_and_a_reason():
    plan = {"set": [{"number": 2, "seed": 1, "replaced": [{"name": "o1/python-1", "reason": " "}]}]}
    with pytest.raises(SystemExit, match="replaced without"):
        seeds.replaced_names(pools(), plan)
    with pytest.raises(SystemExit, match="replaced without"):
        seeds.replaced_names(
            pools(), {"set": [{"number": 2, "seed": 1, "replaced": [{"name": "x/unknown", "reason": "timeout"}]}]}
        )
    assert seeds.replaced_names(pools(), {"set": [{"number": 2, "seed": 1}]}) == set()
