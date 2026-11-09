import pytest

import syntax
from syntax import definitions

RUST = b"""use std::fmt;

/// docs
#[derive(Debug)]
pub struct Shop {
    n: u32,
}

impl Shop {
    pub fn new() -> Self {
        Shop { n: 0 }
    }
    #[inline]
    fn total(&self, items: &[u32]) -> u32 {
        fn inner() {}
        helper(items)
    }
}

impl fmt::Display for Shop {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

pub trait Priced {
    fn price(&self) -> u32;
    type Out;
}

mod inner {
    pub fn f() {}
}

enum E { A, B(u32) }
type Id = u32;
pub const RATE: u32 = 2;
"""

GO = b"""package shop

// Rate doc
const Rate = 2

var (
	Limit = 3
	Other = 4
)

type Shop struct {
	n, m int
}

type Priced interface {
	Price() int
}

type Alias = Shop

// Total does.
func (s *Shop) Total(items []int) int {
	return helper(items)
}

func New() *Shop { return &Shop{n: 0} }
"""

KOTLIN = b"""package shop

const val RATE = 2

/** doc */
@Deprecated("x")
class Shop(val n: Int, other: String) : Base() {
    val limit = 3
    fun total(items: List<Int>): Int = helper(items)
    override fun price(): Int {
        fun local() {}
        return 1
    }
    companion object {
        fun create() = Shop(0, "")
    }
    constructor() : this(0, "")
    class Inner
}

interface Priced {
    fun price(): Int
}

object Single {
    val x = 1
}

fun String.ext(x: Int): Int {
    return x
}

typealias Id = Int

enum class E {
    A,
    B
}
"""


def keys(path: str, source: bytes) -> dict:
    return {d.key: d for d in definitions(path, source)}


def test_rust_definitions_follow_impl_blocks_and_take_attributes_and_docs():
    found = keys("a.rs", RUST)

    assert set(found) == {
        "a.rs::Shop",
        "a.rs::Shop.n",
        "a.rs::Shop.new",
        "a.rs::Shop.total",
        "a.rs::Shop.total.inner",
        "a.rs::Shop.fmt",
        "a.rs::Priced",
        "a.rs::Priced.price",
        "a.rs::Priced.Out",
        "a.rs::inner",
        "a.rs::inner.f",
        "a.rs::E",
        "a.rs::E.A",
        "a.rs::E.B",
        "a.rs::Id",
        "a.rs::RATE",
    }
    shop = found["a.rs::Shop"]
    assert (shop.start, shop.end, shop.decorated, shop.kind) == (3, 7, True, "struct")
    assert (found["a.rs::Shop.total"].start, found["a.rs::Shop.total"].end) == (13, 17)
    assert found["a.rs::inner.f"].kind == "function" and found["a.rs::RATE"].kind == "variable"


def test_go_definitions_cover_grouped_declarations_methods_and_fields():
    found = keys("a.go", GO)

    assert set(found) == {
        "a.go::Rate",
        "a.go::Limit",
        "a.go::Other",
        "a.go::Shop",
        "a.go::Shop.n",
        "a.go::Shop.m",
        "a.go::Priced",
        "a.go::Priced.Price",
        "a.go::Alias",
        "a.go::Shop.Total",
        "a.go::New",
    }
    assert found["a.go::Shop.Total"].kind == "method" and found["a.go::Shop"].kind == "struct"
    assert found["a.go::Priced"].kind == "interface"
    assert (found["a.go::Rate"].start, found["a.go::Rate"].end) == (3, 4)
    assert (found["a.go::Limit"].start, found["a.go::Limit"].end) == (7, 7)
    assert (found["a.go::Shop.Total"].start, found["a.go::Shop.Total"].end) == (21, 24)


def test_kotlin_definitions_cover_classes_members_companions_and_extensions():
    found = keys("a.kt", KOTLIN)

    assert {
        "a.kt::RATE",
        "a.kt::Shop",
        "a.kt::Shop.n",
        "a.kt::Shop.limit",
        "a.kt::Shop.total",
        "a.kt::Shop.price",
        "a.kt::Shop.price.local",
        "a.kt::Shop.Companion.create",
        "a.kt::Shop.constructor",
        "a.kt::Shop.Inner",
        "a.kt::Priced",
        "a.kt::Priced.price",
        "a.kt::Single",
        "a.kt::Single.x",
        "a.kt::ext",
        "a.kt::Id",
        "a.kt::E",
        "a.kt::E.A",
        "a.kt::E.B",
    } <= set(found)
    assert found["a.kt::Priced"].kind == "interface" and found["a.kt::Shop.total"].kind == "method"
    assert (found["a.kt::Shop"].start, found["a.kt::Shop"].end) == (5, 19)


