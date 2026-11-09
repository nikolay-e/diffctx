//! The Go extractor, fact kind by fact kind, on source small enough to read the
//! right answer off. Nothing here runs diffctx to learn an answer.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use _diffctx::facts::lang::go::GoExtractor;
use _diffctx::facts::{
    BlobFacts, Def, DefKind, EntryKind, Export, Extractor, Fact, Import, ImportKind, LocalKey,
    Receiver, Ref, Scope, Star, TypeName, Visibility,
};
use _diffctx::snapshot::{Loss, RunBudget, Stage};

fn extract(source: &str) -> BlobFacts {
    GoExtractor.extract(source.as_bytes(), &RunBudget::default())
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

/// `Cart.Add`: the container, then the name, with `#n` for the nth twin.
fn key(local: &LocalKey) -> String {
    let path: Vec<&str> = local
        .container
        .iter()
        .map(|c| c.as_str())
        .chain([local.name.as_str()])
        .collect();
    let twin = match &local.disambig {
        Some(_) => "#",
        None => "",
    };
    format!("{}{twin}", path.join("."))
}

fn scope(scope: &Scope) -> String {
    match scope {
        Scope::Module => "module".to_string(),
        Scope::Def(local) => format!("def:{}", key(local)),
        Scope::Callback { callee, parent, .. } => {
            let callee: Vec<&str> = callee.iter().map(|c| c.as_str()).collect();
            format!("callback:{} in {}", callee.join("."), self::scope(parent))
        }
    }
}

fn lines_of_defs(source: &str) -> Vec<String> {
    extract(source)
        .defs()
        .map(|d| format!("{:?} {}", d.kind, key(&d.local)))
        .collect()
}

fn def<'a>(blob: &'a BlobFacts, wanted: &str) -> &'a Def {
    blob.defs()
        .find(|d| key(&d.local) == wanted)
        .unwrap_or_else(|| panic!("no definition {wanted}"))
}

/// `Call a.b.f`, with the scope it sits in appended.
fn ref_line(r: &Ref) -> String {
    let qualifier: Vec<&str> = r.qualifier.iter().map(|q| q.as_str()).collect();
    let written = if qualifier.is_empty() {
        r.name.as_str().to_string()
    } else {
        format!("{}.{}", qualifier.join("."), r.name.as_str())
    };
    format!("{:?} {written} in {}", r.use_kind, scope(&r.enclosing))
}

fn ref_lines(source: &str) -> Vec<String> {
    refs(&extract(source)).into_iter().map(ref_line).collect()
}

fn receiver_of<'a>(blob: &'a BlobFacts, name: &str) -> (&'a Receiver, Vec<&'a str>) {
    let found = refs(blob)
        .into_iter()
        .find(|r| r.name.as_str() == name)
        .unwrap_or_else(|| panic!("no reference to {name}"));
    (
        &found.receiver,
        found.qualifier.iter().map(|q| q.as_str()).collect(),
    )
}

#[test]
fn definitions_by_kind_and_container() {
    let found = lines_of_defs(
        "package shop\n\n\
         type Cart struct {\n\titems []int\n\tTotal int\n\tBase\n\t*other.Meta\n}\n\n\
         type Priced interface {\n\tPrice() int\n\tTax() int\n}\n\n\
         type Id = int\n\n\
         type Money int\n\n\
         func (c *Cart) Add(k int) { c.Total += k }\n\n\
         func (c Cart) Price() int { return c.Total }\n\n\
         const Limit = 3\n\n\
         var name = \"x\"\n\n\
         func Top() {}\n",
    );
    assert_eq!(
        found,
        [
            "Struct Cart",
            "Field Cart.items",
            "Field Cart.Total",
            "Field Cart.Base",
            "Field Cart.Meta",
            "Interface Priced",
            "Method Priced.Price",
            "Method Priced.Tax",
            "TypeAlias Id",
            "TypeAlias Money",
            "Method Cart.Add",
            "Method Cart.Price",
            "Constant Limit",
            "Variable name",
            "Function Top",
        ]
    );
}

#[test]
fn grouped_declarations_are_one_definition_per_name() {
    let found = lines_of_defs(
        "package p\n\nconst (\n\tA = iota\n\tB\n)\n\nvar (\n\tx, y int\n\t_ = 1\n)\n\n\
         type (\n\tOne struct{}\n\tTwo interface{}\n)\n",
    );
    assert_eq!(
        found,
        [
            "Constant A",
            "Constant B",
            "Variable x",
            "Variable y",
            "Struct One",
            "Interface Two"
        ]
    );
}

