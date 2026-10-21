// Swift and Java callers through the binding model they share with Kotlin,
// the contracts a test file does not have, and the hedge a partial parse puts
// on a name (Swift/Java lane): each gate pairs the true answer with
// look-alikes that must not join it — a same-named method of another type,
// the name in a string or a comment, a test-fixtures library that is shipped
// — and a control that has to come back exactly.

use std::path::Path;

use serde_json::Value;

mod support;
use support::{diffctx, repo};

fn run(repo: &Path, args: &[&str]) -> Value {
    let out = diffctx()
        .current_dir(repo)
        .args(args)
        .args(["-q", "--timeout", "600"])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

fn symbol(repo: &Path, query: &str) -> Value {
    run(repo, &["--symbol", query])
}

fn impact(repo: &Path) -> Value {
    run(repo, &["--diff", "HEAD~1..HEAD", "--mode", "impact"])
}

fn changed<'a>(doc: &'a Value, path: &str, name: &str) -> &'a Value {
    doc["changed"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["path"] == path && c["symbol"] == name))
        .unwrap_or_else(|| panic!("{path}::{name} not in {doc:#}"))
}

/// `path::caller-symbol evidence` of every caller of `path::name`.
fn callers(doc: &Value, path: &str, name: &str) -> Vec<String> {
    let mut out: Vec<String> = changed(doc, path, name)["callers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            format!(
                "{}::{} {}",
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap_or("-"),
                c["evidence"].as_str().unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

const TRACK: &str = "func lostFree(why: String) { print(why) }

final class Track {
    func sourceLost(why: String, nowNs: UInt64) { print(why, nowNs) }

    func again() {
        sourceLost(why: \"self\", nowNs: 3)
        self.sourceLost(why: \"self\", nowNs: 4)
    }
}
";

const MAIN: &str = "var them: Track?

func check(_ track: Track) {
    track.sourceLost(why: \"x\", nowNs: 2)
    lostFree(why: \"y\")
}

func drainAll() {
    them?.sourceLost(why: \"z\", nowNs: 5)
    guard let me = them else { return }
    me.sourceLost(why: \"w\", nowNs: 6)
}
";

/// The decoys: the same method name on another type, reached through a
/// receiver typed as that type, and the name in a comment and a string.
const OTHER: &str = "final class Other {
    func sourceLost(why: String, nowNs: UInt64) { print(why, nowNs) }
}

func viaOther(_ o: Other) {
    o.sourceLost(why: \"d\", nowNs: 9)
}

func notes() -> String {
    // track.sourceLost(why:nowNs:) and lostFree(why:) were the old way
    return \"track.sourceLost(why:) lostFree(why:)\"
}
";

/// A receiver nothing types: a hedged caller, never none and never resolved.
const LOOP: &str = "func loop(_ all: [Track]) {
    for t in all {
        t.sourceLost(why: \"l\", nowNs: 7)
    }
}
";

/// #526: free functions, typed and optional receivers, explicit and
/// implicit `self` resolve; the other type's method and the text do not.
#[test]
fn swift_calls_resolve_through_their_receiver_and_the_look_alikes_do_not() {
    let tmp = repo(
        &[
            ("Sources/Track.swift", TRACK),
            ("Sources/main.swift", MAIN),
            ("Sources/Other.swift", OTHER),
            ("Sources/Loop.swift", LOOP),
        ],
        &[],
    );
    let doc = symbol(tmp.path(), "Sources/Track.swift:sourceLost");
    assert_eq!(
        callers(&doc, "Sources/Track.swift", "sourceLost"),
        [
            "Sources/Loop.swift::loop candidate",
            "Sources/Track.swift::again resolved",
            "Sources/main.swift::check resolved",
            "Sources/main.swift::drainAll resolved",
        ]
    );
    let doc = symbol(tmp.path(), "lostFree");
    assert_eq!(
        callers(&doc, "Sources/Track.swift", "lostFree"),
        ["Sources/main.swift::check resolved"]
    );
    let doc = symbol(tmp.path(), "Sources/Other.swift:sourceLost");
    assert_eq!(
        callers(&doc, "Sources/Other.swift", "sourceLost"),
        [
            "Sources/Loop.swift::loop candidate",
            "Sources/Other.swift::viaOther resolved",
        ]
    );
}

/// #526: a member an extension in another file adds is the type's, called
/// through a typed receiver or the implicit `self` of another extension; a
/// test target reaches it through its `@testable import`, and a module that
/// does not import it does not.
#[test]
fn an_extension_member_is_the_types_across_files_and_modules() {
    let tmp = repo(
        &[
            (
                "Sources/Rec/Track.swift",
                "public final class Track {\n    public init() {}\n}\n",
            ),
            (
                "Sources/Rec/Track+Summary.swift",
                "extension Track {\n    func summary() -> String {\n        return \"s\"\n    }\n}\n",
            ),
            (
                "Sources/Rec/Track+Report.swift",
                "extension Track {\n    func report() -> String {\n        return summary()\n    }\n}\n",
            ),
            (
                "Sources/Rec/Recorder.swift",
                "final class Recorder {\n    let track = Track()\n\n    func flush() -> String {\n        track.summary()\n    }\n}\n",
            ),
            (
                "Tests/RecTests/TrackTests.swift",
                "import XCTest\n@testable import Rec\n\nfinal class TrackTests: XCTestCase {\n    func testSummary() {\n        XCTAssertEqual(Track().summary(), \"s\")\n    }\n}\n",
            ),
            (
                "Sources/Other/Summary.swift",
                "final class Report {\n    func summary() -> String { \"r\" }\n}\n\nfunc show(_ r: Report) -> String {\n    r.summary()\n}\n",
            ),
        ],
        &[(
            "Sources/Rec/Track+Summary.swift",
            "extension Track {\n    func summary() -> String {\n        return \"summary\"\n    }\n}\n",
        )],
    );
    let doc = impact(tmp.path());
    assert_eq!(
        callers(&doc, "Sources/Rec/Track+Summary.swift", "summary"),
        [
            "Sources/Rec/Recorder.swift::flush resolved",
            "Sources/Rec/Track+Report.swift::report resolved",
            "Tests/RecTests/TrackTests.swift::testSummary resolved",
        ]
    );
}

const MAPPER: &str = "package com.acme.mapper;

public interface Mapper {
    String toDto(String draft);
}
";

fn drug_mapper(body: &str) -> String {
    format!(
        "package com.acme.mapper;

public class DrugMapper implements Mapper {{
    @Override
    public String toDto(String draft) {{
        return {body};
    }}
}}
"
    )
}

fn same_named_mapper(name: &str) -> String {
    format!(
        "package com.acme.mapper;

public class {name} implements Mapper {{
    @Override
    public String toDto(String draft) {{
        return \"{name}\";
    }}
}}
"
    )
}

/// The decoys: a mapper of another type held by a typed field, a report of
/// another package with the same method, and the name in a comment and a
/// string. A receiver typed by the interface may dispatch here: hedged.
const DRUG_SERVICE: &str = "package com.acme.svc;

import com.acme.mapper.AlphaMapper;
import com.acme.mapper.DrugMapper;
import com.acme.mapper.Mapper;
import com.acme.report.Report;

public class DrugService {
    private final DrugMapper drugMapper;
    private final AlphaMapper alphaMapper = new AlphaMapper();
    private Mapper anyMapper;

    public DrugService(DrugMapper drugMapper) {
        this.drugMapper = drugMapper;
    }

    public String details(String draft) {
        return drugMapper.toDto(draft);
    }

    public String viaThis(String draft) {
        return this.drugMapper.toDto(draft);
    }

    public String viaLocal(String draft) {
        var m = new DrugMapper();
        return m.toDto(draft);
    }

    public String viaAlpha(String draft) {
        return alphaMapper.toDto(draft);
    }

    public String viaReport(Report report, String draft) {
        return report.toDto(draft);
    }

    public String viaInterface(String draft) {
        return anyMapper.toDto(draft);
    }

    public String notes() {
        // drugMapper.toDto(draft) was the old way
        return \"drugMapper.toDto(draft)\";
    }
}
";

/// #335: a Java method whose name five classes share is called through a
/// field, `this.field` and a `var` its constructor types: each call binds
/// to the field's type, the service's test reaches it through the caller,
/// and the look-alikes stay out.
#[test]
fn a_java_receiver_typed_by_its_field_resolves_among_same_named_methods() {
    let (before, after) = (drug_mapper("draft"), drug_mapper("draft.trim()"));
    let alpha = same_named_mapper("AlphaMapper");
    let beta = same_named_mapper("BetaMapper");
    let gamma = same_named_mapper("GammaMapper");
    let tmp = repo(
        &[
            ("pom.xml", "<project></project>\n"),
            ("src/main/java/com/acme/mapper/Mapper.java", MAPPER),
            (
                "src/main/java/com/acme/mapper/DrugMapper.java",
                before.as_str(),
            ),
            (
                "src/main/java/com/acme/mapper/AlphaMapper.java",
                alpha.as_str(),
            ),
            (
                "src/main/java/com/acme/mapper/BetaMapper.java",
                beta.as_str(),
            ),
            (
                "src/main/java/com/acme/mapper/GammaMapper.java",
                gamma.as_str(),
            ),
            (
                "src/main/java/com/acme/report/Report.java",
                "package com.acme.report;\n\npublic class Report {\n    public String toDto(String draft) {\n        return draft;\n    }\n}\n",
            ),
            ("src/main/java/com/acme/svc/DrugService.java", DRUG_SERVICE),
            (
                "src/test/java/com/acme/svc/DrugServiceTest.java",
                "package com.acme.svc;\n\nimport com.acme.mapper.DrugMapper;\nimport org.junit.jupiter.api.Test;\n\nclass DrugServiceTest {\n    @Test\n    void details() {\n        new DrugService(new DrugMapper()).details(\"x\");\n    }\n}\n",
            ),
        ],
        &[(
            "src/main/java/com/acme/mapper/DrugMapper.java",
            after.as_str(),
        )],
    );
    let doc = impact(tmp.path());
    let path = "src/main/java/com/acme/mapper/DrugMapper.java";
    let svc = "src/main/java/com/acme/svc/DrugService.java";
    assert_eq!(
        callers(&doc, path, "toDto"),
        [
            format!("{svc}::details resolved"),
            format!("{svc}::viaInterface candidate"),
            format!("{svc}::viaLocal resolved"),
            format!("{svc}::viaThis resolved"),
        ]
    );
    let details = changed(&doc, path, "toDto")["callers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["symbol"] == "details")
        .unwrap()
        .clone();
    assert_eq!(
        details["tested_by"],
        "src/test/java/com/acme/svc/DrugServiceTest.java"
    );
}

/// #517: a test's declarations are no API — a renamed JUnit method, a new
/// test class, a test double, a helper a `*.test.ts` exports. A shipped
/// test-fixtures library and a `testing` package of main sources are API,
/// and so is the main-source signature the same commit changes.
#[test]
fn a_test_file_changes_no_contract_and_a_shipped_fixture_library_does() {
    let tmp = repo(
        &[
            ("build.gradle.kts", "plugins { kotlin(\"jvm\") }\n"),
            (
                "src/main/kotlin/app/AuthHandler.kt",
                "package app\n\nclass AuthHandler {\n    fun login(user: String): Boolean = user.isNotEmpty()\n}\n",
            ),
            (
                "src/test/kotlin/app/AuthHandlerTest.kt",
                "package app\n\nimport org.junit.jupiter.api.Test\n\nclass AuthHandlerTest {\n    @Test\n    fun `a second login still succeeds`() {\n        AuthHandler().login(\"a\")\n    }\n}\n",
            ),
            (
                "web/src/sum.ts",
                "export function sum(a: number, b: number): number {\n  return a + b;\n}\n",
            ),
            (
                "web/src/sum.test.ts",
                "import { sum } from \"./sum\";\n\nexport function helper(): number {\n  return sum(1, 2);\n}\n",
            ),
        ],
        &[
            (
                "src/main/kotlin/app/AuthHandler.kt",
                "package app\n\nclass AuthHandler {\n    fun login(user: String, device: String): Boolean = user.isNotEmpty()\n}\n",
            ),
            (
                "src/test/kotlin/app/AuthHandlerTest.kt",
                "package app\n\nimport org.junit.jupiter.api.Test\n\nclass AuthHandlerTest {\n    @Test\n    fun `a second login keeps its id`() {\n        AuthHandler().login(\"a\", \"d\")\n    }\n}\n",
            ),
            (
                "src/test/kotlin/app/ArtworkCacheTest.kt",
                "package app\n\nimport org.junit.jupiter.api.Test\n\nclass ArtworkCacheTest {\n    @Test\n    fun `caches artwork`() {\n        AuthHandler().login(\"c\", \"d\")\n    }\n}\n",
            ),
            (
                "src/test/java/app/RunnerTest.java",
                "package app;\n\nimport org.junit.jupiter.api.Test;\n\npublic class RunnerTest {\n    @Test\n    public void runs() {\n        Service s = new Service() {\n            @Override public int get(int id) { return 7; }\n        };\n        s.get(1);\n    }\n}\n",
            ),
            (
                "web/src/sum.test.ts",
                "import { sum } from \"./sum\";\n\nexport function helper(n: number): number {\n  return sum(n, 2);\n}\n",
            ),
            (
                "src/testFixtures/kotlin/app/FakeClock.kt",
                "package app\n\nclass FakeClock {\n    fun now(): Long = 0\n}\n",
            ),
            (
                "src/main/kotlin/app/testing/Fakes.kt",
                "package app.testing\n\nclass FakeHandler {\n    fun handle(): Boolean = true\n}\n",
            ),
        ],
    );
    let doc = impact(tmp.path());
    let mut contracts: Vec<String> = doc["contracts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            format!(
                "{} {}::{}",
                c["kind"].as_str().unwrap(),
                c["path"].as_str().unwrap(),
                c["symbol"].as_str().unwrap_or("-")
            )
        })
        .collect();
    contracts.sort();
    assert_eq!(
        contracts,
        [
            "public_api src/main/kotlin/app/AuthHandler.kt::login",
            "public_api src/main/kotlin/app/testing/Fakes.kt::FakeHandler",
            "public_api src/main/kotlin/app/testing/Fakes.kt::handle",
            "public_api src/testFixtures/kotlin/app/FakeClock.kt::FakeClock",
            "public_api src/testFixtures/kotlin/app/FakeClock.kt::now",
        ]
    );
}

/// #537: a file whose parse broke on one line was read everywhere else: a
/// name it spells elsewhere is found like any other and puts no hedge on
/// the definition; a name spelled on the broken line keeps it, whatever of
/// the call the parse recovered.
#[test]
fn a_partial_parse_hedges_only_the_names_on_the_lines_it_broke_on() {
    let tmp = repo(
        &[
            ("api.swift", "func load() -> Int {\n    return 1\n}\n"),
            (
                "Panel.swift",
                "func panel() -> Int {\n    let v = load()\n    return v\n}\n\nfunc f(_ rate: Double) -> Double {\n    let s = (rate +)\n    return s\n}\n",
            ),
            (
                "Broken.swift",
                "func other() -> Int {\n    let w = load(1 +)\n    return w\n}\n",
            ),
        ],
        &[("api.swift", "func load() -> Int {\n    return 2\n}\n")],
    );
    let doc = impact(tmp.path());
    let load = changed(&doc, "api.swift", "load");
    let missed: Vec<&str> = load["missed_in"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(missed, ["Broken.swift"]);
    assert_eq!(
        callers(&doc, "api.swift", "load"),
        [
            "Broken.swift::other resolved",
            "Panel.swift::panel resolved"
        ]
    );
}

/// #537: `as? T ?? default` in a call argument inside a trailing closure is
/// valid Swift, and the grammar reads it: the file is analysed whole.
#[test]
fn a_cast_with_a_default_in_a_trailing_closure_parses() {
    let gaps = |body: &str| {
        format!(
            "func f(_ gaps: [[String: Any]], _ rate: Double) -> Double {{\n    let s = gaps.reduce(0.0) {{ {body} }}\n    return s\n}}\n"
        )
    };
    let tmp = repo(
        &[("a.swift", &gaps("$0 + 2 / rate"))],
        &[("a.swift", &gaps("$0 + Double($1[\"frames\"] as? Int ?? 1)"))],
    );
    let doc = impact(tmp.path());
    let unanalysed: Vec<&str> = doc["unanalysed_files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert!(unanalysed.is_empty(), "{doc}");
}
