//! The Python extractor, fact kind by fact kind, on source small enough to
//! read the right answer off. Nothing here runs diffctx to learn an answer.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use _diffctx::facts::lang::python::PythonExtractor;
use _diffctx::facts::{
    BlobFacts, Def, Disambig, Export, Extractor, Fact, Import, ImportKind, Language, Local,
    LocalKey, Receiver, Ref, Scope, TypeName,
};
use _diffctx::snapshot::{Loss, RunBudget, Stage};

fn extract(source: &str) -> BlobFacts {
    PythonExtractor.extract(source.as_bytes(), &RunBudget::default())
}

fn defs(blob: &BlobFacts) -> Vec<&Def> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::Definition(d) => Some(d),
            _ => None,
        })
        .collect()
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

fn locals(blob: &BlobFacts) -> Vec<&Local> {
    blob.facts
        .iter()
        .filter_map(|f| match f {
            Fact::LocalBinding(l) => Some(l),
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

/// `A.m`, with `#n` for the nth twin and `@cfg` for a configuration twin.
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
        Some(other) => format!("~{other:?}"),
    };
    format!("{}{twin}", path.join("."))
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

/// `Call a.b.f` for a reference, with the scope it sits in appended.
fn ref_line(r: &Ref) -> String {
    let qualifier: Vec<&str> = r.qualifier.iter().map(|q| q.as_str()).collect();
    let dotted = if qualifier.is_empty() {
        r.name.as_str().to_string()
    } else {
        format!("{}.{}", qualifier.join("."), r.name.as_str())
    };
    format!("{:?} {dotted} in {}", r.use_kind, scope(&r.enclosing))
}

fn ref_lines(source: &str) -> Vec<String> {
    refs(&extract(source)).into_iter().map(ref_line).collect()
}

fn starts(blob: &BlobFacts) -> Vec<u32> {
    blob.facts
        .iter()
        .map(|f| match f {
            Fact::Definition(d) => d.span.bytes.start,
            Fact::Reference(r) => r.site.bytes.start,
            Fact::Import(i) => i.site.bytes.start,
            Fact::EntryPoint(e) => e.site.bytes.start,
            Fact::PathMention(p) => p.site.bytes.start,
            Fact::LocalBinding(l) => l.site.bytes.start,
            Fact::Export(Export::Named { site, .. }) => site.bytes.start,
            other => panic!("a fact kind Python does not state: {other:?}"),
        })
        .collect()
}

fn parse_errors(blob: &BlobFacts) -> Vec<&Vec<(u32, u32)>> {
    blob.losses
        .iter()
        .filter_map(|l| match l {
            Loss::ParseError { lines, .. } => Some(lines),
            _ => None,
        })
        .collect()
}

#[test]
fn definitions_by_kind_and_container() {
    let table: &[(&str, &[&str])] = &[
        ("def f(): pass\n", &["Function f"]),
        (
            "class A:\n    def m(self): pass\n    def __init__(self): pass\n",
            &["Class A", "Method A.m", "Constructor A.__init__"],
        ),
        (
            "def outer():\n    def inner(): pass\n    class K:\n        def m(self): pass\n    return inner, K\n",
            &[
                "Function outer",
                "Function outer.inner",
                "Class outer.K",
                "Method outer.K.m",
            ],
        ),
        (
            "def outer():\n    def inner(): pass\n    class K:\n        def m(self): pass\n",
            &["Function outer"],
        ),
        (
            "MAX = 1\nname = 2\nclass C:\n    attr = 3\n    def __init__(self):\n        self.field = 4\n",
            &[
                "Constant MAX",
                "Variable name",
                "Class C",
                "Field C.attr",
                "Constructor C.__init__",
                "Field C.field",
            ],
        ),
        ("@dec\nasync def f(): pass\n", &["Function f"]),
        (
            "class A:\n    @property\n    def x(self): return 1\n",
            &["Class A", "Property A.x"],
        ),
        ("a, b = 1, 2\n", &["Variable a", "Variable b"]),
        (
            "def f(a):\n    x = a\n    for i in a: pass\n",
            &["Function f"],
        ),
        ("x = 1\nx = 2\n", &["Variable x"]),
        (
            "if __name__ == \"__main__\":\n    app = 1\n",
            &["Variable app"],
        ),
        ("X: int = 1\n", &["Constant X"]),
    ];
    for (source, expected) in table {
        assert_eq!(&lines_of_defs(source), expected, "{source}");
    }
}

#[test]
fn a_second_assignment_is_a_write_not_a_second_definition() {
    assert_eq!(ref_lines("x = 1\nx = 2\n"), ["Write x in module"]);
}

#[test]
fn an_instance_attribute_is_a_field_of_its_class_once() {
    let source = "class A:\n    def __init__(self):\n        self.x = 1\n    def reset(self):\n        self.x = 0\n";
    let blob = extract(source);
    let keys: Vec<String> = defs(&blob).iter().map(|d| key(&d.local)).collect();
    assert_eq!(keys, ["A", "A.__init__", "A.x", "A.reset"]);
    let writes: Vec<String> = refs(&blob).into_iter().map(ref_line).collect();
    assert_eq!(writes, ["Write self.x in def:A.reset"]);
    assert_eq!(refs(&blob)[0].receiver, Receiver::SelfType);
}

#[test]
fn decorators_are_names_on_the_def_and_references_from_it() {
    let source = "@app.route(\"/x\")\n@functools.lru_cache\ndef f(): pass\n";
    let blob = extract(source);
    let f = def(&blob, "f");
    let names: Vec<&str> = f.decorators.iter().map(|d| d.as_str()).collect();
    assert_eq!(names, ["app.route(\"/x\")", "functools.lru_cache"]);
    assert_eq!(
        f.span.lines,
        (1, 3),
        "the span starts at the first decorator"
    );
    assert_eq!(
        ref_lines(source),
        [
            "Decorate app.route in def:f",
            "Decorate functools.lru_cache in def:f"
        ]
    );

    let moved = extract("@app.route(\"/y\")\n@functools.lru_cache\ndef f(): pass\n");
    assert_ne!(def(&moved, "f").decorators, f.decorators);
    assert_eq!(def(&moved, "f").body_hash, f.body_hash);
}

#[test]
fn a_nested_def_that_leaves_its_function_has_its_own_key_scope_and_hashes() {
    let source = "def outer(x):\n    def inner(y):\n        return helper(y)\n    return inner\n";
    let blob = extract(source);
    let keys: Vec<String> = defs(&blob).iter().map(|d| key(&d.local)).collect();
    assert_eq!(keys, ["outer", "outer.inner"]);
    let lines = ref_lines(source);
    for wanted in [
        "Call helper in def:outer.inner",
        "PassAsValue y in def:outer.inner",
    ] {
        assert!(
            lines.contains(&wanted.to_string()),
            "{wanted} not in {lines:?}"
        );
    }

    let edited = extract(&source.replace("helper(y)", "other(y)"));
    assert_eq!(
        def(&edited, "outer").body_hash,
        def(&blob, "outer").body_hash
    );
    assert_eq!(
        def(&edited, "outer").text_hash,
        def(&blob, "outer").text_hash
    );
    assert_ne!(
        def(&edited, "outer.inner").body_hash,
        def(&blob, "outer.inner").body_hash
    );
}

#[test]
fn a_nested_def_that_never_leaves_is_a_local_of_its_function() {
    let source =
        "def outer(x):\n    def inner(y):\n        return helper(y)\n    return inner(x)\n";
    let blob = extract(source);
    assert_eq!(lines_of_defs(source), ["Function outer"]);
    assert_eq!(
        ref_lines(source),
        [
            "Call helper in def:outer",
            "PassAsValue y in def:outer",
            "Call inner in def:outer",
            "PassAsValue x in def:outer",
        ]
    );
    let bound: Vec<String> = locals(&blob)
        .iter()
        .map(|l| {
            format!(
                "{:?} {} in {}",
                l.kind,
                l.name.as_str(),
                scope(&l.enclosing)
            )
        })
        .collect();
    assert_eq!(
        bound,
        [
            "Parameter x in def:outer",
            "Closure inner in def:outer",
            "Parameter y in def:outer",
        ]
    );
    // Its change is its function's.
    let edited = extract(&source.replace("helper(y)", "other(y)"));
    assert_ne!(
        def(&edited, "outer").body_hash,
        def(&blob, "outer").body_hash
    );
}

#[test]
fn a_nested_def_or_class_is_a_definition_when_it_leaves_and_a_local_when_it_does_not() {
    let table: &[(&str, &[&str])] = &[
        (
            "def f():\n    def a(): pass\n    return a\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f():\n    def a(): pass\n    return {'k': a}\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f(obj):\n    def a(): pass\n    obj.hook = a\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f(self):\n    def a(): pass\n    self.cb = a\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f():\n    global REG\n    def a(): pass\n    REG = a\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f():\n    def a(): pass\n    register(a)\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f():\n    def a(): pass\n    xs = [a]\n    return xs\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f():\n    @register\n    def a(): pass\n",
            &["Function f", "Function f.a"],
        ),
        (
            "def f():\n    class K:\n        def m(self): pass\n    return K()\n",
            &["Function f", "Class f.K", "Method f.K.m"],
        ),
        (
            "def f():\n    def a():\n        def b(): pass\n        return b\n    return a\n",
            &["Function f", "Function f.a", "Function f.a.b"],
        ),
        ("def f():\n    def a(): pass\n    a()\n", &["Function f"]),
        // A parameter of the same name is that parameter, not the function.
        (
            "def f():\n    def a(): pass\n    def g(a):\n        return a\n    return g\n",
            &["Function f", "Function f.g"],
        ),
        (
            "def f():\n    def a(): pass\n    return map(a, [])\n",
            &["Function f"],
        ),
        (
            "def f():\n    def a(): pass\n    return sorted([], key=a)\n",
            &["Function f"],
        ),
        (
            "def f():\n    def a(): pass\n    xs = [a]\n    return len(xs)\n",
            &["Function f"],
        ),
        (
            "def f():\n    @functools.wraps(g)\n    def a(): pass\n",
            &["Function f"],
        ),
        (
            "def f():\n    class K:\n        def m(self): pass\n    K().m()\n",
            &["Function f"],
        ),
        (
            "def f():\n    def a():\n        def b(): pass\n        return b\n    return a()\n",
            &["Function f"],
        ),
        (
            "class C:\n    def run(self):\n        def helper(): pass\n        helper()\n",
            &["Class C", "Method C.run"],
        ),
    ];
    for (source, expected) in table {
        assert_eq!(&lines_of_defs(source), expected, "{source}");
    }
}

#[test]
fn adding_a_method_leaves_its_class_as_it_was() {
    let before = extract("class A:\n    x = 1\n");
    let after = extract("class A:\n    x = 1\n\n    def m(self):\n        return 2\n");
    for hash in [|d: &Def| d.body_hash, |d: &Def| d.text_hash] {
        assert_eq!(hash(def(&before, "A")), hash(def(&after, "A")));
    }
    assert_eq!(def(&before, "A.x"), def(&after, "A.x"));
}

#[test]
fn parameters_and_locals_are_bindings_not_definitions() {
    let source = "def f(a, b=1, *args, c: int = 2, **kw):\n    x = a\n    for i in b:\n        pass\n    with open(a) as fh:\n        pass\n    total = [y for y in args]\n    return lambda z: z + x\n";
    let blob = extract(source);
    assert_eq!(lines_of_defs(source), ["Function f"]);
    let bound: Vec<String> = locals(&blob)
        .iter()
        .map(|l| {
            format!(
                "{:?} {} in {}",
                l.kind,
                l.name.as_str(),
                scope(&l.enclosing)
            )
        })
        .collect();
    let expected: Vec<String> = [
        "Parameter a",
        "Parameter b",
        "Parameter args",
        "Parameter c",
        "Parameter kw",
        "Variable x",
        "Variable i",
        "Variable fh",
        "Variable total",
        "Variable y",
        "Parameter z",
    ]
    .iter()
    .map(|l| format!("{l} in def:f"))
    .collect();
    assert_eq!(bound, expected);
}

#[test]
fn an_f_string_interpolation_references_and_a_plain_string_does_not() {
    let lines = ref_lines("def f(x):\n    return f\"{g(x)} {y.z}\" + \"{h(x)}\"\n");
    assert_eq!(
        lines,
        [
            "Call g in def:f",
            "PassAsValue x in def:f",
            "Read y.z in def:f",
        ]
    );
}

#[test]
fn a_comprehension_binds_its_variable_and_reads_the_rest() {
    let blob = extract("def f(xs):\n    return [g(x) for x in xs if h(x)]\n");
    let calls: Vec<String> = refs(&blob)
        .into_iter()
        .map(ref_line)
        .filter(|l| l.starts_with("Call"))
        .collect();
    assert_eq!(calls, ["Call g in def:f", "Call h in def:f"]);
    assert!(
        locals(&blob)
            .iter()
            .any(|l| l.name.as_str() == "x" && scope(&l.enclosing) == "def:f")
    );
    assert!(
        ref_lines("def f(xs):\n    return [x for x in xs]\n")
            .contains(&"Read xs in def:f".to_string())
    );
}

#[test]
fn a_lambda_is_transparent_to_the_def_around_it() {
    assert_eq!(
        ref_lines("def f():\n    g = lambda: helper()\n    return g\n"),
        ["Call helper in def:f", "Read g in def:f"]
    );
}

#[test]
fn the_main_guard_is_module_scope_and_an_entry_not_a_caller() {
    use _diffctx::facts::EntryKind;
    let source = "def main():\n    helper()\n\nif __name__ == \"__main__\":\n    main()\n";
    let blob = extract(source);
    let entries: Vec<_> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::EntryPoint(e) => Some(e),
            _ => None,
        })
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(matches!(entries[0].kind, EntryKind::ScriptGuard));
    assert_eq!(scope(&entries[0].target), "module");
    assert_eq!(entries[0].site.lines, (4, 5));
    assert_eq!(
        ref_lines(source),
        ["Call helper in def:main", "Call main in module"]
    );
    assert_eq!(lines_of_defs(source), ["Function main"]);
}

