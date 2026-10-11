// Kotlin through the shared syntax facts and the Kotlin binding model: each
// positive relation paired with a decoy that spells the same name and must
// not bind (#303). These run the binary on real git repositories, the way
// the hook and `--symbol` do.

use std::path::Path;
use std::process::Command;

use serde_json::Value;
use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(repo: &Path, rel: &str, content: &str) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn repo_of(files: &[(&str, &str)], changes: &[(&str, &str)]) -> TempDir {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path();
    git(repo, &["init", "-q", "."]);
    git(repo, &["config", "user.email", "test@example.com"]);
    git(repo, &["config", "user.name", "diffctx tests"]);
    for (p, c) in files {
        write(repo, p, c);
    }
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "initial"]);
    if !changes.is_empty() {
        for (p, c) in changes {
            write(repo, p, c);
        }
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-q", "-m", "change"]);
    }
    tmp
}

fn symbol(repo: &Path, query: &str) -> Value {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--symbol", query, "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

fn impact(repo: &Path) -> Value {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", "HEAD~1..HEAD", "--mode", "impact", "-q"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

/// `path::caller-symbol evidence relation` of every caller of the first
/// changed definition named `name`.
fn callers(doc: &Value, name: &str) -> Vec<String> {
    let Some(sym) = doc["changed"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["symbol"] == name))
    else {
        return Vec::new();
    };
    let mut out: Vec<String> = sym["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            format!(
                "{}::{} {} {}",
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap_or("-"),
                c["evidence"].as_str().unwrap(),
                c["relation"].as_str().unwrap_or("call"),
            )
        })
        .collect();
    out.sort();
    out
}

const REPO_KT: &str =
    "package app.data\n\ninterface Repo {\n    fun findLiked(limit: Int): List<Int>\n}\n";
const BASE_KT: &str = "package app.test\n\nimport app.data.Repo\n\nabstract class Base {\n    lateinit var repo: Repo\n}\n";

/// An inherited, explicitly typed receiver binds through the base class in
/// another file; the same method name on an unrelated receiver does not.
#[test]
fn an_inherited_typed_receiver_binds_and_an_unrelated_one_does_not() {
    let tmp = repo_of(
        &[
            ("data/src/main/kotlin/app/data/Repo.kt", REPO_KT),
            ("app/src/test/kotlin/app/test/Base.kt", BASE_KT),
            (
                "app/src/test/kotlin/app/test/RepoTest.kt",
                "package app.test\n\nclass RepoTest : Base() {\n    fun likedOnly() {\n        repo.findLiked(3)\n    }\n}\n",
            ),
            (
                "app/src/main/kotlin/app/other/Other.kt",
                "package app.other\n\nclass Shelf {\n    fun findLiked(limit: Int) = listOf(limit)\n}\n\nclass User(private val shelf: Shelf) {\n    fun run() = shelf.findLiked(2)\n}\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Repo.findLiked");
    assert_eq!(
        callers(&doc, "findLiked"),
        ["app/src/test/kotlin/app/test/RepoTest.kt::likedOnly resolved call"],
        "{doc:#}"
    );
}

/// `import a.b.f as g`: a call to `g` reaches `f`; a call to an unrelated
/// `f` in another package does not.
#[test]
fn an_aliased_import_binds_and_a_same_named_function_elsewhere_does_not() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/tags/Tags.kt",
                "package a.tags\n\nfun tagFor(id: Int): String = id.toString()\n",
            ),
            (
                "src/main/kotlin/a/ui/View.kt",
                "package a.ui\n\nimport a.tags.tagFor as resolveTag\n\nfun render(id: Int) = resolveTag(id)\n",
            ),
            (
                "src/main/kotlin/a/legacy/Legacy.kt",
                "package a.legacy\n\nfun tagFor(id: Int): String = \"x$id\"\n\nfun old(id: Int) = tagFor(id)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "src/main/kotlin/a/tags/Tags.kt:tagFor");
    assert_eq!(
        callers(&doc, "tagFor"),
        ["src/main/kotlin/a/ui/View.kt::render resolved call"],
        "{doc:#}"
    );
}

/// Kotlin's package is the declared one: a file in another directory that
/// declares the same package reaches a top-level function with no import;
/// a file in the same directory declaring another package does not.
#[test]
fn the_declared_package_decides_not_the_directory() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/x/Money.kt",
                "package billing\n\nfun round(cents: Long): Long = cents\n",
            ),
            (
                "src/main/kotlin/y/Invoice.kt",
                "package billing\n\nfun total(c: Long) = round(c)\n",
            ),
            (
                "src/main/kotlin/x/Other.kt",
                "package shipping\n\nfun round(c: Long): Long = c + 1\n\nfun cost(c: Long) = round(c)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "src/main/kotlin/x/Money.kt:round");
    assert_eq!(
        callers(&doc, "round"),
        ["src/main/kotlin/y/Invoice.kt::total resolved call"],
        "{doc:#}"
    );
}

