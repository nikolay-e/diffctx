//! The Swift cell: the Kotlin engine in its Swift dialect, on small
//! repositories whose right answer is read off the source (#526). A module is
//! the SwiftPM target a file sits in; nothing here runs diffctx to learn an
//! answer.

mod snapshot_fixture;

use _diffctx::facts::{DefKey, FactsCache, LocalKey, Name};
use _diffctx::resolve::{Binding, Relation, Resolution, SwiftResolver};
use _diffctx::snapshot::{AnalysisContext, RangeSpec, RunBudget, Side, SideTag};
use snapshot_fixture::Repo;

struct Fixture {
    _repo: Repo,
    head: SwiftResolver,
}

fn snapshot(files: &[(&str, &str)]) -> Fixture {
    let repo = Repo::init();
    for (path, text) in files {
        repo.write(path, text);
    }
    let head = repo.commit("snapshot");
    let ctx = AnalysisContext::new(
        repo.root.clone(),
        RangeSpec::At(Side::Tree(head)),
        RunBudget::default(),
    )
    .expect("an analysis context");
    let resolver = SwiftResolver::open(&ctx, SideTag::Head, &FactsCache::disabled())
        .expect("the head resolves");
    Fixture {
        _repo: repo,
        head: resolver,
    }
}

fn dotted(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(Name::as_str).collect();
    parts.push(local.name.as_str());
    parts.join(".")
}

impl Fixture {
    fn where_is(&self, key: &DefKey) -> String {
        let found = self
            .head
            .definitions()
            .iter()
            .find(|d| d.key == *key)
            .expect("a resolved target is a definition of the snapshot");
        format!("{}::{}", found.file, dotted(&key.local))
    }

    fn at(&self, file: &str, name: &str, line: u32) -> &Binding {
        let found: Vec<&Binding> = self
            .head
            .bindings()
            .iter()
            .filter(|b| {
                b.file.as_str() == file
                    && b.name.as_str() == name
                    && b.site.lines.0 == line
                    && b.relation != Relation::Imports
            })
            .collect();
        assert_eq!(found.len(), 1, "{file}:{line} {name}: {found:#?}");
        found[0]
    }