/// A name a loop body binds is rebound on every pass: a local of the loop, not
/// a name another file imports, and no definition to report a change of (fj#94).
#[test]
fn a_binding_in_a_loop_body_is_a_local_not_a_definition() {
    for source in [
        "for n in range(3):\n    b = n\n",
        "while go():\n    c = 1\n",
        "for n in range(3):\n    if n:\n        d = n\n",
    ] {
        assert_eq!(lines_of_defs(source), Vec::<String>::new(), "{source}");
        assert!(!locals(&extract(source)).is_empty(), "{source}");
    }
    let kept = "for n in range(3):\n    def f(): pass\n";
    assert_eq!(lines_of_defs(kept), ["Function f"]);
}

/// Python has no block scope: a name bound in a module-level `if`, `with`,
/// `try` or `match` (a script's main guard among them) is a module variable, and
/// a read of it resolves to that definition instead of passing as a local.
#[test]
fn a_binding_in_a_module_level_compound_statement_is_a_module_definition() {
    let table: &[(&str, &str)] = &[
        ("if ready:\n    a = 1\nelse:\n    a = 2\n", "Variable a"),
        ("with open(p) as f:\n    d = f.read()\n", "Variable d"),
        (
            "try:\n    e = load()\nexcept OSError:\n    e = None\n",
            "Variable e",
        ),
        (
            "match cmd:\n    case 1:\n        g = 1\n    case _:\n        g = 2\n",
            "Variable g",
        ),
        (
            "if __name__ == \"__main__\":\n    x = f(1)\n    print(x)\n",
            "Variable x",
        ),
    ];
    for (source, wanted) in table {
        assert!(
            lines_of_defs(source).iter().any(|line| line == wanted),
            "{wanted} missing in {:?} for {source}",
            lines_of_defs(source)
        );
    }
    let guarded = "if __name__ == \"__main__\":\n    x = f(1)\n    print(x)\n";
    let lines = ref_lines(guarded);
    assert!(
        lines.contains(&"Read x in module".to_string())
            || lines.contains(&"PassAsValue x in module".to_string()),
        "the read of x is a use of the module variable: {lines:?}"
    );
    assert!(locals(&extract(guarded)).is_empty());
}

