from pathlib import Path

import syntax
from scipindex import ROLE_DEFINITION, ROLE_IMPORT, ROLE_WRITE, Document, Index, Occurrence, SymbolInfo
from truth import Snapshot, relations_to

TOTAL = "scip-python python shop 0 `shop.pricing`/total()."
BASE_RUN = "scip-python python shop 0 `shop.pricing`/Base#run()."
SUB_RUN = "scip-python python shop 0 `shop.pricing`/Sub#run()."
SHOP = "scip-python python shop 0 `shop.pricing`/Shop#"
SHOP_INIT = "scip-python python shop 0 `shop.pricing`/Shop#__init__()."

PRICING = """def total(items):
    return sum(items)


class Base:
    def run(self):
        return 1


class Sub(Base):
    def run(self):
        return 2


class Shop:
    def __init__(self):
        self.n = 0
"""

CHECKOUT = """from shop.pricing import total, Shop


def charge(items):
    return total(items)


def register(callbacks):
    callbacks.append(total)


RATE = total


def build():
    return Shop()


def recurse(items):
    return total(items)
"""


def at(source: str, line: int, needle: str, nth: int = 0) -> tuple[int, int, int, int]:
    row = source.splitlines()[line]
    col = -1
    for _ in range(nth + 1):
        col = row.index(needle, col + 1)
    return line, col, line, col + len(needle)


def occurrence(rng, symbol, roles=0):
    return Occurrence(*rng, symbol, roles)


def build(tmp_path: Path) -> Snapshot:
    (tmp_path / "shop").mkdir()
    (tmp_path / "shop" / "pricing.py").write_text(PRICING)
    (tmp_path / "shop" / "checkout.py").write_text(CHECKOUT)
    pricing = Document(
        "shop/pricing.py",
        1,
        [
            occurrence(at(PRICING, 0, "total"), TOTAL, ROLE_DEFINITION),
            occurrence(at(PRICING, 5, "run"), BASE_RUN, ROLE_DEFINITION),
            occurrence(at(PRICING, 10, "run"), SUB_RUN, ROLE_DEFINITION),
            occurrence(at(PRICING, 14, "Shop"), SHOP, ROLE_DEFINITION),
            occurrence(at(PRICING, 15, "__init__"), SHOP_INIT, ROLE_DEFINITION),
        ],
        [SymbolInfo(SUB_RUN, 26, [BASE_RUN])],
    )
    checkout = Document(
        "shop/checkout.py",
        1,
        [
            occurrence(at(CHECKOUT, 0, "total"), TOTAL, ROLE_IMPORT),
            occurrence(at(CHECKOUT, 0, "Shop"), SHOP, ROLE_IMPORT),
            occurrence(at(CHECKOUT, 4, "total"), TOTAL),
            occurrence(at(CHECKOUT, 8, "total"), TOTAL),
            occurrence(at(CHECKOUT, 11, "total"), TOTAL, ROLE_WRITE),
            occurrence(at(CHECKOUT, 15, "Shop"), SHOP),
            occurrence(at(CHECKOUT, 19, "total"), TOTAL),
        ],
        [],
    )
    return Snapshot(tmp_path, Index(documents=[pricing, checkout]), "python")


def target(snapshot: Snapshot, key: str):
    return next(d for d in snapshot.definitions(key.split("::")[0]) if d.key == key)


def test_relations_carry_the_syntactic_kind_of_each_reference(tmp_path: Path):
    snapshot = build(tmp_path)

    found = relations_to(snapshot, target(snapshot, "shop/pricing.py::total"))

    assert {(r.from_key, r.kind, r.site) for r in found} == {
        ("shop/checkout.py::<module>", "imports", 1),
        ("shop/checkout.py::charge", "calls", 5),
        ("shop/checkout.py::register", "passes_as_value", 9),
        ("shop/checkout.py::RATE", "writes", 12),
        ("shop/checkout.py::recurse", "calls", 20),
    }
    assert all(r.to_key == "shop/pricing.py::total" and r.resolution == "resolved" for r in found)


def test_a_constructor_also_takes_the_instantiations_of_its_class(tmp_path: Path):
    snapshot = build(tmp_path)

    found = relations_to(snapshot, target(snapshot, "shop/pricing.py::Shop.__init__"))

    assert {(r.from_key, r.kind) for r in found} == {("shop/checkout.py::build", "instantiates")}


