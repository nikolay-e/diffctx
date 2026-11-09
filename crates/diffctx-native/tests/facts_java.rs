//! The Java extractor, read through the facts it states: one table per kind of
//! fact, each fixture small enough to read its answer off the source.

use _diffctx::facts::lang::java::JavaExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Extractor, Fact, Import, ImportKind, LocalKey,
    LocalKind, Namespace, Receiver, Ref, Scope, UseKind, Visibility,
};
use _diffctx::snapshot::RunBudget;

fn java(source: &str) -> BlobFacts {
    JavaExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn key(local: &LocalKey) -> String {
    let mut parts: Vec<&str> = local.container.iter().map(|n| n.as_str()).collect();
    parts.push(local.name.as_str());
    let mut text = parts.join(".");
    if let Some(Disambig::Overload(n)) = &local.disambig {
        text.push_str(&format!("#{n}"));
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

const SERVICE: &str = "package com.acme.svc;

import com.acme.mapper.DrugMapper;
import com.acme.util.*;
import static com.acme.Util.helper;
import static com.acme.Consts.*;

@Service
public class MedicineService extends Base implements Svc {
    private final DrugMapper mapper;
    static final int MAX = 2;

    public MedicineService(DrugMapper mapper) {
        this.mapper = mapper;
    }

    public MedicineService() {
        this(null);
    }

    @Override
    public Dto details(Draft draft) {
        var local = new DrugMapper();
        helper();
        Runnable r = this::close;
        MAX = 3;
        return this.mapper.toDto(draft);
    }

    enum Color { RED, GREEN }

    interface Port { void save(); }
}
";

#[test]
fn the_package_and_the_imports_are_facts_of_the_blob() {
    let facts = java(SERVICE);
    assert!(facts.losses.is_empty(), "{:?}", facts.losses);
    let all = imports(&facts);
    let package = all
        .iter()
        .find(|i| i.kind == ImportKind::SideEffect)
        .expect("a package");
    assert_eq!(package.module.as_str(), "com.acme.svc");
    let named: Vec<(&str, Vec<&str>)> = all
        .iter()
        .filter(|i| i.kind == ImportKind::Static && !i.star.is_star())
        .map(|i| {
            (
                i.module.as_str(),
                i.names.iter().map(|(n, _)| n.as_str()).collect(),
            )
        })
        .collect();
    assert_eq!(
        named,
        [
            ("com.acme.mapper", vec!["DrugMapper"]),
            ("com.acme.Util", vec!["helper"])
        ]
    );
    let mut stars: Vec<&str> = all
        .iter()
        .filter(|i| i.star.is_star())
        .map(|i| i.module.as_str())
        .collect();
    stars.sort_unstable();
    assert_eq!(stars, ["com.acme.Consts", "com.acme.util"]);
}

#[test]
fn definitions_by_kind_container_and_overload() {
    let facts = java(SERVICE);
    let kind = |wanted: &str| def_named(&facts, wanted).kind;
    assert_eq!(kind("MedicineService@type"), DefKind::Class);
    assert_eq!(kind("MedicineService.mapper"), DefKind::Field);
    assert_eq!(kind("MedicineService.MAX"), DefKind::Constant);
    assert_eq!(
        kind("MedicineService.MedicineService"),
        DefKind::Constructor
    );
    assert_eq!(
        kind("MedicineService.MedicineService#1"),
        DefKind::Constructor
    );
    assert_eq!(kind("MedicineService.details"), DefKind::Method);
    assert_eq!(kind("MedicineService.Color@type"), DefKind::Enum);
    assert_eq!(kind("MedicineService.Color.RED"), DefKind::EnumMember);
    assert_eq!(kind("MedicineService.Port@type"), DefKind::Interface);
    let class = def_named(&facts, "MedicineService@type");
    assert_eq!(
        class
            .decorators
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>(),
        ["Service"]
    );
    let header = class.signature.as_deref().expect("a header");
    assert!(header.contains("extends Base implements Svc"), "{header}");
    let details = def_named(&facts, "MedicineService.details");
    assert_eq!(details.visibility, Visibility::Public);
    assert_eq!(
        details
            .decorators
            .iter()
            .map(|d| d.as_str())
            .collect::<Vec<_>>(),
        ["Override"]
    );
    assert_eq!(
        def_named(&facts, "MedicineService.mapper").visibility,
        Visibility::Private
    );
    assert_eq!(
        def_named(&facts, "MedicineService.Port.save").visibility,
        Visibility::Package,
        "a member of a package-private interface"
    );
    // The name is where it is written in the header.
    let name = &def_named(&facts, "MedicineService.details").name_span;
    assert_eq!(
        &SERVICE.as_bytes()[name.bytes.start as usize..name.bytes.end as usize],
        b"details"
    );
}

#[test]
fn references_carry_their_kind_and_the_receiver_they_are_reached_through() {
    let facts = java(SERVICE);
    assert_eq!(
        uses(&facts, "toDto"),
        ["Call self [mapper] in MedicineService.details"]
    );
    assert_eq!(
        uses(&facts, "helper"),
        ["Call none [] in MedicineService.details"]
    );
    assert!(
        uses(&facts, "DrugMapper")
            .iter()
            .any(|u| u == "Instantiate none [] in MedicineService.details"),
        "{:?}",
        uses(&facts, "DrugMapper")
    );
    assert!(
        uses(&facts, "DrugMapper")
            .iter()
            .any(|u| u.starts_with("TypeUse none [] in MedicineService.mapper")),
        "{:?}",
        uses(&facts, "DrugMapper")
    );
    assert_eq!(
        uses(&facts, "close"),
        ["PassAsValue self [] in MedicineService.details"]
    );
    assert_eq!(
        uses(&facts, "MAX"),
        ["Write none [] in MedicineService.details"]
    );
    let draft = references(&facts, "draft");
    assert_eq!(draft.len(), 1);
    assert_eq!(draft[0].use_kind, UseKind::Read);
}

#[test]
fn a_method_that_states_override_has_an_override_reference_to_its_name() {
    let facts = java(SERVICE);
    assert!(
        uses(&facts, "details")
            .contains(&"Override none [] in MedicineService.details".to_string()),
        "{:?}",
        uses(&facts, "details")
    );
}

#[test]
fn locals_are_visible_from_their_binding_to_the_end_of_their_scope() {
    let facts = java(SERVICE);
    let locals: Vec<(&str, LocalKind)> = facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::LocalBinding(local) => Some((local.name.as_str(), local.kind)),
            _ => None,
        })
        .collect();
    assert!(
        locals.contains(&("draft", LocalKind::Parameter)),
        "{locals:?}"
    );
    assert!(
        locals.contains(&("local", LocalKind::Variable)),
        "{locals:?}"
    );
    assert!(locals.contains(&("r", LocalKind::Variable)), "{locals:?}");
}

#[test]
fn a_constructed_var_is_typed_by_its_initializer_and_a_declared_local_by_its_type() {
    let source = "package p;

class Use {
    void run(Repo given) {
        var made = new Repo();
        Repo local = made;
        given.find(1);
        made.find(2);
        local.find(3);
    }
}
";
    let facts = java(source);
    assert_eq!(
        uses(&facts, "find"),
        [
            "Call typed Repo [given] in Use.run",
            "Call call-result Repo [made] in Use.run",
            "Call typed Repo [local] in Use.run"
        ]
    );
}

#[test]
fn a_constructed_type_keeps_the_package_it_is_written_with() {
    let facts = java(
        "package p;\n\nclass Use {\n    Object make() {\n        return new a.b.Foo();\n    }\n}\n",
    );
    let foo = references(&facts, "Foo");
    let instantiated = foo
        .iter()
        .find(|r| r.use_kind == UseKind::Instantiate)
        .expect("an instantiation");
    let qualifier: Vec<&str> = instantiated.qualifier.iter().map(|n| n.as_str()).collect();
    assert_eq!(qualifier, ["a", "b"]);
}

#[test]
fn tests_and_main_are_entry_points() {
    let facts = java(
        "package p;\n\nclass T {\n    @Test void t() {\n    }\n\n    public static void main(String[] args) {\n    }\n\n    void plain() {\n    }\n}\n",
    );
    let entries: Vec<(EntryKind, String)> = facts
        .facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::EntryPoint(e) => Some((e.kind, scope(&e.target))),
            _ => None,
        })
        .collect();
    assert_eq!(
        entries,
        [
            (EntryKind::Test, "T.t".to_string()),
            (EntryKind::Main, "T.main".to_string())
        ]
    );
}

#[test]
fn an_anonymous_class_is_a_type_its_members_belong_to() {
    let facts = java(
        "package p;\n\nclass Use {\n    Svc make() {\n        return new Svc() {\n            @Override public int get(int id) {\n                return 7;\n            }\n        };\n    }\n}\n",
    );
    assert_eq!(def_named(&facts, "Use.new Svc@type").kind, DefKind::Class);
    assert_eq!(def_named(&facts, "Use.new Svc.get").kind, DefKind::Method);
}

#[test]
fn a_record_component_is_a_field_and_an_annotation_type_an_interface() {
    let facts = java("package p;\n\nrecord Pair(int a, int b) {\n}\n\n@interface Marker {\n}\n");
    assert_eq!(def_named(&facts, "Pair@type").kind, DefKind::Class);
    assert_eq!(def_named(&facts, "Pair.a").kind, DefKind::Field);
    assert_eq!(def_named(&facts, "Marker@type").kind, DefKind::Interface);
}

#[test]
fn facts_come_out_in_source_order() {
    let facts = java(SERVICE);
    let starts: Vec<u32> = facts.facts.iter().map(|f| f.site().bytes.start).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted);
}
