from pathlib import Path

from scipindex import ROLE_DEFINITION, ROLE_IMPORT, Document, Index, Occurrence
from truth import Snapshot, relations_to


def at(source: str, line: int, needle: str, nth: int = 0) -> tuple[int, int, int, int]:
    row = source.splitlines()[line]
    col = -1
    for _ in range(nth + 1):
        col = row.index(needle, col + 1)
    return line, col, line, col + len(needle)


def occurrence(rng, symbol, roles=0):
    return Occurrence(*rng, symbol, roles)


def snapshot(tmp_path: Path, files: dict[str, str], occurrences: dict[str, list[Occurrence]]) -> Snapshot:
    documents = []
    for path, text in files.items():
        (tmp_path / path).parent.mkdir(parents=True, exist_ok=True)
        (tmp_path / path).write_text(text)
        documents.append(Document(path, 1, occurrences.get(path, []), []))
    return Snapshot(tmp_path, Index(documents=documents), "python")


def target(snap: Snapshot, key: str):
    return next(d for d in snap.definitions(key.split("::")[0]) if d.key == key)


MODELS = """class Request:
    def __init__(self):
        pass


class Response:
    def __init__(self):
        pass
"""
REQUEST_INIT = "scip-python python httpx 0 `httpx._models`/Request#__init__()."
REQUEST = "scip-python python httpx 0 `httpx._models`/Request#"


def test_a_construction_of_a_sibling_class_is_not_a_construction_of_this_one(tmp_path: Path):
    tests = "def test_a():\n    Response(1)\n\n\ndef test_b():\n    Request(2)\n"
    snap = snapshot(
        tmp_path,
        {"httpx/_models.py": MODELS, "tests/test_a.py": tests},
        {
            "httpx/_models.py": [
                occurrence(at(MODELS, 0, "Request"), REQUEST, ROLE_DEFINITION),
                occurrence(at(MODELS, 1, "__init__"), REQUEST_INIT, ROLE_DEFINITION),
            ],
            "tests/test_a.py": [
                occurrence(at(tests, 1, "Response"), REQUEST),  # SCIP's attribution of the sibling's constructor
                occurrence(at(tests, 5, "Request"), REQUEST),
            ],
        },
    )

    found = relations_to(snap, target(snap, "httpx/_models.py::Request.__init__"))

    assert [(r.from_key, r.kind) for r in found] == [("tests/test_a.py::test_b", "instantiates")]
    assert snap.filters.dropped == {"name": 1}


def test_a_value_built_by_an_export_is_not_a_reference_to_the_export(tmp_path: Path):
    source = "export const create = () => 1;\n"
    user = 'import { got } from "./index";\n\nexport function run() {\n  return got("x");\n}\n'
    create = "scip-typescript npm got 1 `source/create.ts`/create()."
    snap = snapshot(
        tmp_path,
        {"source/create.ts": source, "test/run.ts": user},
        {
            "source/create.ts": [occurrence(at(source, 0, "create"), create, ROLE_DEFINITION)],
            "test/run.ts": [occurrence(at(user, 3, "got"), create)],
        },
    )

    assert relations_to(snap, target(snap, "source/create.ts::create")) == []
    assert snap.filters.dropped == {"name": 1}


def test_an_import_alias_and_an_implicit_protocol_call_are_not_wrong_names(tmp_path: Path):
    lib = "def total(items):\n    return 1\n\n\nclass Box:\n    def __call__(self):\n        return 1\n"
    use = "from lib import total as t\n\n\ndef go(box):\n    return t([1]) + box()\n"
    total = "scip-python python p 0 `lib`/total()."
    call = "scip-python python p 0 `lib`/Box#__call__()."
    snap = snapshot(
        tmp_path,
        {"lib.py": lib, "use.py": use},
        {
            "lib.py": [
                occurrence(at(lib, 0, "total"), total, ROLE_DEFINITION),
                occurrence(at(lib, 5, "__call__"), call, ROLE_DEFINITION),
            ],
            "use.py": [
                occurrence(at(use, 0, "t", 1), total, ROLE_IMPORT),
                occurrence(at(use, 4, "t"), total),
                occurrence(at(use, 4, "box"), call),
            ],
        },
    )

    assert [r.from_key for r in relations_to(snap, target(snap, "lib.py::total"))] == ["use.py::<module>", "use.py::go"]
    assert [r.from_key for r in relations_to(snap, target(snap, "lib.py::Box.__call__"))] == ["use.py::go"]
    assert not snap.filters.dropped


