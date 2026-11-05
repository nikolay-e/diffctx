//! The Rust extractor, fact kind by fact kind, on source small enough to read the
//! right answer off. Nothing here runs diffctx to learn an answer.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use _diffctx::facts::lang::rust::RustExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, Disambig, EntryKind, Export, Extractor, Fact, Import, LocalKey,
    Receiver, Ref, Scope, UseKind, Visibility,
};
use _diffctx::snapshot::{Loss, RunBudget, Stage};

fn extract(source: &str) -> BlobFacts {
    RustExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn defs(blob: &BlobFacts) -> Vec<&Def> {
    blob.defs().collect()
}

fn refs(blob: &BlobFacts) -> Vec<&Ref> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Reference(r) => Some(r),
            _ => None,
        })
        .collect()
}

fn imports(blob: &BlobFacts) -> Vec<&Import> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Import(i) => Some(i),
            _ => None,
        })
        .collect()
}

/// `A.m`, with `~Tr` for a trait impl's method, `#n` for the nth twin and `@cfg`
/// for a configuration twin.
fn key(local: &LocalKey) -> String {
    let path: Vec<&str> = local
        .container
        .iter()
        .map(|c| c.as_str())
        .chain([local.name.as_str()])
        .collect();
    let twin = match &local.disambig {
        None => String::new(),
        Some(Disambig::Overload(n)) => format!("#{n}"),
        Some(Disambig::Cfg(cfg)) => format!("@{cfg}"),
        Some(Disambig::ImplOf(tr)) => format!("~{tr}"),
    };
    format!("{}{twin}", path.join("::"))
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => format!("def:{}", key(local)),
        Scope::Callback { .. } => "callback".to_string(),
    }
}

fn lines_of_defs(source: &str) -> Vec<String> {
    defs(&extract(source))
        .iter()
        .map(|d| format!("{:?} {}", d.kind, key(&d.local)))
        .collect()
}

fn def<'a>(blob: &'a BlobFacts, wanted: &str) -> &'a Def {
    defs(blob)
        .into_iter()
        .find(|d| key(&d.local) == wanted)
        .unwrap_or_else(|| panic!("no definition {wanted}"))
}

/// `Call a::b::f`, with the scope it sits in appended.
fn ref_line(r: &Ref) -> String {
    let qualifier: Vec<&str> = r.qualifier.iter().map(|q| q.as_str()).collect();
    let written = if qualifier.is_empty() {
        r.name.as_str().to_string()
    } else {
        format!("{}::{}", qualifier.join("::"), r.name.as_str())
    };
    format!("{:?} {written} in {}", r.use_kind, scope(&r.enclosing))
}

fn ref_lines(source: &str) -> Vec<String> {
    refs(&extract(source)).into_iter().map(ref_line).collect()
}

fn receiver_of<'a>(blob: &'a BlobFacts, name: &str) -> (&'a Receiver, Vec<&'a str>) {
    let found = refs(blob)
        .into_iter()
        .find(|r| r.name.as_str() == name && r.use_kind != UseKind::Override)
        .unwrap_or_else(|| panic!("no reference to {name}"));
    (
        &found.receiver,
        found.qualifier.iter().map(|q| q.as_str()).collect(),
    )
}

#[test]
fn definitions_by_kind_and_container() {
    let found = lines_of_defs(
        "pub struct Cart { items: Vec<u8>, pub total: u32 }\n\
         pub enum Mode { Fast, Slow(u8), Exact { digits: u8 } }\n\
         pub trait Priced { fn price(&self) -> u32; fn tax(&self) -> u32 { 0 } }\n\
         impl Cart { pub fn new() -> Self { todo!() } fn add(&mut self) {} }\n\
         impl Priced for Cart { fn price(&self) -> u32 { 1 } }\n\
         pub const LIMIT: u32 = 3;\n\
         static NAME: &str = \"x\";\n\
         type Id = u32;\n\
         macro_rules! twice { ($e:expr) => { $e + $e }; }\n\
         pub fn top() {}\n",
    );
    assert_eq!(
        found,
        [
            "Struct Cart",
            "Field Cart::items",
            "Field Cart::total",
            "Enum Mode",
            "EnumMember Mode::Fast",
            "EnumMember Mode::Slow",
            "EnumMember Mode::Exact",
            "Trait Priced",
            "Method Priced::price",
            "Method Priced::tax",
            "Method Cart::new",
            "Method Cart::add",
            "Method Cart::price~Priced",
            "Constant LIMIT",
            "Variable NAME",
            "TypeAlias Id",
            "Macro twice",
            "Function top",
        ]
    );
}