def test_an_overriding_method_reaches_the_overridden_one(tmp_path: Path):
    snapshot = build(tmp_path)

    found = relations_to(snapshot, target(snapshot, "shop/pricing.py::Base.run"))

    assert [(r.from_key, r.kind) for r in found] == [("shop/pricing.py::Sub.run", "overrides")]


def test_a_definition_without_an_index_symbol_has_no_symbols(tmp_path: Path):
    snapshot = build(tmp_path)

    assert snapshot.symbols_of(target(snapshot, "shop/pricing.py::Sub")) == set()
    assert relations_to(snapshot, target(snapshot, "shop/pricing.py::Sub")) == []


NESTED = """def outer():
    def inner():
        return 1

    return inner()
"""


def test_a_local_symbol_reaches_only_references_in_its_own_document(tmp_path: Path):
    (tmp_path / "a.py").write_text(NESTED)
    (tmp_path / "b.py").write_text("x = inner\n")
    first = Document(
        "a.py",
        1,
        [
            occurrence(at(NESTED, 1, "inner"), "local 3", ROLE_DEFINITION),
            occurrence(at(NESTED, 4, "inner"), "local 3"),
        ],
        [],
    )
    second = Document("b.py", 1, [occurrence(at("x = inner", 0, "inner"), "local 3")], [])
    snapshot = Snapshot(tmp_path, Index(documents=[first, second]), "python")

    inner = next(d for d in syntax.definitions("a.py", NESTED.encode()) if d.key == "a.py::outer.inner")

    found = relations_to(snapshot, inner)

    assert [(r.from_key, r.kind, r.site) for r in found] == [("a.py::outer", "calls", 5)]


def test_an_occurrence_inside_an_import_statement_is_imported_even_when_the_indexer_sets_no_import_role(tmp_path: Path):
    # scip-python gives the imported name no import role; read from the syntax it was filed as a read (R8).
    (tmp_path / "shop").mkdir()
    (tmp_path / "shop" / "pricing.py").write_text(PRICING)
    (tmp_path / "shop" / "checkout.py").write_text(CHECKOUT)
    pricing = Document("shop/pricing.py", 1, [occurrence(at(PRICING, 0, "total"), TOTAL, ROLE_DEFINITION)], [])
    checkout = Document(
        "shop/checkout.py", 1, [occurrence(at(CHECKOUT, 0, "total"), TOTAL), occurrence(at(CHECKOUT, 4, "total"), TOTAL)], []
    )
    snapshot = Snapshot(tmp_path, Index(documents=[pricing, checkout]), "python")

    found = relations_to(snapshot, target(snapshot, "shop/pricing.py::total"))

    assert {(r.from_key, r.kind, r.moved) for r in found} == {
        ("shop/checkout.py::<module>", "imports", ("R8",)),
        ("shop/checkout.py::charge", "calls", ()),
    }


def test_an_annotation_is_a_type_use_not_a_read(tmp_path: Path):
    (tmp_path / "shop").mkdir()
    (tmp_path / "shop" / "pricing.py").write_text(PRICING)
    annotated = "def build(shop: Shop) -> Shop:\n    return shop\n"
    (tmp_path / "shop" / "checkout.py").write_text(annotated)
    pricing = Document("shop/pricing.py", 1, [occurrence(at(PRICING, 14, "Shop"), SHOP, ROLE_DEFINITION)], [])
    checkout = Document(
        "shop/checkout.py",
        1,
        [occurrence(at(annotated, 0, "Shop", 0), SHOP), occurrence(at(annotated, 0, "Shop", 1), SHOP)],
        [],
    )
    snapshot = Snapshot(tmp_path, Index(documents=[pricing, checkout]), "python")

    found = relations_to(snapshot, target(snapshot, "shop/pricing.py::Shop"))

    assert {(r.from_key, r.kind, r.moved) for r in found} == {("shop/checkout.py::build", "type_use", ("R9",))}


