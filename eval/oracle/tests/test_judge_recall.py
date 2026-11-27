"""The judge must still count a real loss of exactly the shape each forgiving rule forgives: the omission is counted, the
control the rule exists for stays forgiven. Each case is cut from a loss a real answer of the full run could hide."""

from pathlib import Path

import pytest
from test_enclosure import a_range, entry, silent_candidates
from test_filters import at, occurrence, snapshot, target

from scipindex import ROLE_DEFINITION
from stale import pairing
from truth import relations_to

RUST_ATTRIBUTE_CHANGES = {
    "derive added": ("struct S {\n    x: u8,\n}\n", "#[derive(Clone)]\nstruct S {\n    x: u8,\n}\n", "a.rs::S"),
    "derive changed": ("#[derive(Clone)]\nstruct S {\n}\n", "#[derive(Debug)]\nstruct S {\n}\n", "a.rs::S"),
    "inline added": ("fn f() {}\n", "#[inline]\nfn f() {}\n", "a.rs::f"),
    "cfg swapped": ("#[cfg(unix)]\nfn f() {}\n", "#[cfg(windows)]\nfn f() {}\n", "a.rs::f"),
    "cfg on a method": (
        "struct S;\nimpl S {\n    fn f(&self) {}\n}\n",
        "struct S;\nimpl S {\n    #[cfg(test)]\n    fn f(&self) {}\n}\n",
        "a.rs::S.f",
    ),
}


@pytest.mark.parametrize("case", RUST_ATTRIBUTE_CHANGES)
def test_a_rust_attribute_change_is_a_change_of_its_definition_and_is_lost_when_the_answer_never_names_it(tmp_path: Path, case):
    """S13: a Python decorator or a Kotlin annotation change is `modified`; the attribute sits outside a Rust item's node."""
    base, head, key = RUST_ATTRIBUTE_CHANGES[case]
    prepared = a_range(tmp_path, {"a.rs": base}, {"a.rs": head}, "rust")

    assert [(c.status, c.target.key) for c in prepared.changed] == [("modified", key)]
    assert silent_candidates(prepared, []) == ([key], [key])


def test_a_comment_or_a_doc_edit_beside_a_rust_attribute_is_still_layout(tmp_path: Path):
    base = "/// Old words.\n#[inline]\nfn f() {}\n"
    head = "/// New words.\n#[inline]\n// a note\nfn f() {}\n"
    prepared = a_range(tmp_path, {"a.rs": base}, {"a.rs": head}, "rust")

    assert [(c.status, c.target.key) for c in prepared.changed] == [("layout", "a.rs::f")]
    assert silent_candidates(prepared, []) == ([], [])


TWINS = "def {first}():\n    return compute(1)\n\n\ndef {second}():\n    return compute(1)\n\n\ndef keep():\n    return 0\n"


def removed_twins(tmp_path: Path):
    """Two removals with one shape and two additions with the same shape: the engine's rename is a guess per pair."""
    base = TWINS.format(first="alpha", second="beta")
    head = TWINS.format(first="gamma", second="delta")
    prepared = a_range(tmp_path, {"m.py": base}, {"m.py": head}, "python")
    removed = [c.base for c in prepared.changed if c.status == "removed"]
    assert sorted(d.name for d in removed) == ["alpha", "beta"]
    return prepared, removed


def test_one_head_entry_accounts_for_one_removed_definition_and_two_entries_for_two(tmp_path: Path):
    """S12: hono Node.index and Node.varIndex were both accounted by the one entry `#index`."""
    prepared, removed = removed_twins(tmp_path)

    one = [pairing(prepared.head, prepared.base, d, ["m.py::gamma"], removed) for d in removed]
    both = [pairing(prepared.head, prepared.base, d, ["m.py::gamma", "m.py::delta"], removed) for d in removed]

    assert sorted(p[0] for p in one if p) == ["m.py::gamma"]  # the second removal is lost
    assert sorted(p[0] for p in both if p) == ["m.py::delta", "m.py::gamma"]


def test_a_head_definition_the_base_already_had_is_no_rename_target(tmp_path: Path):
    """S12: a removal is not a rename to an unrelated definition that stood before the range and has the same body."""
    base = TWINS.format(first="alpha", second="twin")
    head = "def twin():\n    return compute(1)\n\n\ndef keep():\n    return 0\n"
    prepared = a_range(tmp_path, {"m.py": base}, {"m.py": head}, "python")
    [removed] = [c.base for c in prepared.changed if c.status == "removed"]

    assert removed.name == "alpha"
    assert pairing(prepared.head, prepared.base, removed, ["m.py::twin"], [removed]) is None