#[test]
fn an_inline_module_is_a_definition_and_the_container_of_its_items() {
    let found = lines_of_defs(
        "mod util;\n\
         pub mod inner {\n    pub fn helper() {}\n    pub struct Thing;\n    impl Thing { pub fn go(&self) {} }\n}\n",
    );
    assert_eq!(
        found,
        [
            "Module util",
            "Module inner",
            "Function inner::helper",
            "Struct inner::Thing",
            "Method inner::Thing::go",
        ]
    );
}

#[test]
fn an_item_local_to_a_function_has_the_function_as_container() {
    let found =
        lines_of_defs("fn outer() {\n    fn helper() {}\n    const N: u8 = 1;\n    helper();\n}\n");
    assert_eq!(
        found,
        [
            "Function outer",
            "Function outer::helper",
            "Constant outer::N"
        ]
    );
}

#[test]
fn visibility_is_what_the_item_writes() {
    let blob = extract(
        "pub fn a() {}\nfn b() {}\npub(crate) fn c() {}\npub(super) fn d() {}\n\
         trait Tr { fn m(&self); }\nstruct S;\nimpl Tr for S { fn m(&self) {} }\n\
         impl S { fn own(&self) {} pub fn shown(&self) {} }\n",
    );
    let seen: Vec<(String, Visibility)> = defs(&blob)
        .iter()
        .map(|d| (key(&d.local), d.visibility))
        .collect();
    let of = |name: &str| seen.iter().find(|(k, _)| k == name).map(|(_, v)| *v);
    assert_eq!(of("a"), Some(Visibility::Public));
    assert_eq!(of("b"), Some(Visibility::Private));
    assert_eq!(of("c"), Some(Visibility::Crate));
    assert_eq!(of("d"), Some(Visibility::Package));
    assert_eq!(of("Tr::m"), Some(Visibility::Public));
    assert_eq!(of("S::m~Tr"), Some(Visibility::Public));
    assert_eq!(of("S::own"), Some(Visibility::Private));
    assert_eq!(of("S::shown"), Some(Visibility::Public));
}

#[test]
fn twins_of_one_key_are_told_apart() {
    let found = lines_of_defs(
        "#[cfg(unix)]\nfn path() {}\n#[cfg(windows)]\nfn path() {}\nstruct W<T>(T);\n\
         impl W<u8> { fn get(&self) {} }\nimpl W<u16> { fn get(&self) {} }\n",
    );
    assert_eq!(
        found,
        [
            "Function path",
            "Function path@#[cfg(windows)]",
            "Struct W",
            "Method W::get",
            "Method W::get#1",
        ]
    );
}

#[test]
fn a_trait_impl_names_the_trait_and_overrides_its_methods() {
    let lines = ref_lines(
        "trait Shape { fn area(&self) -> f64; }\nstruct Circle;\n\
         impl Shape for Circle { fn area(&self) -> f64 { 1.0 } }\n\
         impl std::fmt::Display for Circle { fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result { Ok(()) } }\n",
    );
    for expected in [
        "Override Shape in def:Circle",
        "Override Shape::area in def:Circle::area~Shape",
        "Override std::fmt::Display in def:Circle",
        "Override std::fmt::Display::fmt in def:Circle::fmt~std::fmt::Display",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "{expected} in {lines:#?}"
        );
    }
}

#[test]
fn use_trees_are_imports_of_the_module_they_name_from() {
    let blob = extract(
        "use std::collections::HashMap;\nuse crate::a::{b, c as d, e::{self, f}};\nuse super::*;\nuse self::g as h;\n",
    );
    let seen: Vec<String> = imports(&blob)
        .iter()
        .map(|i| {
            let names: Vec<String> = i
                .names
                .iter()
                .map(|(n, a)| match a {
                    Some(a) => format!("{n} as {a}"),
                    None => n.to_string(),
                })
                .collect();
            let star = if i.star.is_star() { " *" } else { "" };
            format!("{} [{}]{star}", i.module, names.join(", "))
        })
        .collect();
    assert_eq!(
        seen,
        [
            "std::collections [HashMap]",
            "crate::a [b]",
            "crate::a [c as d]",
            "crate::a [e]",
            "crate::a::e [f]",
            "super [] *",
            "self [g as h]",
        ]
    );
}

