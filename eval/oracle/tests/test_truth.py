from pathlib import Path

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