LIBRARY = "def prompt(text):\n    return text\n"
PROMPT = "scip-python python click 0 `click.termui`/prompt()."

SHADOWS = {
    "a string": 'def go():\n    text = """\nprompt = foo\n"""\n    return prompt("x")\n',
    "a docstring": 'def go():\n    """Usage:\n    prompt = foo\n    """\n    return prompt("x")\n',
}


def reached(tmp_path: Path, caller: str, line: int) -> list[str]:
    snap = snapshot(
        tmp_path,
        {"termui.py": LIBRARY, "use.py": caller},
        {
            "termui.py": [occurrence(at(LIBRARY, 0, "prompt"), PROMPT, ROLE_DEFINITION)],
            "use.py": [occurrence(at(caller, line, "prompt", nth=caller.splitlines()[line].count("prompt") - 1), PROMPT)],
        },
    )
    return [r.from_key for r in relations_to(snap, target(snap, "termui.py::prompt"))]


@pytest.mark.parametrize("case", SHADOWS)
def test_text_that_only_looks_like_a_binding_does_not_shadow_a_real_call(tmp_path: Path, case):
    """R4 shadow: the filter reads the caller's lines for `name =`; a string or a docstring binds nothing."""
    caller = SHADOWS[case]

    assert reached(tmp_path, caller, caller.splitlines().index('    return prompt("x")')) == ["use.py::go"]


def test_a_local_binding_of_the_name_still_shadows_the_call_and_a_comment_does_not(tmp_path: Path):
    local = "def go():\n    prompt = 1\n    return prompt('x')\n"
    comment = "def go():\n    # prompt = foo\n    return prompt('x')\n"

    assert reached(tmp_path / "local", local, 2) == []
    assert reached(tmp_path / "comment", comment, 2) == ["use.py::go"]


ESCAPING_LOCALS = {
    "python": (
        "m.py",
        "def option(name):\n    def decorator(f):\n        return f(name)\n\n    return decorator\n",
        "m.py::option.decorator",
        "option",
        "1-5",
    ),
    "typescript": (
        "m.ts",
        "export function create() {\n  const got = () => 1\n  got.extend = function extend(x) {\n    return x\n  }\n  return got\n}\n",
        "m.ts::create.extend",
        "create",
        "1-7",
    ),
}


@pytest.mark.parametrize("language", ESCAPING_LOCALS)
def test_a_local_the_function_hands_on_is_the_functions_change_when_named_and_lost_when_it_is_not(tmp_path: Path, language):
    """S7, a judgement the oracle keeps: the product holds no definition for a local, so naming the function states the
    change (click 0b7a9acc63, got 1e157c43c4); a function the answer leaves out takes its local with it."""
    path, source, key, outer, lines = ESCAPING_LOCALS[language]
    edited = source.replace("f(name)", "f(name.upper())").replace("return x\n", "return x + 1\n")
    prepared = a_range(tmp_path, {path: source}, {path: edited}, language)

    assert silent_candidates(prepared, [entry(path, outer, lines)]) == ([key], [])
    assert silent_candidates(prepared, []) == ([key], [key])


def test_the_removal_an_entry_stands_for_is_the_one_whose_name_it_resembles(tmp_path: Path):
    """S12, jsoup 7d46675b: `jetty` and `proxy` are renamed `Jetty` and `Proxy`; the answer names `Proxy` and is silent on
    `Jetty`: the removal that goes unnamed is `jetty`, whichever sorts first."""
    server = "class Hub {\n    private static final Server {first} = newServer();\n    private static final Server {second} = newServer();\n}\n"
    base = server.replace("{first}", "jetty").replace("{second}", "proxy")
    head = server.replace("{first}", "Jetty").replace("{second}", "Proxy")
    prepared = a_range(tmp_path, {"Hub.java": base}, {"Hub.java": head}, "java")
    removed = [c.base for c in prepared.changed if c.status == "removed"]

    paired = {d.name: pairing(prepared.head, prepared.base, d, ["Hub.java::Hub.Proxy"], removed) for d in removed}

    assert sorted(paired) == ["jetty", "proxy"]
    assert paired["proxy"] == ("Hub.java::Hub.Proxy", "Hub.java::Hub.proxy")
    assert paired["jetty"] is None