@pytest.mark.parametrize(
    ("path", "text", "needle", "is_class", "expected"),
    [
        ("a.rs", "fn f() { helper(items); }", "helper", False, "calls"),
        ("a.rs", "fn f() { s.total(1); }", "total", False, "calls"),
        ("a.rs", "fn f() { Shop::new(); }", "new", False, "calls"),
        ("a.rs", "fn f() { run(total); }", "total", False, "passes_as_value"),
        ("a.rs", "fn f() { let s = Shop { n: 0 }; }", "Shop", True, "instantiates"),
        ("a.rs", "fn f() { let x = total; }", "total", False, "passes_as_value"),
        ("a.rs", "use crate::walk::Worker;", "Worker", False, "imports"),
        ("a.rs", "fn f(w: Worker) {}", "Worker", False, "type_use"),
        ("a.rs", "fn f() -> Worker { todo!() }", "Worker", False, "type_use"),
        ("a.rs", "struct S { w: Worker }", "Worker", False, "type_use"),
        ("a.go", "package a\nfunc f() { helper(1) }", "helper", False, "calls"),
        ("a.go", "package a\nfunc f() { util.Go(1) }", "Go", False, "calls"),
        ("a.go", "package a\nfunc f() { x := &Shop{} }", "Shop", True, "instantiates"),
        ("a.go", "package a\nfunc f() { run(New) }", "New", False, "passes_as_value"),
        ("a.go", "package a\nfunc f() { x := New }", "New", False, "passes_as_value"),
        ("a.go", 'package a\nimport "x/util"', "util", False, "imports"),
        ("a.kt", "fun f() {\n    helper(1)\n}", "helper", False, "calls"),
        ("a.kt", "fun f() {\n    Other.make().n\n}", "make", False, "calls"),
        ("a.kt", "fun f() {\n    val s = Shop(1)\n}", "Shop", True, "instantiates"),
        ("a.kt", "fun f() {\n    run(::total)\n}", "total", False, "passes_as_value"),
        ("a.kt", "fun f() {\n    val x = total\n}", "total", False, "passes_as_value"),
        ("a.kt", "import a.b.Shop", "Shop", False, "imports"),
        ("A.java", "class A { void f() { foo(1); } }", "foo", False, "calls"),
        ("A.java", "class A { void f() { o.foo(1); } }", "foo", False, "calls"),
        ("A.java", "class A { Object f() { return new Shop(1); } }", "Shop", True, "instantiates"),
        ("A.java", "class A { Object f() { return new Shop<>(1); } }", "Shop", True, "instantiates"),
        ("A.java", "import a.b.Shop;", "Shop", False, "imports"),
        ("A.java", "class A { void f(Shop s) {} }", "Shop", False, "type_use"),
        ("A.java", "class A extends Shop {}", "Shop", False, "type_use"),
        ("A.java", "class A { void f() { run(total); } }", "total", False, "passes_as_value"),
        ("A.java", "class A { Object r = A::total; }", "total", False, "passes_as_value"),
        ("A.java", "class A { int x = total; }", "total", False, "passes_as_value"),
        ("A.java", "class A { int f() { return o.total + 1; } }", "total", False, "reads"),
        ("a.kt", "fun f(s: Shop) {\n}", "Shop", False, "type_use"),
        ("a.kt", "class A : Shop()", "Shop", False, "type_use"),
        ("a.kt", "fun f() {\n    val s: List<Shop> = x\n}", "Shop", False, "type_use"),
    ],
)
def test_compiled_language_reference_kinds(path, text, needle, is_class, expected):
    source = text.encode()
    tree = syntax.parse(path, source)
    line = next(n for n, row in enumerate(text.splitlines()) if needle in row)
    col = text.splitlines()[line].index(needle)

    kind = syntax.reference_kind(tree.root_node, (line, col), (line, col + len(needle)), is_class, syntax.grammar_for(path))

    assert kind == expected


JAVA = """package p;

import java.util.List;

/** doc */
@Deprecated
public class A<T> extends B implements C {
    private int x = 1, y;

    public A(int a) {
        this.x = a;
    }

    public <U> List<U> run(String s, int... r) throws E {
        Runnable local = new Runnable() { public void run() {} };
        return foo(s);
    }

    enum K { ONE, TWO; void f() {} }

    interface I { void g(); }
}

record R(int a) {}
"""


def test_java_definitions_are_types_members_constructors_fields_and_enum_constants():
    found = {d.key: (d.kind, d.decorated) for d in syntax.definitions("A.java", JAVA.encode())}

    assert found == {
        "A.java::A": ("class", True),
        "A.java::A.x": ("property", False),
        "A.java::A.y": ("property", False),
        "A.java::A.constructor": ("method", False),
        "A.java::A.run": ("method", False),
        "A.java::A.K": ("enum", False),
        "A.java::A.K.ONE": ("variant", False),
        "A.java::A.K.TWO": ("variant", False),
        "A.java::A.K.f": ("method", False),
        "A.java::A.I": ("interface", False),
        "A.java::A.I.g": ("method", False),
        "A.java::R": ("class", False),
    }


def test_a_java_definition_starts_at_its_doc_comment_and_a_method_body_holds_no_definition():
    defs = {d.key: d for d in syntax.definitions("A.java", JAVA.encode())}

    assert defs["A.java::A"].start == 5 and defs["A.java::A"].name == "A"
    assert not any(key.startswith("A.java::A.run.") for key in defs)