/// What `importlib.import_module(name)` loads is not in the text: the import is a
/// fact of its own, where the literal one is an import.
#[test]
fn an_import_the_code_computes_is_a_computed_import_and_a_literal_one_is_not() {
    let blob = extract(
        "import importlib\n\n\ndef load(name):\n    a = importlib.import_module(name)\n    b = importlib.import_module(\"pkg.mod\")\n    c = __import__(f\"x.{name}\")\n    d = __import__(\"y\")\n    return a, b, c, d\n",
    );
    let computed: Vec<(u32, String, ImportKind)> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::ComputedImport(c) => Some((c.site.lines.0, scope(&c.enclosing), c.kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        computed,
        [
            (5, "def:load".to_string(), ImportKind::Dynamic),
            (7, "def:load".to_string(), ImportKind::Dynamic),
        ]
    );
    let literal: Vec<&str> = imports(&blob)
        .iter()
        .filter(|i| i.kind == ImportKind::Dynamic)
        .map(|i| i.module.as_str())
        .collect();
    assert_eq!(literal, ["pkg.mod", "y"]);
}

#[test]
fn a_guard_inside_a_function_is_no_entry() {
    let blob = extract("def f():\n    if __name__ == '__main__':\n        pass\n");
    assert!(blob.facts.iter().all(|f| !matches!(f, Fact::EntryPoint(_))));
}

#[test]
fn sys_path_edits_name_the_path_they_add() {
    let table: &[(&str, Option<&str>)] = &[
        ("import sys\nsys.path.insert(0, \"lib\")\n", Some("lib")),
        (
            "import os, sys\nsys.path.append(os.path.join(os.path.dirname(__file__), \"..\", \"src\"))\n",
            Some("__file__/../../src"),
        ),
        (
            "HERE = Path(__file__).resolve().parent\nsys.path.insert(0, str(HERE / \"vendor\"))\n",
            Some("__file__/../vendor"),
        ),
        (
            "sys.path.insert(0, str(Path(__file__).parents[1] / \"src\"))\n",
            Some("__file__/../../src"),
        ),
        ("sys.path.insert(0, os.environ[\"X\"])\n", None),
    ];
    for (source, expected) in table {
        let blob = extract(source);
        let mentioned: Vec<&str> = blob
            .facts
            .iter()
            .filter_map(|f| match f {
                Fact::PathMention(p) => Some(p.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            mentioned,
            expected.iter().copied().collect::<Vec<_>>(),
            "{source}"
        );
    }
}

/// A path chain longer than the evaluator follows is a loss, not a path
/// mention quietly left out. Red on the base: the depth cut returned `None`,
/// the same answer as for an expression that is no path. A name that holds an
/// expression of itself is no path either, but no cut.
#[test]
fn a_path_chain_past_the_cut_is_a_loss_and_a_self_reference_is_not() {
    let deep = extract(&format!(
        "sys.path.append(Path(__file__){})\n",
        ".parent".repeat(12)
    ));
    assert!(
        deep.losses
            .iter()
            .any(|l| matches!(l, Loss::ExtractorFailed { .. })),
        "{:?}",
        deep.losses
    );
    let shallow = extract("sys.path.append(Path(__file__).parent.parent)\n");
    assert!(shallow.losses.is_empty(), "{:?}", shallow.losses);
    let selfish = extract("HERE = HERE / \"x\"\nsys.path.append(HERE)\n");
    assert!(selfish.losses.is_empty(), "{:?}", selfish.losses);
}

#[test]
fn a_docstring_is_a_doc_hash_and_not_part_of_the_body() {
    let plain = extract("def f():\n    \"\"\"Compute the\n    total.\"\"\"\n    return 1\n");
    let reflowed = extract("def f():\n    \"\"\"Compute   the total.\"\"\"\n    return 1\n");
    let reworded = extract("def f():\n    \"\"\"Compute the sum.\"\"\"\n    return 1\n");
    let bare = extract("def f():\n    return 1\n");
    let (plain, reflowed, reworded, bare) = (
        def(&plain, "f"),
        def(&reflowed, "f"),
        def(&reworded, "f"),
        def(&bare, "f"),
    );
    assert!(plain.doc_hash.is_some());
    assert_eq!(plain.doc_hash, reflowed.doc_hash);
    assert_ne!(plain.doc_hash, reworded.doc_hash);
    assert_eq!(bare.doc_hash, None);
    assert_ne!(plain.text_hash, reflowed.text_hash);
    for other in [reflowed, reworded, bare] {
        assert_eq!(plain.body_hash, other.body_hash);
        assert_eq!(plain.signature, other.signature);
    }
}

#[test]
fn a_property_and_its_setter_are_twins_of_one_key() {
    let blob = extract(
        "class A:\n    @property\n    def x(self): return 1\n    @x.setter\n    def x(self, v): pass\n",
    );
    let lines: Vec<String> = defs(&blob)
        .iter()
        .map(|d| format!("{:?} {}", d.kind, key(&d.local)))
        .collect();
    assert_eq!(lines, ["Class A", "Property A.x", "Property A.x#1"]);
    let setter: Vec<&str> = def(&blob, "A.x#1")
        .decorators
        .iter()
        .map(|d| d.as_str())
        .collect();
    assert_eq!(setter, ["x.setter"]);
}

#[test]
fn overload_stubs_are_numbered_and_the_implementation_keeps_the_key() {
    let blob = extract(
        "@overload\ndef f(a: int) -> int: ...\n@overload\ndef f(a: str) -> str: ...\ndef f(a): return a\n",
    );
    let keys: Vec<String> = defs(&blob).iter().map(|d| key(&d.local)).collect();
    assert_eq!(keys, ["f#1", "f#2", "f"]);
}

#[test]
fn a_type_checking_twin_is_told_apart_by_its_branch() {
    let blob = extract(
        "if TYPE_CHECKING:\n    def f(a: int) -> int: ...\nelse:\n    def f(a): return a\n",
    );
    let keys: Vec<String> = defs(&blob).iter().map(|d| key(&d.local)).collect();
    assert_eq!(keys, ["f@TYPE_CHECKING", "f"]);
}

#[test]
fn a_plain_redefinition_is_a_numbered_twin() {
    let keys: Vec<String> = defs(&extract("def f(): return 1\ndef f(): return 2\n"))
        .iter()
        .map(|d| key(&d.local))
        .collect();
    assert_eq!(keys, ["f", "f#1"]);
}

#[test]
fn a_statement_moved_between_blocks_changes_the_body() {
    let base = extract("def f(x):\n    if x:\n        a()\n    b()\n");
    let moved = extract("def f(x):\n    if x:\n        a()\n        b()\n");
    let (base, moved) = (def(&base, "f"), def(&moved, "f"));
    assert_ne!(base.body_hash, moved.body_hash);
    assert_eq!(base.signature, moved.signature);
}

#[test]
fn a_pure_re_indent_keeps_the_body_and_changes_the_text() {
    let four = extract("def f(x):\n    if x:\n        a()\n    b()\n");
    let two = extract("def f(x):\n  if x:\n    a()\n  b()\n");
    let nested = extract("if True:\n    def f(x):\n        if x:\n            a()\n        b()\n");
    let commented = extract("def f(x):\n    if x:\n        a()  # why\n    b()\n");
    let four = def(&four, "f");
    for same_body in [def(&two, "f"), def(&nested, "f"), def(&commented, "f")] {
        assert_eq!(four.body_hash, same_body.body_hash);
        assert_eq!(four.signature, same_body.signature);
        assert_eq!(four.doc_hash, same_body.doc_hash);
        assert_eq!(four.decorators, same_body.decorators);
        assert_ne!(four.text_hash, same_body.text_hash);
    }
}

#[test]
fn a_changed_parameter_list_is_a_signature_not_a_new_key() {
    let before = extract("def f(x):\n    return x\n");
    let after = extract("def f(x, y=0):\n    return x\n");
    let (before, after) = (def(&before, "f"), def(&after, "f"));
    assert_eq!(before.local, after.local);
    assert_eq!(before.signature.as_deref(), Some("def f(x)"));
    assert_eq!(after.signature.as_deref(), Some("def f(x, y = 0)"));
    assert_eq!(before.body_hash, after.body_hash);
}

#[test]
fn imports_state_module_names_and_relative_depth() {
    let table: &[(&str, &[&str])] = &[
        ("import os\n", &["os|0||-|-|Static"]),
        ("import a.b.c as d\n", &["a.b.c|0||-|d|Static"]),
        (
            "import a, b as c\n",
            &["a|0||-|-|Static", "b|0||-|c|Static"],
        ),
        (
            "from x.y import a, b as c\n",
            &["x.y|0|a,b as c|-|-|Static"],
        ),
        ("from . import z\n", &["|1|z|-|-|Static"]),
        ("from ..p.q import r\n", &["p.q|2|r|-|-|Static"]),
        ("from m import *\n", &["m|0||*|-|Static"]),
        (
            "from __future__ import annotations\n",
            &["__future__|0|annotations|-|-|Static"],
        ),
        (
            "if TYPE_CHECKING:\n    from m import T\nimport n\n",
            &["m|0|T|-|-|TypeOnly", "n|0||-|-|Static"],
        ),
        (
            "import importlib\nm = importlib.import_module(\"pkg.mod\")\n",
            &["importlib|0||-|-|Static", "pkg.mod|0||-|-|Dynamic"],
        ),
    ];
    for (source, expected) in table {
        let blob = extract(source);
        let lines: Vec<String> = imports(&blob)
            .iter()
            .map(|i| {
                let names: Vec<String> = i
                    .names
                    .iter()
                    .map(|(n, a)| match a {
                        Some(a) => format!("{} as {}", n.as_str(), a.as_str()),
                        None => n.as_str().to_string(),
                    })
                    .collect();
                format!(
                    "{}|{}|{}|{}|{}|{:?}",
                    i.module.as_str(),
                    i.relative,
                    names.join(","),
                    if i.star.is_star() { "*" } else { "-" },
                    i.default.as_ref().map_or("-", |d| d.as_str()),
                    i.kind
                )
            })
            .collect();
        assert_eq!(&lines, expected, "{source}");
    }
}

#[test]
fn an_import_in_a_function_belongs_to_that_function() {
    let blob = extract("def f():\n    import json\n");
    assert_eq!(scope(&imports(&blob)[0].enclosing), "def:f");
}

#[test]
fn dunder_all_exports_its_strings() {
    let blob = extract("__all__ = [\"a\", 'b']\n__all__ += (\"c\",)\n");
    let names: Vec<&str> = blob
        .facts
        .iter()
        .filter_map(|f| match f {
            Fact::Export(Export::Named { name, .. }) => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["a", "b", "c"]);
}

#[test]
fn annotations_are_type_uses_and_type_the_receiver() {
    let source = "def f(x: Foo, y: \"mod.Bar\" = None) -> list[Baz]:\n    return x.run()\n";
    let blob = extract(source);
    let type_uses: Vec<String> = refs(&blob)
        .into_iter()
        .map(ref_line)
        .filter(|l| l.starts_with("TypeUse"))
        .collect();
    assert_eq!(
        type_uses,
        [
            "TypeUse Foo in def:f",
            "TypeUse mod.Bar in def:f",
            "TypeUse list in def:f",
            "TypeUse Baz in def:f",
        ]
    );
    let run = refs(&blob)
        .into_iter()
        .find(|r| r.name.as_str() == "run")
        .expect("x.run()");
    assert_eq!(run.receiver, Receiver::Typed(TypeName::new("Foo")));
}

#[test]
fn a_call_states_how_its_receiver_was_written() {
    let source = "class A:\n    def m(self, o: B):\n        self.run()\n        self.f.go()\n        o.use()\n        B().make()\n        super().m()\n        free()\n";
    let blob = extract(source);
    let calls: Vec<(String, Receiver)> = refs(&blob)
        .into_iter()
        .filter(|r| {
            format!("{:?}", r.use_kind) == "Call" && !matches!(r.name.as_str(), "B" | "super")
        })
        .map(|r| (ref_line(r), r.receiver.clone()))
        .collect();
    let named = |parts: &[&str]| {
        parts
            .iter()
            .map(|p| _diffctx::facts::Name::new(*p))
            .collect()
    };
    assert_eq!(
        calls,
        [
            ("Call self.run in def:A.m".to_string(), Receiver::SelfType),
            ("Call self.f.go in def:A.m".to_string(), Receiver::Unknown),
            (
                "Call o.use in def:A.m".to_string(),
                Receiver::Typed(TypeName::new("B"))
            ),
            (
                "Call make in def:A.m".to_string(),
                Receiver::CallResult(named(&["B"]))
            ),
            (
                "Call m in def:A.m".to_string(),
                Receiver::CallResult(named(&["super"]))
            ),
            ("Call free in def:A.m".to_string(), Receiver::None),
        ]
    );
}

#[test]
fn a_name_handed_to_a_call_is_passed_as_a_value() {
    assert_eq!(
        ref_lines("map(func, items)\nThread(target=self.run)\n"),
        [
            "Call map in module",
            "PassAsValue func in module",
            "PassAsValue items in module",
            "Call Thread in module",
            "PassAsValue self.run in module",
        ]
    );
}

#[test]
fn visibility_follows_the_underscore_convention() {
    let source = "def pub(): pass\ndef _priv(): pass\ndef __dunder__(): pass\nclass C:\n    def _prot(self): pass\n    def __mangled(self): pass\n    def run(self):\n        def helper(): pass\n        return helper\n";
    let blob = extract(source);
    let seen: Vec<String> = defs(&blob)
        .iter()
        .map(|d| format!("{} {:?}", key(&d.local), d.visibility))
        .collect();
    assert_eq!(
        seen,
        [
            "pub Public",
            "_priv Private",
            "__dunder__ Public",
            "C Public",
            "C._prot Protected",
            "C.__mangled Private",
            "C.run Public",
            "C.run.helper Private",
        ]
    );
}

#[test]
fn a_parse_with_errors_gives_the_loss_and_what_was_read() {
    let blob = extract("def ok():\n    return 1\n\ndef broken(:\n    x = (\n");
    let errors = parse_errors(&blob);
    assert_eq!(errors.len(), 1, "{:?}", blob.losses);
    assert!(
        errors[0].iter().any(|(from, to)| *from <= 5 && 4 <= *to),
        "{errors:?}"
    );
    assert!(defs(&blob).iter().any(|d| key(&d.local) == "ok"));
    assert!(parse_errors(&extract("def ok():\n    return 1\n")).is_empty());
}

#[test]
fn facts_come_in_source_order_and_the_same_every_time() {
    let source = "import os\nfrom . import z\n__all__ = [\"f\"]\n\n@dec\ndef f(a: int, b=g()):\n    \"\"\"Doc.\"\"\"\n    x = a.b(b)\n    return [y for y in x]\n\nclass C(Base):\n    attr = 1\n    def m(self):\n        self.v = 2\n\nsys.path.insert(0, \"lib\")\nif __name__ == \"__main__\":\n    f(1)\n";
    let first = extract(source);
    let positions = starts(&first);
    assert!(positions.len() > 20, "{}", positions.len());
    let mut sorted = positions.clone();
    sorted.sort_unstable();
    assert_eq!(
        positions[1..],
        sorted[1..],
        "exports aside, in source order"
    );
    assert_eq!(format!("{first:?}"), format!("{:?}", extract(source)));
}

#[test]
fn a_script_without_extension_reads_as_python() {
    let bin = _diffctx::snapshot::RepoPath::new("bin/tool").expect("a path");
    for head in [
        "#!/usr/bin/env python3\n",
        "#!/usr/bin/python\n",
        "#!/usr/bin/env -S python3 -u\n",
    ] {
        assert_eq!(
            Language::detect(&bin, head.as_bytes()),
            Some(Language::Python),
            "{head}"
        );
    }
    // A shell shebang makes the file shell, since the shell has facts of its own;
    // an interpreter no extractor reads makes it nothing.
    assert_eq!(
        Language::detect(&bin, b"#!/bin/sh\n"),
        Some(Language::Shell)
    );
    assert_eq!(Language::detect(&bin, b"#!/usr/bin/perl\n"), None);
    let only = extract("#!/usr/bin/env python3\n");
    assert!(only.facts.is_empty() && only.losses.is_empty());

    let source =
        "#!/usr/bin/env python3\nimport sys\n\ndef main():\n    return sys.argv\n\nmain()\n";
    let blob = extract(source);
    assert!(blob.losses.is_empty(), "{:?}", blob.losses);
    assert_eq!(lines_of_defs(source), ["Function main"]);
    assert_eq!(imports(&blob).len(), 1);
    assert!(ref_lines(source).contains(&"Call main in module".to_string()));
}

#[test]
fn an_expired_run_says_so_instead_of_returning_a_clean_file() {
    let budget = RunBudget {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..RunBudget::default()
    };
    let blob = PythonExtractor.extract(b"def f():\n    return 1\n", &budget);
    assert!(
        blob.losses.contains(&Loss::Deadline {
            stage: Stage::Extraction
        }),
        "{:?}",
        blob.losses
    );
    assert!(parse_errors(&blob).is_empty());
}

/// W5-R1 (R2): a module-level statement that mutates a module variable is part
/// of that variable's value, so its edit changes the variable's hashes and
/// nothing else's. A statement that only reads it, one in a function, in the
/// main guard, or a method that does not change a dict, list or set, does not.
#[test]
fn a_module_level_mutation_is_part_of_the_variable_it_changes() {
    let body = |source: &str, name: &str| {
        let blob = extract(source);
        let found = def(&blob, name);
        (found.body_hash, found.text_hash)
    };
    let base = "TABLE = {\"a\": 1}\nOTHER = 2\n";
    let (table, other) = (body(base, "TABLE"), body(base, "OTHER"));
    for (shape, statement) in [
        ("item", "TABLE[\"b\"] = 2"),
        ("attribute", "TABLE.note = 1"),
        ("delete", "del TABLE[\"a\"]"),
        ("method", "TABLE.pop(\"a\")"),
        ("update", "TABLE.update({\"b\": 2})"),
    ] {
        let source = format!("{base}\nif OTHER:\n    {statement}\n");
        assert_ne!(body(&source, "TABLE").0, table.0, "{shape}");
        assert_ne!(body(&source, "TABLE").1, table.1, "{shape}");
        assert_eq!(body(&source, "OTHER"), other, "{shape}");
    }
    let augmented = "COUNT = 1\nCOUNT += 1\n";
    assert_ne!(
        body(augmented, "COUNT").0,
        body("COUNT = 1\n", "COUNT").0,
        "augmented"
    );
    // A reformat of the statement is a change of the text and not of the tree.
    let spaced = format!("{base}TABLE.pop( \"a\" )\n");
    let tight = format!("{base}TABLE.pop(\"a\")\n");
    assert_eq!(body(&spaced, "TABLE").0, body(&tight, "TABLE").0);
    assert_ne!(body(&spaced, "TABLE").1, body(&tight, "TABLE").1);
    for (shape, source) in [
        ("a read", format!("{base}print(TABLE[\"a\"])\n")),
        ("a call that reads", format!("{base}TABLE.get(\"a\")\n")),
        (
            "a function",
            format!("{base}def f():\n    TABLE[\"b\"] = 2\n"),
        ),
        (
            "the main guard",
            format!("{base}if __name__ == \"__main__\":\n    TABLE.pop(\"a\")\n"),
        ),
        (
            "a method of a value that is no container",
            "N = 1\nN.pop(2)\n".to_string(),
        ),
        ("an unbound name", format!("{base}MISSING[\"b\"] = 2\n")),
    ] {
        let name = if shape.starts_with("a method") {
            "N"
        } else {
            "TABLE"
        };
        let plain = if name == "N" { "N = 1\n" } else { base };
        assert_eq!(body(&source, name), body(plain, name), "{shape}");
    }
}