#[test]
fn a_method_of_a_generic_type_is_the_types_member() {
    let blob = extract(
        "package p\n\ntype Box[T any] struct{ v T }\n\nfunc (b *Box[T]) Get() T { return b.v }\n",
    );
    assert_eq!(def(&blob, "Box.Get").kind, DefKind::Method);
    assert_eq!(def(&blob, "Box.v").kind, DefKind::Field);
}

#[test]
fn a_capital_is_public_and_the_rest_is_package() {
    let blob =
        extract("package p\n\nfunc Open() {}\n\nfunc open() {}\n\ntype T struct{ A, b int }\n");
    assert_eq!(def(&blob, "Open").visibility, Visibility::Public);
    assert_eq!(def(&blob, "open").visibility, Visibility::Package);
    assert_eq!(def(&blob, "T.A").visibility, Visibility::Public);
    assert_eq!(def(&blob, "T.b").visibility, Visibility::Package);
}

#[test]
fn a_signature_is_its_tokens_without_the_body_the_value_or_the_members() {
    let blob = extract(
        "package p\n\nfunc (c *Cart) Add(k int) (int, error) { return k, nil }\n\n\
         type Cart struct{ n int }\n\nconst Limit int = 3\n\nvar Name = f()\n",
    );
    assert_eq!(
        def(&blob, "Cart.Add").signature.as_deref(),
        Some("func ( c * Cart ) Add ( k int ) ( int , error )")
    );
    assert_eq!(
        def(&blob, "Cart").signature.as_deref(),
        Some("Cart struct { }")
    );
    assert_eq!(def(&blob, "Cart.n").signature.as_deref(), Some("n int"));
    assert_eq!(
        def(&blob, "Limit").signature.as_deref(),
        Some("Limit int =")
    );
    assert_eq!(def(&blob, "Name").signature.as_deref(), Some("Name ="));
}

#[test]
fn an_embedded_field_is_named_after_its_type_and_is_that_type_alone() {
    let blob = extract("package p\n\ntype T struct {\n\tBase\n\t*io.Reader\n\tn int\n}\n");
    assert_eq!(def(&blob, "T.Base").signature.as_deref(), Some("Base"));
    assert_eq!(
        def(&blob, "T.Reader").signature.as_deref(),
        Some("* io . Reader")
    );
}

#[test]
fn imports_by_form() {
    let blob = extract(
        "package shop\n\nimport \"strconv\"\n\nimport (\n\tp \"example.com/app/pricing\"\n\t. \"example.com/app/dot\"\n\t_ \"example.com/app/side\"\n)\n",
    );
    let found: Vec<String> = imports(&blob)
        .into_iter()
        .map(|i| {
            format!(
                "{} {:?} {:?} {}",
                i.module,
                i.kind,
                i.star,
                i.default.as_ref().map_or("-", |d| d.as_str())
            )
        })
        .collect();
    assert_eq!(
        found,
        [
            "strconv Static None -",
            "example.com/app/pricing Static None p",
            "example.com/app/dot Static Glob -",
            "example.com/app/side SideEffect None -",
        ]
    );
    assert!(imports(&blob).iter().all(|i| i.relative == 0));
    assert_eq!(imports(&blob)[2].star, Star::Glob);
    assert_eq!(imports(&blob)[3].kind, ImportKind::SideEffect);
}

#[test]
fn calls_through_packages_locals_and_results() {
    let lines = ref_lines(
        "package shop\n\nfunc Fill(c *cart.Cart) int {\n\tc.Add(1)\n\tt := p.Total(2)\n\tx := cart.New()\n\tx.Add(3)\n\tfmt.Println(t)\n\treturn Limit\n}\n",
    );
    assert_eq!(
        lines,
        [
            "TypeUse cart.Cart in def:Fill",
            "Call Add in def:Fill",
            "Call p.Total in def:Fill",
            "Call cart.New in def:Fill",
            "Call Add in def:Fill",
            "Call fmt.Println in def:Fill",
            "Read Limit in def:Fill",
        ]
    );
}

