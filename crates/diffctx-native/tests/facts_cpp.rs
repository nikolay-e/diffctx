//! The C++ extractor, read through the facts it states: one table per kind of
//! fact, each fixture small enough to read its answer off the source.

use _diffctx::facts::lang::c::CppExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Extractor, Fact, Import, ImportKind, LocalKey,
    LocalKind, Namespace, Receiver, Ref, Scope, Visibility,
};
use _diffctx::snapshot::RunBudget;

fn c(source: &str) -> BlobFacts {
    CppExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    match &local.disambig {
        Some(Disambig::Overload(n)) => text.push_str(&format!("#{n}")),
        Some(Disambig::Cfg(cond)) => text.push_str(&format!("[{cond}]")),
        _ => {}
    }
    if matches!(local.ns, Namespace::Type) {
        text.push_str("@type");
    }
    text
}

fn def_named<'a>(facts: &'a BlobFacts, wanted: &str) -> &'a Def {
    facts
        .defs()
        .find(|def| key(&def.local) == wanted)
        .unwrap_or_else(|| {
            let all: Vec<String> = facts.defs().map(|d| key(&d.local)).collect();
            panic!("no definition {wanted} in {all:?}")
        })
}

fn imports(facts: &BlobFacts) -> Vec<&Import> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Import(import) => Some(import),
            _ => None,
        })
        .collect()
}

fn references<'a>(facts: &'a BlobFacts, name: &str) -> Vec<&'a Ref> {
    facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::Reference(r) if r.name.as_str() == name => Some(r),
            _ => None,
        })
        .collect()
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => key(local),
        Scope::Callback { .. } => "callback".to_string(),
    }
}

/// `kind receiver [qualifier] in scope`, for the references to `name`.
fn uses(facts: &BlobFacts, name: &str) -> Vec<String> {
    references(facts, name)
        .iter()
        .map(|r| {
            let qualifier: Vec<&str> = r.qualifier.iter().map(|n| n.as_str()).collect();
            let receiver = match &r.receiver {
                Receiver::None => "none".to_string(),
                Receiver::SelfType => "self".to_string(),
                Receiver::Unknown => "unknown".to_string(),
                Receiver::Typed(t) => format!("typed {}", t.as_str()),
                Receiver::New(t) => format!("new {}", t.as_str()),
                Receiver::Module(m) => format!("module {}", m.as_str()),
                Receiver::CallResult(path) => {
                    let path: Vec<&str> = path.iter().map(|n| n.as_str()).collect();
                    format!("call-result {}", path.join("."))
                }
                other => format!("{other:?}"),
            };
            format!(
                "{:?} {receiver} [{}] in {}",
                r.use_kind,
                qualifier.join("."),
                scope(&r.enclosing)
            )
        })
        .collect()
}

const UNIT: &str = r#"#include <string>
#include "shape.h"

namespace geo {

using std::string;

template <typename T>
T clamp(T v, T lo, T hi) { return v < lo ? lo : v > hi ? hi : v; }

class Shape {
public:
    Shape(int n);
    virtual ~Shape();
    virtual double area() const = 0;
    void scale(double k);
protected:
    int sides;
private:
    double factor;
};

class Circle : public Shape {
public:
    double area() const override { return 3.14 * r * r; }
private:
    double r;
};

Shape::Shape(int n) : sides(n), factor(1.0) {}

void Shape::scale(double k) { factor = k; helper(k); }

}  // namespace geo

double run(geo::Shape *s, int n)
{
    geo::Circle c;
    geo::Shape *p = new geo::Circle();
    s->scale(2.0);
    return geo::clamp(n, 0, 1) + p->area();
}
"#;