#[test]
fn a_pub_use_is_also_an_export() {
    let blob = extract("pub use self::inner::Thing;\npub use other::*;\nuse private::Hidden;\n");
    let exports: Vec<String> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::Export(Export::Named { name, from, .. }) => Some(format!(
                "{name} from {}",
                from.as_ref().map(|m| m.as_str()).unwrap_or("")
            )),
            Fact::Export(Export::Star { from, .. }) => Some(format!("* from {from}")),
            _ => None,
        })
        .collect();
    assert_eq!(exports, ["Thing from self::inner", "* from other"]);
}

#[test]
fn a_use_inside_a_function_belongs_to_it() {
    let blob = extract("fn f() {\n    use std::fmt::Write;\n    let _ = 1;\n}\n");
    let found = imports(&blob);
    assert_eq!(found.len(), 1);
    assert_eq!(scope(&found[0].enclosing), "def:f");
}

#[test]
fn references_name_the_last_segment_and_qualify_it() {
    let lines = ref_lines(
        "use a::b;\nfn f(x: Foo) {\n    plain();\n    a::b::deep();\n    Cart::new();\n    Self::me();\n    let _ = LIMIT;\n}\n",
    );
    for expected in [
        "TypeUse Foo in def:f",
        "Call plain in def:f",
        "Call a::b::deep in def:f",
        "Call Cart::new in def:f",
        "Call Self::me in def:f",
        "Read LIMIT in def:f",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "{expected} in {lines:#?}"
        );
    }
}

#[test]
fn a_local_is_no_reference_and_a_parameter_neither() {
    let lines = ref_lines(
        "fn f(count: u32) -> u32 {\n    let total = count + 1;\n    let step = |n: u32| n + total;\n    step(total)\n}\n",
    );
    assert!(
        !lines.iter().any(|l| l.contains(" count ")
            || l.contains(" total ")
            || l.contains(" step ")
            || l.contains(" n ")),
        "{lines:#?}"
    );
}

#[test]
fn a_function_passed_by_name_is_passed_as_a_value() {
    let lines = ref_lines(
        "fn f(v: Vec<&str>) {\n    v.iter().map(parse).count();\n    apply(handler, 3);\n}\n",
    );
    assert!(
        lines.iter().any(|l| l == "PassAsValue parse in def:f"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "PassAsValue handler in def:f"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "Call apply in def:f"),
        "{lines:#?}"
    );
}

#[test]
fn a_struct_literal_instantiates_and_a_pattern_reads() {
    let lines = ref_lines(
        "fn f(m: Mode) -> Cart {\n    match m {\n        Mode::Fast => {}\n        Mode::Slow(n) => {}\n        _ => {}\n    }\n    Cart { total: 1 }\n}\n",
    );
    assert!(
        lines.iter().any(|l| l == "Instantiate Cart in def:f"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "Read Mode::Fast in def:f"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "Read Mode::Slow in def:f"),
        "{lines:#?}"
    );
}

#[test]
fn a_macro_invocation_is_a_call_of_the_name_with_a_bang() {
    let lines = ref_lines("fn f() {\n    println!(\"{}\", helper(1));\n    twice!(3);\n}\n");
    assert!(
        lines.iter().any(|l| l == "Call println! in def:f"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "Call twice! in def:f"),
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l == "Call helper in def:f"),
        "{lines:#?}"
    );
}