def test_a_local_or_a_parameter_with_the_name_shadows_the_function(tmp_path: Path):
    termui = "def prompt(text):\n    return text\n\n\ndef confirm(text):\n    prompt = text\n    return prompt\n\n\ndef ask(text):\n    return prompt(text)\n"
    core = "def init(self, shell_complete):\n    self.x = shell_complete\n"
    prompt = "scip-python python click 0 `click.termui`/prompt()."
    snap = snapshot(
        tmp_path,
        {"termui.py": termui, "core.py": core},
        {
            "termui.py": [
                occurrence(at(termui, 0, "prompt"), prompt, ROLE_DEFINITION),
                occurrence(at(termui, 6, "prompt"), prompt),
                occurrence(at(termui, 10, "prompt"), prompt),
            ],
            "core.py": [occurrence(at(core, 1, "shell_complete"), prompt)],
        },
    )

    found = relations_to(snap, target(snap, "termui.py::prompt"))

    assert [r.from_key for r in found] == ["termui.py::ask"]
    assert snap.filters.dropped == {"shadow": 1, "name": 1}


def test_a_member_with_the_name_of_a_local_is_not_shadowed(tmp_path: Path):
    source = "def total(x):\n    return x\n\n\ndef use(total):\n    return self.total(1)\n"
    total = "scip-python python p 0 `m`/Shop#total()."
    snap = snapshot(
        tmp_path,
        {"m.py": source},
        {"m.py": [occurrence(at(source, 0, "total"), total, ROLE_DEFINITION), occurrence(at(source, 5, "total"), total)]},
    )

    assert [r.from_key for r in relations_to(snap, target(snap, "m.py::total"))] == ["m.py::use"]


def test_a_wrapper_and_the_function_it_calls_are_told_apart_by_the_files_a_caller_reaches(tmp_path: Path):
    impl = "def get_pager_file():\n    return 1\n"
    wrapper = "from ._impl import get_pager_file as _g\n\n\ndef get_pager_file():\n    return _g()\n"
    test_a = "import click\n\n\ndef test_a():\n    click.get_pager_file()\n"
    test_b = "import click\nfrom click._impl import helper\n\n\ndef test_b():\n    click.get_pager_file()\n    helper()\n"
    impl_symbol = "scip-python python click 0 `click._impl`/get_pager_file()."
    wrapper_symbol = "scip-python python click 0 `click.termui`/get_pager_file()."
    helper_symbol = "scip-python python click 0 `click._impl`/helper()."
    snap = snapshot(
        tmp_path,
        {
            "click/_impl.py": impl + "\n\ndef helper():\n    return 2\n",
            "click/termui.py": wrapper,
            "tests/test_a.py": test_a,
            "tests/test_b.py": test_b,
        },
        {
            "click/_impl.py": [
                occurrence(at(impl, 0, "get_pager_file"), impl_symbol, ROLE_DEFINITION),
                occurrence((3, 4, 3, 10), helper_symbol, ROLE_DEFINITION),
            ],
            "click/termui.py": [occurrence(at(wrapper, 3, "get_pager_file"), wrapper_symbol, ROLE_DEFINITION)],
            "tests/test_a.py": [occurrence(at(test_a, 4, "get_pager_file"), impl_symbol)],
            "tests/test_b.py": [
                occurrence(at(test_b, 5, "get_pager_file"), impl_symbol),
                occurrence(at(test_b, 6, "helper"), helper_symbol),
            ],
        },
    )

    found = relations_to(snap, target(snap, "click/_impl.py::get_pager_file"))

    assert [r.from_key for r in found] == ["tests/test_b.py::test_b"]
    assert snap.filters.dropped == {"homonym": 1}


RESPONSE = """var res = {};
res.jsonp = function jsonp(obj) {
  return res.send(obj);
};
res.other = 1;
"""
RES = "scip-typescript npm express 0 `response.js`/res."


def test_the_target_of_an_assignment_is_no_use_by_the_function_assigned(tmp_path: Path):
    (tmp_path / "response.js").write_text(RESPONSE)
    document = Document(
        "response.js",
        1,
        [
            occurrence(at(RESPONSE, 0, "res"), RES, ROLE_DEFINITION),
            occurrence(at(RESPONSE, 1, "res"), RES),  # the assignment target: a use by the module
            occurrence(at(RESPONSE, 2, "res"), RES),  # inside the function body: a use by the function
        ],
        [],
    )
    snap = Snapshot(tmp_path, Index(documents=[document]), "javascript")

    found = relations_to(snap, target(snap, "response.js::res"))

    assert [(r.from_key, r.site) for r in found] == [("response.js::jsonp", 3)]
    assert snap.filters.dropped == {"fnexpr": 1}