def test_a_call_whose_callee_the_index_calls_a_class_is_an_instantiation(tmp_path: Path):
    # `Widget` is declared as a function the AST cannot tell from any other; the index says Class (kind 7).
    widget = "scip-typescript npm shop 0 `lib.ts`/Widget#"
    lib, use = "export function Widget() {}\n", "import { Widget } from './lib';\nexport function make() { return Widget(); }\n"
    (tmp_path / "lib.ts").write_text(lib)
    (tmp_path / "use.ts").write_text(use)
    library = Document("lib.ts", 1, [occurrence(at(lib, 0, "Widget"), widget, ROLE_DEFINITION)], [SymbolInfo(widget, 7)])
    caller = Document("use.ts", 1, [occurrence(at(use, 1, "Widget"), widget)], [])
    snapshot = Snapshot(tmp_path, Index(documents=[library, caller]), "typescript")

    found = relations_to(snapshot, target(snapshot, "lib.ts::Widget"))

    assert {(r.from_key, r.kind, r.moved) for r in found} == {("use.ts::make", "instantiates", ("R10",))}


def test_a_definition_whose_only_symbol_is_local_belongs_to_its_enclosing_definition(tmp_path: Path):
    (tmp_path / "a.py").write_text(NESTED)
    document = Document(
        "a.py",
        1,
        [
            occurrence(at(NESTED, 0, "outer"), "scip-python python shop 0 `a`/outer()."),
            occurrence(at(NESTED, 1, "inner"), "local 3", ROLE_DEFINITION),
        ],
        [],
    )
    document.occurrences[0].roles = ROLE_DEFINITION
    snapshot = Snapshot(tmp_path, Index(documents=[document]), "python")

    assert [d.key for d in snapshot.definitions("a.py")] == ["a.py::outer"]
    assert snapshot.enclosing("a.py", 3).key == "a.py::outer"


NESTED_PYTHON = """class Box:
    def method(self):
        return 1


def test_helper():
    def helper():
        return 1

    class Local:
        def run(self):
            return helper()

    return helper() + Local().run()


def make():
    def inner():
        return 1

    return inner


def register(app):
    def callback():
        return 1

    app.callback = callback

    def unused():
        return 2

    def handler():
        return unused()

    return handler
"""


def test_a_python_definition_inside_a_function_that_keeps_it_there_belongs_to_that_function(tmp_path: Path):
    # scip-python gives `helper` a full descriptor, not `local N`: the syntax decides (R18)
    (tmp_path / "t.py").write_text(NESTED_PYTHON)
    snapshot = Snapshot(tmp_path, Index(), "python")

    keys = {d.key for d in snapshot.definitions("t.py")}

    assert "t.py::test_helper.helper" not in keys and "t.py::test_helper.Local" not in keys
    assert "t.py::test_helper.Local.run" not in keys  # a method of a local class is as local as the class
    assert {"t.py::Box.method", "t.py::test_helper", "t.py::make"} <= keys
    assert "t.py::make.inner" in keys  # returned: it leaves the function
    assert "t.py::register.callback" in keys  # assigned to an attribute: it leaves the function
    assert "t.py::register.unused" not in keys  # only called by another local
    assert "t.py::register.handler" in keys  # returned


ALIAS_LIB = "export class C {\n  static m(a: number): number {\n    return a;\n  }\n}\n"
ALIAS_USE = "import { C } from './lib';\nconst {m} = C;\nconst {m: other} = C;\nexport function build() {\n  const x = m(1);\n  return other(x);\n}\n"
C_CLASS = "scip-typescript npm shop 0 `lib.ts`/C#"
C_M = "scip-typescript npm shop 0 `lib.ts`/C#m()."


def alias_snapshot(tmp_path: Path) -> Snapshot:
    (tmp_path / "lib.ts").write_text(ALIAS_LIB)
    (tmp_path / "use.ts").write_text(ALIAS_USE)
    lib = Document(
        "lib.ts",
        1,
        [occurrence(at(ALIAS_LIB, 0, "C"), C_CLASS, ROLE_DEFINITION), occurrence(at(ALIAS_LIB, 1, "m"), C_M, ROLE_DEFINITION)],
        [],
    )
    use = Document(
        "use.ts",
        1,
        [
            occurrence(at(ALIAS_USE, 0, "C"), C_CLASS, ROLE_IMPORT),
            occurrence(at(ALIAS_USE, 1, "m"), "local 1", ROLE_DEFINITION),  # `const {m}` binds a constant
            occurrence(at(ALIAS_USE, 1, "m"), C_M),  # ... and names the member it takes
            occurrence(at(ALIAS_USE, 2, "m"), C_M),  # `{m: other}`: m is the member
            occurrence(at(ALIAS_USE, 2, "other"), "local 2", ROLE_DEFINITION),
            occurrence(at(ALIAS_USE, 4, "m"), "local 1"),
            occurrence(at(ALIAS_USE, 5, "other"), "local 2"),
        ],
        [],
    )
    return Snapshot(tmp_path, Index(documents=[lib, use]), "typescript")