#[test]
fn method_receivers_say_what_the_syntax_states() {
    let blob = extract(
        "impl Cart {\n    fn run(&self, other: &Other, boxed: Box<Inner>) {\n        self.direct();\n        self.field.indirect();\n        other.typed();\n        boxed.smart();\n        let built = Maker::new();\n        built.constructed();\n        let loose = make();\n        loose.from_call();\n        stranger.unknown();\n        helper().chained();\n    }\n}\n",
    );
    assert_eq!(receiver_of(&blob, "direct"), (&Receiver::SelfType, vec![]));
    assert_eq!(
        receiver_of(&blob, "indirect"),
        (&Receiver::SelfType, vec!["field"])
    );
    let typed = |t: &str| Receiver::Typed(t.into());
    assert_eq!(receiver_of(&blob, "typed"), (&typed("Other"), vec![]));
    assert_eq!(receiver_of(&blob, "smart"), (&typed("Inner"), vec![]));
    assert_eq!(receiver_of(&blob, "constructed"), (&typed("Maker"), vec![]));
    assert_eq!(
        receiver_of(&blob, "from_call"),
        (&Receiver::CallResult(vec!["make".into()]), vec![])
    );
    assert_eq!(
        receiver_of(&blob, "unknown"),
        (&Receiver::Unknown, vec!["stranger"])
    );
    assert_eq!(
        receiver_of(&blob, "chained"),
        (&Receiver::CallResult(vec!["helper".into()]), vec![])
    );
}

#[test]
fn a_trait_object_and_a_bound_are_the_type_dyn_trait() {
    let blob = extract(
        "fn f(a: &dyn Shape, b: impl Draw, c: T) where T: Paint {\n    a.area();\n    b.draw();\n    c.paint();\n}\n\
         fn g<U: Draw>(u: U, v: Free) {\n    u.draw();\n    v.other();\n}\n",
    );
    let typed = |t: &str| Receiver::Typed(t.into());
    assert_eq!(receiver_of(&blob, "area"), (&typed("dyn Shape"), vec![]));
    assert_eq!(receiver_of(&blob, "draw").0, &typed("dyn Draw"));
    assert_eq!(receiver_of(&blob, "paint"), (&typed("dyn Paint"), vec![]));
    assert_eq!(receiver_of(&blob, "other"), (&typed("Free"), vec![]));
}

#[test]
fn an_unwrapped_call_result_carries_a_final_question_mark() {
    let blob = extract(
        "fn f() -> Result<(), E> {\n    let x = load()?;\n    x.use_it();\n    Ok(())\n}\n",
    );
    assert_eq!(
        receiver_of(&blob, "use_it"),
        (
            &Receiver::CallResult(vec!["load".into(), "?".into()]),
            vec![]
        )
    );
}

#[test]
fn a_write_to_a_field_is_a_write() {
    let lines = ref_lines(
        "impl C {\n    fn f(&mut self) {\n        self.n = 1;\n        self.n += 1;\n        let _ = self.n;\n    }\n}\n",
    );
    let writes = lines.iter().filter(|l| l.starts_with("Write n ")).count();
    assert_eq!(writes, 2, "{lines:#?}");
    assert!(lines.iter().any(|l| l.starts_with("Read n ")), "{lines:#?}");
}

#[test]
fn tokens_of_a_macro_are_read_as_code() {
    let blob = extract(
        "impl C {\n    fn f(&self, x: &Other) {\n        assert_eq!(self.a(), x.b());\n        vec![Maker::new(), free()];\n    }\n}\n",
    );
    let lines: Vec<String> = refs(&blob).into_iter().map(ref_line).collect();
    for expected in [
        "Call a in def:C::f",
        "Call b in def:C::f",
        "Call Maker::new in def:C::f",
        "Call free in def:C::f",
    ] {
        assert!(
            lines.iter().any(|l| l == expected),
            "{expected} in {lines:#?}"
        );
    }
    assert_eq!(receiver_of(&blob, "a").0, &Receiver::SelfType);
    assert_eq!(receiver_of(&blob, "b").0, &Receiver::Typed("Other".into()));
}

#[test]
fn tests_and_main_are_entry_points() {
    let blob = extract(
        "fn main() {}\n#[test]\nfn works() {}\n#[tokio::test]\nasync fn async_works() {}\nmod inner { fn main() {} }\n",
    );
    let entries: Vec<(EntryKind, String)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::EntryPoint(e) => Some((e.kind, scope(&e.target))),
            _ => None,
        })
        .collect();
    assert_eq!(
        entries,
        [
            (EntryKind::Main, "def:main".to_string()),
            (EntryKind::Test, "def:works".to_string()),
            (EntryKind::Test, "def:async_works".to_string()),
        ]
    );
}

