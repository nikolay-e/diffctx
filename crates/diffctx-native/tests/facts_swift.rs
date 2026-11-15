//! The Swift extractor, read through the facts it states: the Kotlin reader's
//! contract on the syntax of the Swift grammar, on source small enough to read
//! the answer off.

use _diffctx::facts::lang::swift::SwiftExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Extractor, Fact, ImportKind, Language, Receiver,
    Ref, Scope, Star, TypeName, UseKind,
};
use _diffctx::snapshot::RunBudget;

fn extract(source: &str) -> BlobFacts {
    SwiftExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn def<'a>(blob: &'a BlobFacts, name: &str) -> &'a Def {
    blob.defs()
        .find(|d| d.local.name.as_str() == name)
        .unwrap_or_else(|| panic!("{name}"))
}

fn refs<'a>(blob: &'a BlobFacts, name: &str) -> Vec<&'a Ref> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Reference(r) if r.name.as_str() == name => Some(r),
            _ => None,
        })
        .collect()
}

const SOURCE: &str = "import Foundation
@testable import Shop

public struct Checkout: Billable {
    func charge(_ items: [Int]) -> Int { 1 }
}

enum Pricing {
    static func total(_ items: [Int]) -> Int { 0 }
}

extension Checkout: Codable {
    func summary() -> String { \"x\" }
}

final class CheckoutTests: XCTestCase {
    func testCharge() {
        _ = Checkout().charge([1])
    }
    func helper() {}
}

@main
struct Tool {
    static func main() {}
}

func check(_ c: Checkout, them: Checkout?) {
    guard let me = them else { return }
    me.charge([2])
    c.charge([3])
}
";

#[test]
fn a_swift_file_is_swift_and_reads_whole() {
    let blob = extract(SOURCE);
    assert_eq!(blob.language, Language::Swift);
    assert!(blob.losses.is_empty(), "{:?}", blob.losses);
}

#[test]
fn an_import_is_a_glob_of_the_module_it_names() {
    let blob = extract(SOURCE);
    let modules: Vec<(&str, bool)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::Import(i) => Some((i.module.as_str(), i.star == Star::Glob)),
            _ => None,
        })
        .collect();
    assert_eq!(modules, [("Foundation", true), ("Shop", true)]);
    assert!(blob.facts.iter().all(|f| !matches!(
        f,
        Fact::Import(i) if i.kind == ImportKind::SideEffect
    )));
}

#[test]
fn types_are_told_apart_by_their_keyword() {
    let blob = extract(SOURCE);
    assert_eq!(def(&blob, "Checkout").kind, DefKind::Struct);
    assert_eq!(def(&blob, "Pricing").kind, DefKind::Enum);
    assert_eq!(def(&blob, "CheckoutTests").kind, DefKind::Class);
    assert_eq!(def(&blob, "charge").kind, DefKind::Method);
    assert_eq!(def(&blob, "check").kind, DefKind::Function);
}

#[test]
fn an_extension_is_a_definition_that_extends_the_type_of_its_name() {
    let blob = extract(SOURCE);
    let extension = blob
        .defs()
        .find(|d| d.local.name.as_str() == "Checkout" && d.local.disambig.is_some())
        .expect("the extension");
    assert_eq!(
        extension.local.disambig,
        Some(Disambig::ImplOf(TypeName::new("Checkout")))
    );
    assert!(extension.local.container.is_empty());
    assert!(
        extension
            .signature
            .as_deref()
            .is_some_and(|s| s.contains("Codable"))
    );
    let summary = def(&blob, "summary");
    assert_eq!(summary.local.container[0].as_str(), "Checkout");
    assert_eq!(summary.local.disambig, None);
}

#[test]
fn a_xctest_method_and_a_main_type_are_entries() {
    let blob = extract(SOURCE);
    let entries: Vec<(EntryKind, String)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::EntryPoint(e) => match &e.target {
                Scope::Def(key) => Some((e.kind, key.name.as_str().to_string())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(entries.contains(&(EntryKind::Test, "testCharge".to_string())));
    assert!(entries.contains(&(EntryKind::Main, "Tool".to_string())));
    assert!(!entries.iter().any(|(_, name)| name == "helper"));
}

#[test]
fn a_receiver_is_typed_by_its_parameter_and_an_unwrapped_optional_goes_back_to_its_source() {
    let blob = extract(SOURCE);
    let calls: Vec<&Ref> = refs(&blob, "charge")
        .into_iter()
        .filter(|r| r.use_kind == UseKind::Call && !r.qualifier.is_empty())
        .collect();
    let by_head = |head: &str| {
        calls
            .iter()
            .find(|r| r.qualifier[0].as_str() == head)
            .unwrap_or_else(|| panic!("{head}: {calls:?}"))
    };
    assert_eq!(
        by_head("c").receiver,
        Receiver::Typed(TypeName::new("Checkout"))
    );
    // `me` unwraps `them`, a parameter of type `Checkout?`: the chain goes on
    // from `them`, typed as written.
    let me = calls
        .iter()
        .find(|r| r.qualifier[0].as_str() == "them")
        .expect("me is read as them");
    assert_eq!(me.receiver, Receiver::Typed(TypeName::new("Checkout")));
}