#[test]
fn namespaces_classes_and_members_are_definitions() {
    let facts = c(UNIT);
    let sig = |wanted: &str| {
        def_named(&facts, wanted)
            .signature
            .clone()
            .expect("a signature")
    };
    assert_eq!(def_named(&facts, "geo@type").kind, DefKind::Module);
    assert_eq!(def_named(&facts, "geo.Shape@type").kind, DefKind::Class);
    assert_eq!(def_named(&facts, "geo.Circle@type").kind, DefKind::Class);
    assert_eq!(def_named(&facts, "geo.Shape.~Shape").kind, DefKind::Method);
    assert_eq!(sig("geo.Shape.area"), "virtual double area() const = 0");
    let sides = def_named(&facts, "geo.Shape.sides");
    assert_eq!(
        (sides.kind, sides.visibility),
        (DefKind::Field, Visibility::Protected)
    );
    assert_eq!(
        def_named(&facts, "geo.Shape.factor").visibility,
        Visibility::Private
    );
    assert_eq!(
        def_named(&facts, "geo.Circle.area").visibility,
        Visibility::Public
    );
    assert_eq!(def_named(&facts, "geo.Shape.scale").kind, DefKind::Method);
}

#[test]
fn a_template_is_one_definition_with_its_header_in_the_signature() {
    let facts = c(UNIT);
    let clamp = def_named(&facts, "geo.clamp");
    assert_eq!(clamp.kind, DefKind::Function);
    assert!(
        clamp
            .signature
            .as_deref()
            .is_some_and(|s| s.starts_with("template <typename T> T clamp(")),
        "{:?}",
        clamp.signature
    );
}

#[test]
fn an_out_of_line_definition_pairs_with_its_declaration() {
    let facts = c(UNIT);
    let keys: Vec<(String, DefKind, u64)> = facts
        .defs()
        .filter(|d| key(&d.local).starts_with("geo.Shape.Shape"))
        .map(|d| (key(&d.local), d.kind, d.body_hash))
        .collect();
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_eq!(keys[0].0, "geo.Shape.Shape[declaration]");
    assert_eq!(keys[1].0, "geo.Shape.Shape");
    assert!(keys.iter().all(|k| k.1 == DefKind::Constructor));
    assert!(keys[0].2 == 0 && keys[1].2 != 0);
}

#[test]
fn uses_name_their_scope_and_receiver() {
    let facts = c(UNIT);
    assert_eq!(
        uses(&facts, "Shape"),
        [
            "TypeUse none [] in geo.Circle@type",
            "TypeUse module geo [geo] in run",
            "TypeUse module geo [geo] in run"
        ]
    );
    assert_eq!(
        uses(&facts, "Circle"),
        [
            "TypeUse module geo [geo] in run",
            "Instantiate module geo [geo] in run",
            "Instantiate module geo [geo] in run"
        ]
    );
    assert_eq!(uses(&facts, "scale"), ["Call typed geo::Shape [] in run"]);
    assert_eq!(uses(&facts, "area"), ["Call typed geo::Shape [] in run"]);
    assert_eq!(uses(&facts, "clamp"), ["Call module geo [geo] in run"]);
    assert_eq!(uses(&facts, "helper"), ["Call none [] in geo.Shape.scale"]);
}

#[test]
fn using_is_an_import_enclosed_by_its_namespace() {
    let facts = c(UNIT);
    let found: Vec<(String, String)> = imports(&facts)
        .iter()
        .map(|i| (i.module.as_str().to_string(), scope(&i.enclosing)))
        .collect();
    assert_eq!(
        found,
        vec![
            ("string".to_string(), "module".to_string()),
            ("shape.h".to_string(), "module".to_string()),
            ("std".to_string(), "geo@type".to_string()),
        ]
    );
    let directive = c("namespace a { using namespace b::c; }\n");
    let import = imports(&directive)[0];
    assert!(import.star.is_star());
    assert_eq!(import.module.as_str(), "b::c");
}

#[test]
fn this_is_the_receiver_of_a_member() {
    let facts = c("class A {\npublic:\n  int f() { return this->x; }\n  int x;\n};\n");
    assert_eq!(uses(&facts, "x"), ["Read self [] in A.f"]);
}