#[test]
fn a_member_is_left_out_of_its_containers_hashes() {
    let one = extract("struct S { a: u8, b: u8 }\n");
    let two = extract("struct S { a: u8, b: u16 }\n");
    assert_eq!(def(&one, "S").body_hash, def(&two, "S").body_hash);
    assert_eq!(def(&one, "S").text_hash, def(&two, "S").text_hash);
    assert_ne!(def(&one, "S::b").signature, def(&two, "S::b").signature);
    assert_eq!(def(&one, "S::a").text_hash, def(&two, "S::a").text_hash);
}

#[test]
fn a_body_change_moves_the_body_hash_alone() {
    let one = extract("pub fn f(x: u8) -> u8 {\n    x + 1\n}\n");
    let two = extract("pub fn f(x: u8) -> u8 {\n    x + 2\n}\n");
    let (a, b) = (def(&one, "f"), def(&two, "f"));
    assert_eq!(a.signature, b.signature);
    assert_ne!(a.body_hash, b.body_hash);
    let reformatted = extract("pub fn f(x: u8) -> u8 {\n    // note\n    x   +   1\n}\n");
    assert_eq!(a.body_hash, def(&reformatted, "f").body_hash);
    assert_eq!(
        a.signature.as_deref(),
        Some("pub fn f(x: u8) -> u8"),
        "the signature is the header"
    );
}

#[test]
fn a_doc_comment_is_a_doc_hash_and_a_plain_one_is_nothing() {
    let blob = extract("/// Adds one.\nfn a() {}\n// plain\nfn b() {}\n");
    assert!(def(&blob, "a").doc_hash.is_some());
    assert!(def(&blob, "b").doc_hash.is_none());
}

#[test]
fn facts_come_in_source_order() {
    let blob = extract("fn a() { b(); }\nfn b() {}\n");
    let starts: Vec<u32> = blob.facts.iter().map(|f| f.site().bytes.start).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted);
}

#[test]
fn a_parse_error_is_a_loss_and_the_rest_is_read() {
    let blob = extract("fn ok() {}\nfn broken( {\nfn also_ok() {}\n");
    assert!(
        blob.losses
            .iter()
            .any(|l| matches!(l, Loss::ParseError { .. })),
        "{:?}",
        blob.losses
    );
    assert!(defs(&blob).iter().any(|d| key(&d.local) == "ok"));
}

#[test]
fn an_expired_budget_is_a_deadline_loss() {
    let budget = RunBudget {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..RunBudget::default()
    };
    let blob = RustExtractor.extract(b"fn a() {}\n", &budget);
    assert!(
        blob.losses.contains(&Loss::Deadline {
            stage: Stage::Extraction
        }),
        "{:?}",
        blob.losses
    );
}

#[test]
fn the_kind_of_a_method_is_method_and_a_free_function_is_a_function() {
    let blob = extract("impl S { fn m(&self) { fn inner() {} } }\nfn free() {}\n");
    assert_eq!(def(&blob, "S::m").kind, DefKind::Method);
    assert_eq!(def(&blob, "S::m::inner").kind, DefKind::Function);
    assert_eq!(def(&blob, "free").kind, DefKind::Function);
}

#[test]
fn a_qualified_path_calls_through_its_type() {
    let blob = extract("fn f() {\n    <Maker as Build>::make();\n    <Other>::go();\n}\n");
    assert_eq!(
        receiver_of(&blob, "make"),
        (&Receiver::Typed("Maker".into()), vec![])
    );
    assert_eq!(
        receiver_of(&blob, "go"),
        (&Receiver::Typed("Other".into()), vec![])
    );
}

#[test]
fn a_generic_parameter_is_declared_not_used() {
    let lines = ref_lines("fn f<T: Clone>(x: T) -> T {\n    x\n}\n");
    assert!(
        lines.iter().any(|l| l == "TypeUse Clone in def:f"),
        "{lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("TypeUse T ")),
        "{lines:#?}"
    );
}

#[test]
fn an_impl_for_a_reference_is_an_impl_of_the_type() {
    let found = lines_of_defs("impl Tr for &Foo {\n    fn m(&self) {}\n}\n");
    assert_eq!(found, ["Method Foo::m~Tr"]);
}
