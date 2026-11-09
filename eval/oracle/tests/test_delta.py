from pathlib import Path

import pytest

from changed import ChangedDefinition
from delta import classify
from scipindex import Index
from truth import Snapshot

LANGUAGE = {"py": "python", "ts": "typescript", "rs": "rust", "kt": "kotlin"}


def delta_of(tmp_path: Path, path: str, old: str, new: str, key: str, status: str = "modified"):
    snapshots = []
    for side, text in (("base", old), ("head", new)):
        root = tmp_path / side
        (root / path).parent.mkdir(parents=True, exist_ok=True)
        (root / path).write_text(text)
        snapshots.append(Snapshot(root, Index(), LANGUAGE[path.rsplit(".", 1)[1]]))
    base, head = snapshots
    find = lambda snap: next((d for d in snap.definitions(path) if d.key == f"{path}::{key}"), None)  # noqa: E731
    item = ChangedDefinition(status, find(head), find(base))
    return classify(item, base, head)


PY = "def f(a: int, b: int = 1) -> int:\n    return a + b\n"


@pytest.mark.parametrize(
    ("path", "old", "new", "change"),
    [
        ("m.py", PY, PY.replace("a + b", "a - b"), "body"),
        ("m.py", PY, PY.replace("    return", '    """Adds."""\n    return'), "doc_layout"),
        ("m.py", PY, PY.replace("    return", "    # a note\n    return"), "doc_layout"),
        ("m.py", PY, PY.replace("b: int = 1", "b: int = 1, c: int = 2"), "signature_compatible"),
        ("m.py", PY, PY.replace("b: int = 1", "b: int = 1, *rest"), "signature_compatible"),
        ("m.py", PY, PY.replace("b: int = 1", "b: int = 1, *, key=None"), "signature_compatible"),
        ("m.py", PY, PY.replace("b: int = 1", "b: int = 1, c: int"), "signature"),
        ("m.py", PY, PY.replace("a: int, b", "b, a: int"), "signature"),
        ("m.py", PY, PY.replace("-> int", "-> float"), "signature"),
        ("m.py", PY, "@cache\n" + PY, "decorators"),
        (
            "m.ts",
            "export function f(a: number): number { return a }",
            "export function f(a: number): number { return a + 1 }",
            "body",
        ),
        (
            "m.ts",
            "export function f(a: number): number { return a }",
            "export function f(a: number, b?: string): number { return a }",
            "signature_compatible",
        ),
        (
            "m.ts",
            "export function f(a: number): number { return a }",
            "export function f(a: number, b = 1, ...r: number[]): number { return a }",
            "signature_compatible",
        ),
        (
            "m.ts",
            "export function f(a: number): number { return a }",
            "export function f(a: number, b: string): number { return a }",
            "signature",
        ),
        ("m.ts", "export function f(a: number): number { return a }", "function f(a: number): number { return a }", "visibility"),
        ("m.rs", "pub fn f(a: i32) -> i32 { a }", "pub fn f(a: i32) -> i32 { a + 1 }", "body"),
        ("m.rs", "pub fn f(a: i32) -> i32 { a }", "pub fn f(a: i32, b: i32) -> i32 { a }", "signature"),
        ("m.rs", "pub fn f(a: i32) -> i32 { a }", "fn f(a: i32) -> i32 { a }", "visibility"),
        ("m.kt", "fun f(a: Int): Int { return a }", "fun f(a: Int): Int { return a + 1 }", "body"),
        ("m.kt", "fun f(a: Int): Int { return a }", 'fun f(a: Int, b: String = "x"): Int { return a }', "signature_compatible"),
        ("m.kt", "fun f(a: Int): Int { return a }", "private fun f(a: Int): Int { return a }", "visibility"),
    ],
)
def test_a_callable_is_classified_by_its_own_base_and_head_syntax(tmp_path: Path, path, old, new, change):
    key = "f"

    assert delta_of(tmp_path, path, old, new, key).change == change