#[test]
fn the_receiver_is_what_the_syntax_states() {
    let blob = extract(
        "package shop\n\nfunc (c *Cart) Total() int {\n\treturn c.Sum()\n}\n\nfunc Fill(d *cart.Cart, n Other) {\n\td.Add(1)\n\tx := cart.New()\n\tx.Run()\n\tn.Go()\n\tm := make(map[string]int)\n\tm.Len()\n\tcart.New().Close()\n}\n",
    );
    assert_eq!(
        receiver_of(&blob, "Sum"),
        (&Receiver::Typed(TypeName::new("Cart")), vec![])
    );
    assert_eq!(
        receiver_of(&blob, "Add"),
        (&Receiver::Typed(TypeName::new("cart.Cart")), vec![])
    );
    assert!(matches!(
        receiver_of(&blob, "Run"),
        (Receiver::CallResult(path), _) if path.iter().map(|n| n.as_str()).eq(["cart", "New"])
    ));
    assert_eq!(
        receiver_of(&blob, "Go"),
        (&Receiver::Typed(TypeName::new("Other")), vec![])
    );
    assert_eq!(receiver_of(&blob, "Len"), (&Receiver::Unknown, vec!["m"]));
    assert!(matches!(
        receiver_of(&blob, "Close"),
        (Receiver::CallResult(path), _) if path.iter().map(|n| n.as_str()).eq(["cart", "New"])
    ));
}

#[test]
fn fields_after_a_typed_local_are_the_qualifier() {
    let blob = extract("package p\n\nfunc F(a *A) {\n\ta.b.Run()\n}\n");
    assert_eq!(
        receiver_of(&blob, "Run"),
        (&Receiver::Typed(TypeName::new("A")), vec!["b"])
    );
}

#[test]
fn a_composite_literal_instantiates_and_its_keys_write_the_fields() {
    let lines = ref_lines(
        "package p\n\nfunc F() {\n\tc := Cart{Total: 1, items: nil}\n\td := &cart.Cart{}\n\t_ = new(Box)\n\t_, _ = c, d\n}\n",
    );
    assert_eq!(
        lines,
        [
            "Instantiate Cart in def:F",
            "Write Total in def:F",
            "Write items in def:F",
            "Instantiate cart.Cart in def:F",
            "Instantiate Box in def:F",
        ]
    );
    let blob = extract("package p\n\nfunc F() {\n\t_ = Cart{Total: 1}\n}\n");
    assert_eq!(
        receiver_of(&blob, "Total"),
        (&Receiver::Typed(TypeName::new("Cart")), vec![])
    );
}

#[test]
fn the_type_make_names_is_said_once_and_a_decrement_writes() {
    assert_eq!(
        ref_lines("package p\n\nfunc F(n int) {\n\t_ = make([]Item, n)\n\tCount--\n}\n"),
        ["TypeUse Item in def:F", "Write Count in def:F"]
    );
}

#[test]
fn a_name_handed_to_a_call_is_passed_as_a_value() {
    let lines = ref_lines(
        "package p\n\nfunc F(items []int) {\n\tsort.Ints(items)\n\tApply(Double)\n\tApply(cart.Double)\n}\n",
    );
    assert_eq!(
        lines,
        [
            "Call sort.Ints in def:F",
            "Call Apply in def:F",
            "PassAsValue Double in def:F",
            "Call Apply in def:F",
            "PassAsValue cart.Double in def:F",
        ]
    );
}

#[test]
fn assignments_write_fields_and_package_variables_not_locals() {
    let lines = ref_lines(
        "package p\n\nfunc F(c *Cart) {\n\tn := 0\n\tn = 1\n\tc.Total += 2\n\tCount++\n\tcart.Mode = 3\n\t_ = n\n}\n",
    );
    assert_eq!(
        lines,
        [
            "TypeUse Cart in def:F",
            "Write Total in def:F",
            "Write Count in def:F",
            "Write cart.Mode in def:F",
        ]
    );
}

#[test]
fn a_function_literal_argument_is_a_callback_and_any_other_is_part_of_its_definition() {
    let lines = ref_lines(
        "package p\n\nfunc F() {\n\thttp.HandleFunc(\"/\", func() { Serve() })\n\tgo func() { Work() }()\n}\n",
    );
    assert_eq!(
        lines,
        [
            "Call http.HandleFunc in def:F",
            "Call Serve in callback:http.HandleFunc in def:F",
            "Call Work in def:F",
        ]
    );
}

