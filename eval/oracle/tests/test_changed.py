from changed import changed_definitions
from gitrepo import FileChange, Hunk
from model import Definition


def definition(path, name, start, end, body="b", signature="()", container=(), shape=None):
    return Definition(
        path=path,
        kind="function",
        name=name,
        container=container,
        start=start,
        end=end,
        name_row=start - 1,
        name_col=4,
        signature=signature,
        body_hash=f"{name}:{body}",
        shape_hash=shape or body,
    )


def statuses(found):
    return {(c.base.key if c.base else c.target.key): c.status for c in found}


def test_a_body_only_edit_counts_though_the_name_line_is_untouched():
    base = {"m.py": [definition("m.py", "total", 1, 5, body="old"), definition("m.py", "other", 7, 9)]}
    head = {"m.py": [definition("m.py", "total", 1, 5, body="new"), definition("m.py", "other", 7, 9)]}
    change = FileChange("m.py", "m.py", (Hunk(4, 1, 4, 1),))

    found = changed_definitions([change], lambda p: base[p], lambda p: head[p])

    assert statuses(found) == {"m.py::total": "modified"}


def test_a_class_is_untouched_when_only_a_method_changes():
    cls_base = definition("m.py", "Shop", 1, 9)
    method_base = definition("m.py", "total", 3, 6, container=("Shop",), body="old")
    cls_head = definition("m.py", "Shop", 1, 9, body="shop-new")
    method_head = definition("m.py", "total", 3, 6, container=("Shop",), body="new")
    change = FileChange("m.py", "m.py", (Hunk(5, 1, 5, 1),))

    found = changed_definitions([change], lambda p: [cls_base, method_base], lambda p: [cls_head, method_head])

    assert statuses(found) == {"m.py::Shop.total": "modified"}


def test_a_deleted_definition_is_removed_and_a_pure_deletion_inside_a_body_modifies_it():
    base = [definition("m.py", "keep", 1, 6, body="old"), definition("m.py", "gone", 8, 10)]
    head = [definition("m.py", "keep", 1, 4, body="new")]
    change = FileChange("m.py", "m.py", (Hunk(3, 2, 2, 0), Hunk(8, 3, 6, 0)))

    found = changed_definitions([change], lambda p: base, lambda p: head)

    assert statuses(found) == {"m.py::keep": "modified", "m.py::gone": "removed"}


def test_a_move_between_files_is_no_removal():
    old = definition("a.py", "mover", 1, 3, body="same")
    new = definition("b.py", "mover", 1, 3, body="same")
    changes = [FileChange("a.py", "a.py", (Hunk(1, 3, 0, 0),)), FileChange(None, "b.py", (Hunk(0, 0, 1, 3),))]

    found = changed_definitions(changes, lambda p: {"a.py": [old], "b.py": []}[p], lambda p: {"a.py": [], "b.py": [new]}[p])

    assert [(c.status, c.base.key, c.head.key) for c in found] == [("moved", "a.py::mover", "b.py::mover")]


def test_a_rename_with_the_same_shape_is_no_removal():
    old = definition("m.py", "old_name", 1, 3, body="x", shape="shape")
    new = definition("m.py", "new_name", 1, 3, body="y", shape="shape")
    change = FileChange("m.py", "m.py", (Hunk(1, 1, 1, 1),))

    found = changed_definitions([change], lambda p: [old], lambda p: [new])

    assert [(c.status, c.base.name, c.head.name) for c in found] == [("renamed", "old_name", "new_name")]


def test_formatting_only_edit_is_layout():
    base = definition("m.py", "f", 1, 3, body="same")
    head = definition("m.py", "f", 1, 4, body="same")
    change = FileChange("m.py", "m.py", (Hunk(2, 1, 2, 2),))

    found = changed_definitions([change], lambda p: [base], lambda p: [head])

    assert statuses(found) == {"m.py::f": "layout"}


def test_a_git_rename_with_edits_pairs_definitions_across_the_path_change():
    base = definition("old.py", "f", 1, 3, body="old")
    head = definition("new.py", "f", 1, 3, body="new")
    change = FileChange("old.py", "new.py", (Hunk(2, 1, 2, 1),))

    found = changed_definitions([change], lambda p: [base], lambda p: [head])

    assert [(c.status, c.base.key, c.head.key) for c in found] == [("modified", "old.py::f", "new.py::f")]


def test_an_added_definition_and_non_code_files():
    new = definition("m.py", "fresh", 4, 6)
    changes = [FileChange("m.py", "m.py", (Hunk(3, 0, 4, 3),)), FileChange("README.md", "README.md", (Hunk(1, 1, 1, 1),))]

    found = changed_definitions(changes, lambda p: [], lambda p: [new])

    assert statuses(found) == {"m.py::fresh": "added"}