def test_the_parameters_a_compatible_change_added_are_named(tmp_path: Path):
    found = delta_of(tmp_path, "m.py", PY, PY.replace("b: int = 1", "b: int = 1, c: int = 2, *rest"), "f")

    assert found.parameters == ("c : int = 2", "* rest")


def test_removal_rename_and_move_are_what_the_matching_says(tmp_path: Path):
    for status in ("removed", "renamed", "moved", "added"):
        assert delta_of(tmp_path, "m.py", PY, PY, "f", status).change == status


def test_two_definitions_of_one_name_make_the_pairing_unknown(tmp_path: Path):
    twice = PY + "\n\n" + PY.replace("a + b", "a * b")

    assert delta_of(tmp_path, "m.py", twice, twice.replace("a * b", "a % b"), "f").change == "unknown"


@pytest.mark.parametrize(
    ("path", "text", "typed"),
    [
        ("m.py", PY, True),
        ("m.py", "def f(a, b: int = 1) -> int:\n    return a\n", False),
        ("m.py", "def f(a: int, b: int = 1):\n    return a\n", False),
        ("m.py", "class C:\n    def f(self, a: int) -> int:\n        return a\n", True),
        ("m.ts", "export function f(a: number): number { return a }", True),
        ("m.ts", "export function f(a: number) { return a }", False),
        ("m.ts", "export function f(a): number { return a }", False),
        ("m.rs", "pub fn f(a: i32) -> i32 { a }", True),
        ("m.kt", "fun f(a: Int): Int { return a }", True),
        ("m.kt", "fun f(a: Int) { println(a) }", False),
        ("m.kt", "fun f(a: Int): Int = a", False),
    ],
)
def test_a_callable_is_typed_when_its_parameters_and_return_type_are_written_down(tmp_path: Path, path, text, typed):
    key = "C.f" if text.startswith("class C") else "f"
    changed = (
        text.replace("return a", "return a + 1")
        .replace("= a", "= a + 1")
        .replace("{ a }", "{ a + 1 }")
        .replace("println(a)", "println(a + 1)")
    )

    assert delta_of(tmp_path, path, text, changed, key).typed is typed


def test_a_class_a_type_and_a_variable_have_their_own_changes(tmp_path: Path):
    cls = "class C(Base):\n    limit = 1\n"
    assert delta_of(tmp_path, "m.py", cls, cls.replace("1", "2"), "C").change == "class_body"
    assert delta_of(tmp_path, "m.py", cls, cls.replace("Base", "Other"), "C").change == "other"
    var = "LIMIT: int = 1\n"
    value = delta_of(tmp_path, "m.py", var, "LIMIT: int = 2\n", "LIMIT")
    assert (value.change, value.typed) == ("value", True)
    inferred = delta_of(tmp_path, "m.py", "LIMIT = 1\n", "LIMIT = 2\n", "LIMIT")
    assert (inferred.change, inferred.typed) == ("value", False)
    retyped = delta_of(tmp_path, "m.py", var, "LIMIT: float = 1\n", "LIMIT")
    assert retyped.change == "other"
    iface = "export interface I { a: number }\n"
    assert delta_of(tmp_path, "m.ts", iface, iface.replace("number", "string"), "I").change == "declaration"
    enum = "pub enum E { A }\n"
    assert delta_of(tmp_path, "m.rs", enum, "pub enum E { A, B }\n", "E").change == "declaration"


def test_a_constructor_is_marked_and_a_decorated_one_stays_decorated(tmp_path: Path):
    cls = "class C:\n    @property\n    def v(self) -> int:\n        return 1\n\n    def __init__(self) -> None:\n        pass\n"

    assert delta_of(tmp_path, "m.py", cls, cls.replace("pass", "self.x = 1"), "C.__init__").constructor
    found = delta_of(tmp_path, "m.py", cls, cls.replace("return 1", "return 2"), "C.v")
    assert found.decorated and found.change == "body"
