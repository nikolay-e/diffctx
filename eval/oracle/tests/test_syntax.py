import pytest

import syntax
from model import innermost
from syntax import definitions, reference_kind

PYTHON = b"""RATE = 2


class Shop:
    limit = 3

    def total(self, items):
        return sum(items)

    @staticmethod
    def helper(x):
        return x


def outer():
    def inner():
        return 1

    return inner
"""

TYPESCRIPT = b"""export const RATE = 2;
export function total(items: number[]): number {
  return items.length;
}
export class Shop {
  limit = 3;
  total(items: number[]): number {
    return items.length;
  }
}
const run = () => 1;
interface Item { id: number }
type Id = string;
enum Color { Red }
"""

JAVASCRIPT = b"""app.listen = function listen() {
  return 1;
};
function plain() {}
"""


def by_key(path: str, source: bytes) -> dict:
    return {d.key: d for d in definitions(path, source)}


def test_python_definitions_cover_whole_extents():
    found = by_key("shop.py", PYTHON)

    assert set(found) == {
        "shop.py::RATE",
        "shop.py::Shop",
        "shop.py::Shop.limit",
        "shop.py::Shop.total",
        "shop.py::Shop.helper",
        "shop.py::outer",
        "shop.py::outer.inner",
    }
    assert found["shop.py::Shop.total"].kind == "method" and found["shop.py::outer"].kind == "function"
    helper = found["shop.py::Shop.helper"]
    assert (helper.start, helper.end, helper.decorated) == (10, 12, True)
    assert (found["shop.py::Shop"].start, found["shop.py::Shop"].end) == (4, 12)
    assert found["shop.py::RATE"].kind == "variable"


def test_typescript_definitions_include_exports_members_and_types():
    found = by_key("shop.ts", TYPESCRIPT)

    assert {
        "shop.ts::RATE",
        "shop.ts::total",
        "shop.ts::Shop",
        "shop.ts::Shop.limit",
        "shop.ts::Shop.total",
        "shop.ts::run",
        "shop.ts::Item",
        "shop.ts::Id",
        "shop.ts::Color",
    } <= set(found)
    assert (found["shop.ts::total"].start, found["shop.ts::total"].end) == (2, 4)
    assert found["shop.ts::run"].kind == "function" and found["shop.ts::Shop.total"].kind == "method"


def test_javascript_member_assignment_is_a_definition():
    found = by_key("app.js", JAVASCRIPT)

    assert {"app.js::listen", "app.js::plain"} <= set(found)
    assert (found["app.js::listen"].start, found["app.js::listen"].end) == (1, 3)


def test_hashes_ignore_formatting_and_comments_but_not_code_or_names():
    base = by_key("m.py", b"def f(a):\n    return a + 1\n")["m.py::f"]
    spaced = by_key("m.py", b"def f( a ):\n    # note\n    return   a+1\n")["m.py::f"]
    edited = by_key("m.py", b"def f(a):\n    return a + 2\n")["m.py::f"]
    renamed = by_key("m.py", b"def g(a):\n    return a + 1\n")["m.py::g"]

    assert spaced.body_hash == base.body_hash and spaced.signature == base.signature
    assert edited.body_hash != base.body_hash and edited.shape_hash != base.shape_hash
    assert renamed.body_hash != base.body_hash and renamed.shape_hash == base.shape_hash


def test_innermost_picks_the_method_over_the_class():
    defs = definitions("shop.py", PYTHON)

    assert innermost(defs, 8).key == "shop.py::Shop.total"
    assert innermost(defs, 4).key == "shop.py::Shop"
    assert innermost(defs, 3) is None


def kind_at(path: str, source: str, line: int, needle: str, is_class: bool = False, nth: int = 0) -> str:
    tree = syntax.parse(path, source.encode())
    row = source.splitlines()[line]
    col = -1
    for _ in range(nth + 1):
        col = row.index(needle, col + 1)
    grammar = "python" if path.endswith(".py") else "script"
    return reference_kind(tree.root_node, (line, col), (line, col + len(needle)), is_class, grammar)