def test_a_call_through_a_name_destructured_from_the_members_owner_is_a_call_of_the_member(tmp_path: Path):
    # SCIP binds `m(1)` and `other(x)` to the local constants, so the truth held only the destructuring statements.
    snapshot = alias_snapshot(tmp_path)

    found = relations_to(snapshot, target(snapshot, "lib.ts::C.m"))

    assert {(r.from_key, r.kind, r.site, r.moved) for r in found} == {
        ("use.ts::<module>", "reads", 2, ()),  # the destructuring statements stay reads
        ("use.ts::<module>", "reads", 3, ()),
        ("use.ts::build", "calls", 5, ("alias",)),
        ("use.ts::build", "calls", 6, ("alias",)),
    }


FACTORY = """const create = (d: number) => {
  const got = (x: number) => x + d;
  const helper = (y: number) => y * 2;
  const paginate = (z: number) => z;
  got.paginate = paginate as any;
  const unused = () => helper(1);
  return got;
};
"""


def test_a_local_typescript_function_the_factory_returns_or_stores_is_a_definition_of_its_own(tmp_path: Path):
    # The index calls every constant inside a function `local N`; the ones that leave it (`got` is returned, `paginate`
    # stored as a member of what is returned) are the definitions an edit inside them belongs to, not `create`.
    (tmp_path / "f.ts").write_text(FACTORY)
    names = {"create": None, "got": 1, "helper": 2, "paginate": 3, "unused": 4}
    row = {"create": 0, "got": 1, "helper": 2, "paginate": 3, "unused": 5}
    occurrences = [
        occurrence(
            at(FACTORY, row[name], name),
            "scip-typescript npm s 0 `f.ts`/create()." if number is None else f"local {number}",
            ROLE_DEFINITION,
        )
        for name, number in names.items()
    ]
    snapshot = Snapshot(tmp_path, Index(documents=[Document("f.ts", 1, occurrences, [])]), "typescript")

    keys = {d.key for d in snapshot.definitions("f.ts")}

    assert keys == {"f.ts::create", "f.ts::create.got", "f.ts::create.paginate"}
    assert snapshot.enclosing("f.ts", 2).key == "f.ts::create.got"  # an edit on `got`'s line is an edit of `got`
    assert snapshot.enclosing("f.ts", 6).key == "f.ts::create"  # `unused`, local and kept, is create's own


CAST_FACTORY = """const create = () => {
  const got = ((x: number) => x) as Got;
  got.paginate = (async function* (y: number) {
    yield y;
  }) as Paginate;
  const kept = ((z: number) => z) as Fn;
  return got;
};
"""


def test_a_function_under_parentheses_and_a_type_assertion_is_still_a_function(tmp_path: Path):
    (tmp_path / "f.ts").write_text(CAST_FACTORY)
    names = {"create": (0, None), "got": (1, 1), "kept": (5, 2)}
    occurrences = [
        occurrence(
            at(CAST_FACTORY, row, name),
            "scip-typescript npm s 0 `f.ts`/create()." if number is None else f"local {number}",
            ROLE_DEFINITION,
        )
        for name, (row, number) in names.items()
    ]
    snapshot = Snapshot(tmp_path, Index(documents=[Document("f.ts", 1, occurrences, [])]), "typescript")

    defs = {d.key: d.kind for d in snapshot.definitions("f.ts")}

    assert defs == {"f.ts::create": "function", "f.ts::create.got": "function", "f.ts::create.paginate": "function"}
    assert snapshot.enclosing("f.ts", 4).key == "f.ts::create.paginate"  # the assigned generator holds its own lines


def test_an_edge_from_a_document_outside_the_repository_is_not_truth(tmp_path: Path):
    generated_main = "../../../go/build/dc/0123abcd-d"
    snapshot = build(tmp_path)
    snapshot.index.documents.append(Document(generated_main, 1, [occurrence((3, 4, 3, 9), TOTAL)], []))
    snapshot = Snapshot(tmp_path, snapshot.index, "python")

    found = relations_to(snapshot, target(snapshot, "shop/pricing.py::total"))

    assert found and all(not r.from_key.startswith("..") for r in found)
    assert snapshot.documents.keys() == {"shop/pricing.py", "shop/checkout.py"}
