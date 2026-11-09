"""Truth filters for systematic SCIP attributions the adjudication found wrong (Python and TypeScript/JavaScript).

A reference is dropped, and counted by filter, when the site cannot be the relation SCIP gave it:

- name: the identifier at the site is not the name of what it references (and not an import alias). httpx's
  `Response(...)` and `Headers(...)` were attributed to `Request.__init__`; got's `got`, a value built by the
  exported `create`, was attributed to `create` though the file never names it.
- shadow: a parameter or a local variable of the enclosing definition carries that name, so the site names the
  local (click's `confirm` assigns a local `prompt`; `Parameter.__init__` has a parameter `shell_complete`).
- homonym: a module-level name defined in two files, referenced from a file that reaches the target's file by no
  other reference. A wrapper and the function it calls share a name, and a test calling the wrapper through a
  package was attributed to the function two hops away (click's `get_pager_file`).

- fnexpr: the caller is a definition by assignment (`res.json = function json() {}`, whose extent is the whole
  statement) and the site lies outside the function that is assigned: it is the assignment target, a use by the
  module, which SCIP attributes to the function (R17; 7 of express's rows in the blind sample).

Imports are never filtered, and only the two script languages are: the other indexers have no adjudicated errors.
"""

from __future__ import annotations

import re
from collections import Counter, defaultdict
from typing import TYPE_CHECKING

from model import Definition, grammar_for
from scipindex import ROLE_IMPORT, Document, Occurrence, parse_symbol
from syntax import FUNCTION_VALUES, SCRIPT_GRAMMARS

if TYPE_CHECKING:
    from truth import Snapshot

FILTERED_GRAMMARS = ("python", "typescript", "tsx", "javascript")
CONSTRUCTORS = {"__init__", "constructor"}
EXEMPT_TEXT = {"super", "this", "self", "cls"}
MODULE_LEVEL_SUFFIXES = ("().", "#", ".")


def _implicit_protocol(target: Definition) -> bool:
    return target.name.startswith("__") and target.name.endswith("__") and target.name != "__init__"


def _bound(name: str, grammar: str) -> list[str]:
    n = re.escape(name)
    if grammar == "python":
        return [rf"(?m)^\s*{n}\s*(?::[^=\n]+)?=(?!=)", rf"\bfor\s+{n}\b", rf"\bas\s+{n}\b"]
    return [rf"\b(?:const|let|var)\s+{n}\b", rf"\bfor\s*\(\s*(?:const|let|var)\s+{n}\b"]


class Filters:
    def __init__(self, snapshot: Snapshot) -> None:
        self.snapshot = snapshot
        self.dropped: Counter[str] = Counter()
        self._aliases: dict[str, set[str]] = {}
        self._homes: dict[str, str] | None = None
        self._module_level: dict[str, set[str]] | None = None

    def rejects(self, doc: Document, occ: Occurrence, target: Definition, caller: Definition | None) -> str | None:
        """The name of the filter that drops this reference, or None when it stands."""
        grammar = grammar_for(doc.path)
        if grammar not in FILTERED_GRAMMARS or occ.roles & ROLE_IMPORT:
            return None
        if _implicit_protocol(target):
            return None  # `box()` reaches `Box.__call__`: the site names the object, not the method
        text = self.text(doc, occ)
        if text is None or text in EXEMPT_TEXT:
            return None
        for reason, failed in (
            ("name", self._wrong_name(doc, text, target)),
            ("shadow", self._shadowed(doc, occ, text, caller, grammar or "")),
            ("homonym", self._homonym_without_path(doc, occ, target)),
            ("fnexpr", self._outside_assigned_function(doc, occ, caller, grammar or "")),
        ):
            if failed:
                self.dropped[reason] += 1
                return reason
        return None

    def text(self, doc: Document, occ: Occurrence) -> str | None:
        if occ.line != occ.end_line:
            return None
        lines = self.snapshot.source(doc.path).split(b"\n")
        if occ.line >= len(lines):
            return None
        start, _, end = self.snapshot.position(doc, occ)
        return lines[occ.line][start:end].decode("utf-8", "replace") or None

    def import_aliases(self, doc: Document) -> set[str]:
        if doc.path not in self._aliases:
            texts = (self.text(doc, o) for o in doc.occurrences if o.roles & ROLE_IMPORT)
            self._aliases[doc.path] = {t for t in texts if t}
        return self._aliases[doc.path]

    def _wrong_name(self, doc: Document, text: str, target: Definition) -> bool:
        expected = {target.name}
        if target.name in CONSTRUCTORS and target.container:
            expected.add(target.container[-1])
        return text not in expected and text not in self.import_aliases(doc)

    def _shadowed(self, doc: Document, occ: Occurrence, text: str, caller: Definition | None, grammar: str) -> bool:
        if caller is None or not text.isidentifier():
            return False
        line = self.snapshot.source(doc.path).split(b"\n")[occ.line]
        start = self.snapshot.position(doc, occ)[0]
        if line[:start].rstrip().endswith(b"."):
            return False  # a member, not the bare name
        n = re.escape(text)
        if re.search(rf"(?:[(,]|\*\*?)\s*{n}\s*(?=[:=,)?])", caller.signature):
            return True
        lines = self.snapshot.source(doc.path).decode("utf-8", "replace").split("\n")
        before = "\n".join(lines[caller.start - 1 : occ.line])
        return any(re.search(pattern, before) for pattern in _bound(text, grammar))

    def _outside_assigned_function(self, doc: Document, occ: Occurrence, caller: Definition | None, grammar: str) -> bool:
        if caller is None or grammar not in SCRIPT_GRAMMARS:
            return False
        tree = self.snapshot.parsed(doc.path)
        if tree is None:
            return False
        name = tree.root_node.named_descendant_for_point_range(
            (caller.name_row, caller.name_col), (caller.name_row, caller.name_col + 1)
        )
        target = name.parent.parent if name is not None and name.parent is not None else None
        if target is None or target.type != "assignment_expression" or name.parent.type != "member_expression":
            return False
        value = target.child_by_field_name("right")
        if value is None or value.type not in FUNCTION_VALUES:
            return False
        site = (occ.line, self.snapshot.position(doc, occ)[0])
        return not value.start_point <= site < value.end_point

    def _homonym_without_path(self, doc: Document, occ: Occurrence, target: Definition) -> bool:
        if target.container or doc.path == target.path:
            return False
        if len(self._module_level_homes().get(target.name, ())) < 2:
            return False
        homes = self._symbol_homes()
        reached = {homes[o.symbol] for o in doc.occurrences if o.symbol != occ.symbol and o.symbol in homes}
        return target.path not in reached

    def _symbol_homes(self) -> dict[str, str]:
        if self._homes is None:
            self._homes = {
                o.symbol: d.path for d in self.snapshot.index.documents for o in d.occurrences if o.is_definition and o.symbol
            }
        return self._homes

    def _module_level_homes(self) -> dict[str, set[str]]:
        if self._module_level is None:
            found: dict[str, set[str]] = defaultdict(set)
            for symbol, path in self._symbol_homes().items():
                parsed = parse_symbol(symbol)
                if parsed is not None and not parsed.container and parsed.suffix in MODULE_LEVEL_SUFFIXES:
                    found[parsed.name].add(path)
            self._module_level = found
        return self._module_level