/// A call on the implicit `this` reaches the member, also from the
/// initializer of a local of the same name (`val p = p()`); a call of that
/// local, a string and a comment do not.
#[test]
fn implicit_this_binds_and_a_local_a_string_and_a_comment_do_not() {
    let tmp = repo_of(
        &[(
            "src/main/kotlin/a/Resolver.kt",
            "package a\n\nclass Resolver {\n    fun tagFor(id: Int) = id\n\n    fun tagsFor(ids: List<Int>) = ids.map { tagFor(it) }\n\n    fun shadowed() {\n        val tagFor = { x: Int -> x }\n        tagFor(1)\n    }\n\n    fun named(): Int {\n        val tagFor = tagFor(4)\n        return tagFor + 1\n    }\n\n    // tagFor(2) in a comment\n    fun quoted() = \"tagFor(3)\"\n}\n",
        )],
        &[],
    );
    let doc = symbol(tmp.path(), "tagFor");
    assert_eq!(
        callers(&doc, "tagFor"),
        [
            "src/main/kotlin/a/Resolver.kt::named resolved call",
            "src/main/kotlin/a/Resolver.kt::tagsFor resolved call",
        ],
        "{doc:#}"
    );
}

/// `Obj::f` and `x::f` are references to `f`, not calls.
#[test]
fn a_callable_reference_is_a_reference() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Covers.kt",
                "package a\n\nobject Covers {\n    fun findIn(dir: String): String? = dir\n}\n",
            ),
            (
                "src/main/kotlin/a/Indexer.kt",
                "package a\n\nclass Indexer {\n    fun ensure(dir: String?) = dir?.let(Covers::findIn)\n}\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Covers.findIn");
    assert_eq!(
        callers(&doc, "findIn"),
        ["src/main/kotlin/a/Indexer.kt::ensure resolved reference"],
        "{doc:#}"
    );
}

/// An extension applies to its receiver type; a member of the receiver's
/// own type wins over it, and another type's call is no use of it.
#[test]
fn an_extension_binds_by_receiver_type_and_a_member_wins() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Pad.kt",
                "package a\n\nfun String.pad(n: Int): String = padStart(n)\n",
            ),
            (
                "src/main/kotlin/a/Use.kt",
                "package a\n\nclass Cell {\n    fun pad(n: Int) = n\n}\n\nfun ok(s: String) = s.pad(3)\n\nfun member(c: Cell) = c.pad(3)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "src/main/kotlin/a/Pad.kt:pad");
    assert_eq!(
        callers(&doc, "pad"),
        ["src/main/kotlin/a/Use.kt::ok resolved call"],
        "{doc:#}"
    );
}

/// Overloads stay distinct definitions, and a call that cannot be told
/// apart is a candidate, never a resolved caller of both.
#[test]
fn an_overloaded_member_is_a_candidate_not_a_resolved_call() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Log.kt",
                "package a\n\nclass Log {\n    fun write(s: String) = s\n    fun write(n: Int) = n\n}\n",
            ),
            (
                "src/main/kotlin/a/App.kt",
                "package a\n\nclass App(private val log: Log) {\n    fun run() = log.write(\"x\")\n}\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Log.write");
    assert_eq!(doc["changed"].as_array().unwrap().len(), 2, "{doc:#}");
    for sym in doc["changed"].as_array().unwrap() {
        for c in sym["callers"].as_array().unwrap() {
            assert_eq!(c["evidence"], "candidate", "{doc:#}");
            assert_eq!(c["reason"], "overloaded", "{doc:#}");
        }
    }
}