    fn lands(&self, file: &str, name: &str, line: u32) -> String {
        match &self.at(file, name, line).resolution {
            Resolution::Resolved { target, .. } => self.where_is(target),
            Resolution::Module { file, .. } => format!("module {file}"),
            Resolution::Candidate { targets, reason } => {
                let mut names: Vec<String> = targets.iter().map(|t| self.where_is(t)).collect();
                names.sort();
                format!("candidate({reason:?}) {}", names.join(", "))
            }
            Resolution::Unresolved { reason } => format!("unresolved({reason:?})"),
        }
    }
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

const LOOP: &str = "func loop(_ all: [Track]) {
    for t in all {
        t.sourceLost(why: \"l\", nowNs: 7)
    }
}
";

fn receivers() -> Fixture {
    snapshot(&[
        ("Sources/Track.swift", TRACK),
        ("Sources/main.swift", MAIN),
        ("Sources/Other.swift", OTHER),
        ("Sources/Loop.swift", LOOP),
    ])
}

#[test]
fn a_method_is_reached_through_self_a_typed_parameter_and_an_unwrapped_optional() {
    let fx = receivers();
    let method = "Sources/Track.swift::Track.sourceLost";
    assert_eq!(fx.lands("Sources/Track.swift", "sourceLost", 7), method);
    assert_eq!(fx.lands("Sources/Track.swift", "sourceLost", 8), method);
    assert_eq!(fx.lands("Sources/main.swift", "sourceLost", 4), method);
    assert_eq!(fx.lands("Sources/main.swift", "sourceLost", 9), method);
    assert_eq!(fx.lands("Sources/main.swift", "sourceLost", 11), method);
}

#[test]
fn a_free_function_is_no_method_namesake_and_text_names_nothing() {
    let fx = receivers();
    assert_eq!(
        fx.lands("Sources/main.swift", "lostFree", 5),
        "Sources/Track.swift::lostFree"
    );
    assert!(
        fx.head
            .bindings()
            .iter()
            .all(|b| b.file.as_str() != "Sources/Other.swift" || b.site.lines.0 != 11),
        "a comment and a string are no use"
    );
}

#[test]
fn a_receiver_of_the_other_type_lands_on_its_own_method() {
    let fx = receivers();
    assert_eq!(
        fx.lands("Sources/Other.swift", "sourceLost", 6),
        "Sources/Other.swift::Other.sourceLost"
    );
}

#[test]
fn a_loop_variable_over_a_collection_is_a_candidate_of_every_namesake() {
    let fx = receivers();
    assert_eq!(
        fx.lands("Sources/Loop.swift", "sourceLost", 3),
        "candidate(UntypedReceiver) Sources/Other.swift::Other.sourceLost, Sources/Track.swift::Track.sourceLost"
    );
}

#[test]
fn a_static_member_is_reached_through_its_type_and_an_instance_member_is_not() {
    let fx = snapshot(&[
        (
            "Sources/Shop/Pricing.swift",
            "enum Pricing {\n    static func total(_ items: [Int]) -> Int { 0 }\n}\n",
        ),
        (
            "Sources/Shop/Basket.swift",
            "struct Basket {\n    func total(_ items: [Int]) -> Int { items.count }\n}\n\nfunc size(_ b: Basket) -> Int {\n    b.total([1])\n}\n",
        ),
        (
            "Sources/Shop/Report.swift",
            "func build(_ items: [Int]) -> String {\n    \"total=\" + String(Pricing.total(items))\n}\n",
        ),
    ]);
    assert_eq!(
        fx.lands("Sources/Shop/Report.swift", "total", 2),
        "Sources/Shop/Pricing.swift::Pricing.total"
    );
    assert_eq!(
        fx.lands("Sources/Shop/Basket.swift", "total", 6),
        "Sources/Shop/Basket.swift::Basket.total"
    );
}

#[test]
fn a_module_reaches_another_one_only_through_its_import() {
    let fx = snapshot(&[
        (
            "Sources/Shop/Checkout.swift",
            "public struct Checkout {\n    public init() {}\n    public func charge(_ items: [Int]) -> Int { 1 }\n}\n",
        ),
        (
            "Tests/ShopTests/CheckoutTests.swift",
            "import XCTest\n@testable import Shop\n\nfinal class CheckoutTests: XCTestCase {\n    func testCharge() {\n        _ = Checkout().charge([1, 2])\n    }\n}\n",
        ),
        (
            "Sources/Other/Elsewhere.swift",
            "func elsewhere() {\n    _ = Checkout().charge([1])\n}\n",
        ),
    ]);
    assert_eq!(
        fx.lands("Tests/ShopTests/CheckoutTests.swift", "charge", 6),
        "Sources/Shop/Checkout.swift::Checkout.charge"
    );
    assert!(
        fx.lands("Sources/Other/Elsewhere.swift", "charge", 2)
            .starts_with("unresolved"),
        "no import, no name"
    );
}

#[test]
fn an_extension_adds_its_members_to_the_type_of_another_file() {
    let fx = snapshot(&[
        ("Sources/A/Track.swift", "final class Track {}\n"),
        (
            "Sources/A/Track+Summary.swift",
            "extension Track {\n    func summary() -> String { \"x\" }\n    func again() { summary() }\n}\n",
        ),
        (
            "Sources/A/Use.swift",
            "func use(_ t: Track) {\n    t.summary()\n}\n",
        ),
    ]);
    assert_eq!(
        fx.lands("Sources/A/Use.swift", "summary", 2),
        "Sources/A/Track+Summary.swift::Track.summary"
    );
    assert_eq!(
        fx.lands("Sources/A/Track+Summary.swift", "summary", 3),
        "Sources/A/Track+Summary.swift::Track.summary"
    );
}

#[test]
fn a_call_through_a_protocol_may_run_in_a_conforming_type() {
    let fx = snapshot(&[(
        "Sources/A/Sink.swift",
        "protocol Sink {\n    func drain()\n}\n\nstruct File: Sink {\n    func drain() {}\n}\n\nfunc flush(_ s: Sink) {\n    s.drain()\n}\n",
    )]);
    assert_eq!(
        fx.lands("Sources/A/Sink.swift", "drain", 10),
        "candidate(Dispatch) Sources/A/Sink.swift::File.drain, Sources/A/Sink.swift::Sink.drain"
    );
}