#[test]
fn names_in_type_positions_are_type_uses_and_the_predeclared_ones_are_not() {
    let lines = ref_lines(
        "package p\n\ntype T struct {\n\ta []Item\n\tb map[string]*pkg.Value\n\tc error\n}\n\nfunc G(x Item) (r Result, err error) { return }\n",
    );
    assert_eq!(
        lines,
        [
            "TypeUse Item in def:T",
            "TypeUse pkg.Value in def:T",
            "TypeUse Item in def:G",
            "TypeUse Result in def:G",
        ]
    );
}

#[test]
fn a_local_is_no_reference_and_a_package_name_is() {
    let lines = ref_lines("package p\n\nfunc F(a int) int {\n\tb := a + Base\n\treturn b + c\n}\n");
    assert_eq!(lines, ["Read Base in def:F", "Read c in def:F"]);
}

#[test]
fn package_level_values_own_the_uses_in_their_initialisers() {
    let lines = ref_lines("package p\n\nvar total = Sum(rows)\n\nconst limit = Max + 1\n");
    assert_eq!(
        lines,
        [
            "Call Sum in def:total",
            "PassAsValue rows in def:total",
            "Read Max in def:limit",
        ]
    );
}

#[test]
fn entry_points() {
    let blob = extract(
        "package main\n\nimport \"testing\"\n\nfunc main() {}\n\nfunc TestX(t *testing.T) {}\n\nfunc BenchmarkX(b *testing.B) {}\n\nfunc Testing() {}\n",
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
            (EntryKind::Test, "def:TestX".to_string()),
            (EntryKind::Test, "def:BenchmarkX".to_string()),
        ]
    );
    let library = extract("package lib\n\nfunc main() {}\n");
    assert!(
        !library
            .facts
            .iter()
            .any(|f| matches!(f, Fact::EntryPoint(_)))
    );
}

#[test]
fn twins_of_one_name_are_told_apart_in_source_order() {
    let blob = extract("package p\n\nfunc init() {}\n\nfunc init() {}\n");
    let keys: Vec<String> = blob.defs().map(|d| key(&d.local)).collect();
    assert_eq!(keys, ["init", "init#"]);
}

#[test]
fn a_body_edit_moves_the_body_hash_and_not_the_signature() {
    let before = extract("package p\n\nfunc F() int {\n\treturn 1\n}\n");
    let after = extract("package p\n\nfunc F() int {\n\treturn 2\n}\n");
    let (a, b) = (def(&before, "F"), def(&after, "F"));
    assert_eq!(a.signature, b.signature);
    assert_ne!(a.body_hash, b.body_hash);
    let spaced = extract("package p\n\nfunc F() int {\n\n\t// note\n\treturn   1\n}\n");
    assert_eq!(def(&spaced, "F").body_hash, a.body_hash);
    assert_ne!(def(&spaced, "F").text_hash, a.text_hash);
}

#[test]
fn a_doc_comment_is_the_doc_hash_and_not_the_body() {
    let plain = extract("package p\n\nfunc F() {}\n");
    let documented = extract("package p\n\n// F does a thing.\nfunc F() {}\n");
    assert_eq!(def(&plain, "F").doc_hash, None);
    assert!(def(&documented, "F").doc_hash.is_some());
    assert_eq!(def(&plain, "F").body_hash, def(&documented, "F").body_hash);
}

#[test]
fn facts_come_in_source_order() {
    let blob = extract("package p\n\nfunc B() { A() }\n\nfunc A() {}\n\nvar V = B\n");
    let starts: Vec<u32> = blob.facts.iter().map(|f| f.site().bytes.start).collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted);
}

#[test]
fn a_parse_error_is_a_loss_and_the_rest_is_read() {
    let blob = extract("package p\n\nfunc Good() {}\n\nfunc Bad( {\n");
    assert!(
        blob.losses
            .iter()
            .any(|l| matches!(l, Loss::ParseError { .. })),
        "{:?}",
        blob.losses
    );
    assert!(blob.defs().any(|d| key(&d.local) == "Good"));
}

#[test]
fn an_expired_budget_is_a_deadline_loss() {
    let budget = RunBudget {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..RunBudget::default()
    };
    let blob = GoExtractor.extract(b"package p\n\nfunc A() {}\n", &budget);
    assert!(
        blob.losses.contains(&Loss::Deadline {
            stage: Stage::Extraction
        }),
        "{:?}",
        blob.losses
    );
}

#[test]
fn the_package_clause_names_the_package() {
    let blob = extract("package pricing\n\nfunc Total() {}\n");
    let named: Vec<&str> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::Export(Export::Default {
                local: Some(name), ..
            }) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(named, ["pricing"]);
}