@pytest.mark.parametrize(
    ("line", "text", "needle", "is_class", "expected"),
    [
        (0, "foo(1)", "foo", False, "calls"),
        (0, "self.foo(1)", "foo", False, "calls"),
        (0, "foo(1)", "foo", True, "instantiates"),
        (0, "bar(foo)", "foo", False, "passes_as_value"),
        (0, "y = foo", "foo", False, "passes_as_value"),
        (0, "bar(key=foo)", "foo", False, "passes_as_value"),
        (0, "class A(foo): pass", "foo", False, "reads"),
        (0, "z = foo.attr", "foo", False, "reads"),
        (0, "@foo\ndef f(): pass", "foo", False, "calls"),
    ],
)
def test_python_reference_kinds(line, text, needle, is_class, expected):
    assert kind_at("m.py", text, line, needle, is_class) == expected


@pytest.mark.parametrize(
    ("text", "needle", "is_class", "expected"),
    [
        ("foo(1);", "foo", False, "calls"),
        ("obj.foo(1);", "foo", False, "calls"),
        ("new foo();", "foo", True, "instantiates"),
        ("bar(foo);", "foo", False, "passes_as_value"),
        ("const y = foo;", "foo", False, "passes_as_value"),
        ("y = foo;", "foo", False, "passes_as_value"),
        ("const o = { k: foo };", "foo", False, "passes_as_value"),
        ('export { foo } from "./x";', "foo", False, "reexports"),
        ("const y = foo.x;", "foo", False, "reads"),
    ],
)
def test_script_reference_kinds(text, needle, is_class, expected):
    assert kind_at("m.ts", text, 0, needle, is_class) == expected


def click_example(commands: int = 30) -> bytes:
    """A long module of decorated commands with nested functions, the shape that crashed py-tree-sitter 0.26.0 on `Node.children`."""
    lines = ['"""Image pipeline example."""', "", "from functools import update_wrapper", "", "import click", ""]
    for n in range(commands):
        lines += [
            "",
            "@click.group(chain=True)",
            f'@click.option("-i", "--input", type=click.Path(), multiple=True, help="input {n}")',
            "@click.pass_context",
            f"def command_{n}(ctx, images, input=None):",
            f'    """Handle step {n}."""',
            "",
            "    def processor(f):",
            "        def new_func(*args, **kwargs):",
            "            def generator(stream):",
            "                for item in stream:",
            f"                    yield f(item, *args, factor={n}, **kwargs)",
            "",
            "            return generator",
            "",
            "        return update_wrapper(new_func, f)",
            "",
            '    click.echo("step", nl=False)',
            "    return processor",
            "",
        ]
    return "\n".join(lines).encode() + b"\n"


def test_a_long_decorated_module_with_nested_functions_is_read_through_cursors():
    source = click_example()
    found = by_key("imagepipe.py", source)

    assert len(source.splitlines()) > 600
    assert {f"imagepipe.py::command_{n}.processor.new_func.generator" for n in range(30)} <= set(found)
    assert all(d.decorated for d in found.values() if d.container == () and d.name.startswith("command_"))
    tree = syntax.parse("imagepipe.py", source)
    assert b"update_wrapper" in syntax.tokens(tree.root_node, source)
    assert len(syntax.tokens(tree.root_node, source)) > 1000


def test_tokens_of_a_leaf_or_a_comment_stay_inside_the_node():
    source = b"x = 1  # note\ny = 2\n"
    tree = syntax.parse("m.py", source)
    first, second = (n for n in syntax.kids(tree.root_node, True) if n.type == "expression_statement")

    assert syntax.tokens(first, source) == [b"x", b"=", b"1"]
    assert syntax.tokens(second, source) == [b"y", b"=", b"2"]
    leaf = syntax.kids(syntax.kids(first, True)[0], True)[0]
    assert leaf.type == "identifier" and syntax.tokens(leaf, source) == [b"x"]
    [comment] = [n for n in syntax.kids(tree.root_node, True) if n.type == "comment"]
    assert syntax.tokens(comment, source) == []