/// A new abstract member is implemented by every class that overrides it:
/// they are listed as implementations, apart from callers.
#[test]
fn implementers_of_a_changed_abstract_member_are_their_own_relation() {
    let port = |extra: &str| {
        format!(
            "package a.port\n\ninterface ImagePort {{\n    fun tag(id: Int): String\n{extra}}}\n"
        )
    };
    let tmp = repo_of(
        &[
            ("src/main/kotlin/a/port/ImagePort.kt", &port("")),
            (
                "src/main/kotlin/a/adapter/Disk.kt",
                "package a.adapter\n\nimport a.port.ImagePort\n\nclass Disk : ImagePort {\n    override fun tag(id: Int) = \"d$id\"\n    override fun purge(id: Int) {}\n}\n",
            ),
            (
                "src/main/kotlin/a/adapter/Cloud.kt",
                "package a.adapter\n\nimport a.port.ImagePort\n\nclass Cloud : ImagePort {\n    override fun tag(id: Int) = \"c$id\"\n    override fun purge(id: Int) {}\n}\n",
            ),
            (
                "src/main/kotlin/a/other/Trash.kt",
                "package a.other\n\nclass Trash {\n    fun purge(id: Int) {}\n}\n",
            ),
        ],
        &[(
            "src/main/kotlin/a/port/ImagePort.kt",
            &port("    fun purge(id: Int)\n"),
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "purge"),
        [
            "src/main/kotlin/a/adapter/Cloud.kt::purge resolved implementation",
            "src/main/kotlin/a/adapter/Disk.kt::purge resolved implementation",
        ],
        "{doc:#}"
    );
}

/// `internal` stops at the project — a test of the same project reaches
/// it, another project does not.
#[test]
fn internal_reaches_its_own_tests_and_stops_at_another_project() {
    let tmp = repo_of(
        &[
            ("settings.gradle.kts", "include(\":core\", \":web\")\n"),
            ("core/build.gradle.kts", "plugins { kotlin(\"jvm\") }\n"),
            (
                "web/build.gradle.kts",
                "dependencies { implementation(project(\":core\")) }\n",
            ),
            (
                "core/src/main/kotlin/a/Ledger.kt",
                "package a\n\ninternal fun settle(x: Int) = x\n",
            ),
            (
                "core/src/test/kotlin/a/LedgerTest.kt",
                "package a\n\nclass LedgerTest {\n    fun settles() = settle(1)\n}\n",
            ),
            (
                "web/src/main/kotlin/a/Page.kt",
                "package a\n\nfun page() = settle(2)\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "settle");
    assert_eq!(
        callers(&doc, "settle"),
        ["core/src/test/kotlin/a/LedgerTest.kt::settles resolved call"],
        "{doc:#}"
    );
}

/// An override in a private nested class is reachable only from its file:
/// the call there binds, an untyped `close()` in another file is not even a
/// candidate.
#[test]
fn an_override_in_a_private_class_stays_in_its_file() {
    let tmp = repo_of(
        &[
            (
                "src/test/kotlin/a/SchedulingTest.kt",
                "package a\n\nclass SchedulingTest {\n    private class FakeCluster : AutoCloseable {\n        override fun close() {}\n    }\n\n    fun uses() {\n        FakeCluster().close()\n    }\n}\n",
            ),
            (
                "src/test/kotlin/a/CleanupTest.kt",
                "package a\n\nclass CleanupTest {\n    fun cleans() {\n        val listener = open()\n        listener.close()\n    }\n}\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "FakeCluster.close");
    assert_eq!(
        callers(&doc, "close"),
        ["src/test/kotlin/a/SchedulingTest.kt::uses resolved call"],
        "{doc:#}"
    );
}

/// `val x = store.find(…)` takes the declared return type of the method it
/// calls, so an extension on that type binds; the same call on another
/// store types `x` as that store's result and rejects it.
#[test]
fn a_value_takes_the_return_type_of_the_member_it_is_initialized_by() {
    let tmp = repo_of(
        &[(
            "src/main/kotlin/a/Lyrics.kt",
            "package a\n\nclass Lyrics\n\nclass Cover\n\ninterface LyricsStore {\n    fun find(id: Int): Lyrics?\n}\n\ninterface CoverStore {\n    fun find(id: Int): Cover?\n}\n\nprivate fun Lyrics?.settles(x: Int) = x > 0\n\nprivate fun Cover?.settles(x: Int) = x < 0\n\nclass Resolver(private val lyrics: LyricsStore, private val covers: CoverStore) {\n    fun look(): Boolean {\n        val stored = lyrics.find(1)\n        return stored.settles(2)\n    }\n\n    fun cover(): Boolean {\n        val stored = covers.find(1)\n        return stored.settles(3)\n    }\n}\n",
        )],
        &[],
    );
    let doc = symbol(tmp.path(), "Lyrics.settles");
    assert_eq!(
        callers(&doc, "settles"),
        ["src/main/kotlin/a/Lyrics.kt::look resolved call"],
        "{doc:#}"
    );
}

/// An `inner class` reaches the instance that holds it: the outer property
/// types its receiver and the outer method is its implicit `this`. A nested
/// class's own property of another type, and another class's method of the
/// same name, are not it.
#[test]
fn an_inner_class_reaches_its_outer_instance() {
    let tmp = repo_of(
        &[(
            "src/main/kotlin/a/Scanner.kt",
            "package a\n\nclass Writer {\n    fun upsert(x: Int) = x\n}\n\nclass Other {\n    fun upsert(x: Int) = x\n}\n\nclass Scanner(private val writer: Writer) {\n    fun begin() = 1\n\n    private inner class Visitor {\n        fun visit() {\n            writer.upsert(1)\n            begin()\n        }\n    }\n\n    private class Detached {\n        val writer = Other()\n        fun go() = writer.upsert(2)\n    }\n}\n\nclass Unrelated {\n    fun begin() = 2\n    fun run() = begin()\n}\n",
        )],
        &[],
    );
    assert_eq!(
        callers(&symbol(tmp.path(), "Writer.upsert"), "upsert"),
        ["src/main/kotlin/a/Scanner.kt::visit resolved call"]
    );
    assert_eq!(
        callers(&symbol(tmp.path(), "Scanner.begin"), "begin"),
        ["src/main/kotlin/a/Scanner.kt::visit resolved call"]
    );
}

/// A companion's member through its class, an object's short member through
/// the object; another object's member of the same name, and a lower-case
/// value of the same spelling, are not it.
#[test]
fn companion_and_object_members_bind_through_their_holder_case_kept() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/UI.kt",
                "package a\n\nobject UI {\n    const val DP = 8\n}\n\nobject Print {\n    const val DP = 300\n}\n\nclass Worker {\n    companion object {\n        fun create() = Worker()\n    }\n}\n",
            ),
            (
                "src/main/kotlin/a/Layout.kt",
                "package a\n\nclass Layout(private val ui: Ui) {\n    fun gutter(c: Int) = UI.DP * c\n    fun dpi() = Print.DP\n    fun scaled() = ui.dp\n    fun make() = Worker.create()\n}\n\nclass Ui {\n    val dp = 2\n}\n",
            ),
        ],
        &[],
    );
    assert_eq!(
        callers(&symbol(tmp.path(), "UI.DP"), "DP"),
        ["src/main/kotlin/a/Layout.kt::gutter resolved call"]
    );
    assert_eq!(
        callers(&symbol(tmp.path(), "create"), "create"),
        ["src/main/kotlin/a/Layout.kt::make resolved call"]
    );
}

/// A bodyless annotated class before another annotated class: both are
/// declarations (sg 0.4.1 read the second as an expression).
#[test]
fn a_bodyless_annotated_class_does_not_swallow_the_next_class() {
    let tmp = repo_of(
        &[
            (
                "src/test/kotlin/a/Base.kt",
                "package a\n\n@SpringBootApplication\n@EntityScan(basePackages = [\"a\", \"b\"])\nclass TestApp\n\n// fixture\n@SpringBootTest(classes = [TestApp::class])\nabstract class Base {\n    lateinit var repo: Repo\n}\n",
            ),
            (
                "src/main/kotlin/a/Repo.kt",
                "package a\n\ninterface Repo {\n    fun liked(): List<Int>\n}\n",
            ),
            (
                "src/test/kotlin/a/LikedTest.kt",
                "package a\n\nclass LikedTest : Base() {\n    fun liked() = repo.liked()\n}\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Repo.liked");
    assert_eq!(
        callers(&doc, "liked"),
        ["src/test/kotlin/a/LikedTest.kt::liked resolved call"],
        "{doc:#}"
    );
}

/// A removed declaration still used outside the diff is reported with the
/// use; a removed call statement is no removed declaration.
#[test]
fn a_removed_declaration_is_reported_and_a_removed_call_is_not() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Store.kt",
                "package a\n\nclass Store {\n    fun keep() = 1\n    fun legacy() = 2\n    fun run() {\n        keep()\n        legacy()\n    }\n}\n",
            ),
            (
                "src/main/kotlin/a/Client.kt",
                "package a\n\nclass Client(private val store: Store) {\n    fun go() = store.legacy()\n}\n",
            ),
        ],
        &[(
            "src/main/kotlin/a/Store.kt",
            "package a\n\nclass Store {\n    fun keep() = 1\n    fun run() {\n    }\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    let removed: Vec<&str> = doc["changed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "removed")
        .map(|c| c["symbol"].as_str().unwrap())
        .collect();
    assert_eq!(removed, ["legacy"], "{doc:#}");
    assert_eq!(
        callers(&doc, "legacy"),
        ["src/main/kotlin/a/Client.kt::go resolved call"],
        "{doc:#}"
    );
}

/// A parameter changed after a default is a contract change; a body-only
/// edit of a public function is not.
#[test]
fn a_signature_change_after_a_default_is_a_contract_and_a_body_edit_is_not() {
    let api = |param: &str, body: &str| {
        format!(
            "package a\n\nclass Api {{\n    fun fetch(limit: Int = 10, mode: {param} = {param}()): Int = {body}\n\n    fun stable(x: Int): Int = {body}\n}}\n\nclass Fast\nclass Slow\n"
        )
    };
    let tmp = repo_of(
        &[("src/main/kotlin/a/Api.kt", &api("Fast", "1"))],
        &[("src/main/kotlin/a/Api.kt", &api("Slow", "2"))],
    );
    let doc = impact(tmp.path());
    let contracts: Vec<&str> = doc["contracts"]
        .as_array()
        .map(|a| a.iter().filter_map(|c| c["symbol"].as_str()).collect())
        .unwrap_or_default();
    assert_eq!(contracts, ["fetch"], "{doc:#}");
}

/// A file that parses with an error is analysed only in part, even when a
/// definition survives the break.
#[test]
fn a_surviving_definition_does_not_hide_a_broken_parse() {
    let tmp = repo_of(
        &[(
            "src/main/kotlin/a/Mixed.kt",
            "package a\n\nclass Good {\n    fun ok() = 1\n}\n",
        )],
        &[(
            "src/main/kotlin/a/Mixed.kt",
            "package a\n\nclass Good {\n    fun ok() = 2\n}\n\nclass Bad {\n    fun broken(: Int {\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    let files = doc["unanalysed_files"]
        .as_array()
        .expect("unanalysed_files");
    assert_eq!(files.len(), 1, "{doc:#}");
    assert_eq!(files[0]["reason"], "parse_partial", "{doc:#}");
    assert_eq!(doc["empty"], false);
}

/// A `FooTest` beside `Foo` is a related file, not a guard: a changed
/// method its tests never call has no static test link, and one they call
/// does — with no import, through the shared package of main and test.
#[test]
fn a_test_class_guards_the_methods_it_calls_and_only_those() {
    let foo = |body: &str| {
        format!(
            "package a\n\nclass Foo {{\n    fun changed() = {body}\n\n    fun other() = 1\n}}\n"
        )
    };
    let with_test = |calls: &str| {
        repo_of(
            &[
                ("src/main/kotlin/a/Foo.kt", &foo("1")),
                (
                    "src/test/kotlin/a/FooTest.kt",
                    &format!(
                        "package a\n\nclass FooTest {{\n    fun works() {{\n        Foo().{calls}()\n    }}\n}}\n"
                    ),
                ),
            ],
            &[("src/main/kotlin/a/Foo.kt", &foo("2"))],
        )
    };
    let unguarded = impact(with_test("other").path());
    let unlisted: Vec<&str> = unguarded["untested_unlisted"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(
        unlisted.iter().any(|u| u.contains("Foo.kt::changed")),
        "{unguarded:#}"
    );
    let guarded = impact(with_test("changed").path());
    assert_eq!(
        callers(&guarded, "changed"),
        ["src/test/kotlin/a/FooTest.kt::works resolved call"],
        "{guarded:#}"
    );
    assert!(
        guarded["untested_unlisted"].is_null(),
        "a called method is guarded: {guarded:#}"
    );
}

/// A file in view that spells the changed name but did not parse cleanly
/// may hold a caller nobody saw: the symbol says so instead of reading as
/// one nothing calls. A clean file naming it in a comment is not such a file.
#[test]
fn a_namer_that_did_not_parse_is_named_where_a_caller_may_hide() {
    let ledger =
        |body: &str| format!("package a\n\nclass Ledger {{\n    fun settle(x: Int) = {body}\n}}\n");
    let tmp = repo_of(
        &[
            ("src/main/kotlin/a/Ledger.kt", &ledger("x")),
            (
                "src/main/kotlin/a/Broken.kt",
                "package a\n\nclass Broken(private val l: Ledger) {\n    fun go() = l.settle(1)\n    fun half(: Int {\n}\n",
            ),
            (
                "src/main/kotlin/a/Notes.kt",
                "package a\n\n// settle is called by the checkout\nclass Notes\n",
            ),
        ],
        &[("src/main/kotlin/a/Ledger.kt", &ledger("x + 1"))],
    );
    let doc = impact(tmp.path());
    let sym = doc["changed"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["symbol"] == "settle"))
        .unwrap_or_else(|| panic!("settle listed: {doc:#}"));
    let missed: Vec<&str> = sym["missed_in"]
        .as_array()
        .expect("missed_in")
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(missed, ["src/main/kotlin/a/Broken.kt"], "{doc:#}");
    assert_eq!(doc["empty"], false);
}

/// `--symbol` reads Kotlin's qualifiers from the syntax: an extension by its
/// receiver, a companion member by its companion; a member of the same name
/// on another class is not what the query names.
#[test]
fn a_symbol_query_takes_receivers_and_companions_from_the_syntax() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Pad.kt",
                "package a\n\nfun String.pad(n: Int): String = padStart(n)\n\nclass Cell {\n    fun pad(n: Int) = n\n}\n\nclass Worker {\n    companion object {\n        fun create() = Worker()\n    }\n}\n",
            ),
            (
                "src/main/kotlin/a/Use.kt",
                "package a\n\nfun ok(s: String) = s.pad(3)\n\nfun make() = Worker.create()\n",
            ),
        ],
        &[],
    );
    let pad = symbol(tmp.path(), "String.pad");
    let defs: Vec<&str> = pad["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["lines"].as_str().unwrap())
        .collect();
    assert_eq!(defs, ["3-3"], "the extension, not Cell.pad: {pad:#}");
    assert_eq!(
        callers(&pad, "pad"),
        ["src/main/kotlin/a/Use.kt::ok resolved call"]
    );
    let create = symbol(tmp.path(), "Worker.Companion.create");
    assert_eq!(
        callers(&create, "create"),
        ["src/main/kotlin/a/Use.kt::make resolved call"],
        "{create:#}"
    );
}

/// A caller long enough to have a signature variant is the whole definition:
/// a use in its body is one of its sites, not lost to the signature.
#[test]
fn a_caller_with_a_signature_variant_keeps_the_sites_in_its_body() {
    let tmp = repo_of(
        &[
            (
                "src/main/kotlin/a/Turns.kt",
                "package a\n\nclass Turns(val api: String)\n",
            ),
            (
                "src/main/kotlin/a/Config.kt",
                "package a\n\nclass Config {\n    fun turns(\n        api: String,\n        namespace: String,\n        job: String,\n        token: String,\n    ): Turns {\n        val joined = api + namespace + job + token\n        return Turns(joined)\n    }\n}\n",
            ),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Turns");
    let sym = doc["changed"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["symbol"] == "Turns"))
        .expect("Turns");
    let caller = sym["callers"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["symbol"] == "turns"))
        .unwrap_or_else(|| panic!("{doc:#}"));
    let sites: Vec<u64> = caller["sites"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    assert!(
        sites.contains(&11),
        "the constructor call in the body: {doc:#}"
    );
}
