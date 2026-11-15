"""R-layer: a language table is data. A row names a language and its facts, never a function, closure or trait object."""

import re
from pathlib import Path

ROOT = Path(__file__).parent.parent

LITERALS = re.compile(
    r"""r(?P<h>\#*)"(?:.|\n)*?"(?P=h)|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])'|//[^\n]*|/\*(?:.|\n)*?\*/""",
    re.VERBOSE,
)
DECLARATION = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+(?:mut\s+)?([A-Z][A-Z0-9_]*)\s*:")
TYPE_DEFINITION = re.compile(r"(?m)^[ \t]*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum|type)\s+(\w+)")
LANGUAGE_KEY = re.compile(r"\b(?:Family|Language|Lang\w*)\b|TABLE")
COLLECTION = re.compile(r"\[|\bVec<|Map<")
CODE_IN_TYPE = re.compile(r"\bfn\s*\(|\bFn(?:Mut|Once)?\b|Box\s*<\s*dyn")
CLOSURE = re.compile(r"(?<![|])\|(?![|=])[^|\n]*\||\|\|\s*[{A-Za-z_&(]")
OPENERS = {"(": ")", "[": "]", "{": "}", "<": ">"}
CLOSERS = set(OPENERS.values())


def _blank(source: str) -> str:
    return LITERALS.sub('""', source)


def _span(text: str, start: int, stop: str, angles: bool = False) -> str:
    """Text from start up to the first top-level `stop`, so brackets inside a type or value do not end it early."""
    depth = 0
    i = start
    while i < len(text):
        c = text[i]
        if text.startswith("->", i) or text.startswith("=>", i):
            i += 2
            continue
        if c == stop and depth <= 0:
            break
        if c in "<>" and not angles:
            pass
        elif c in OPENERS:
            depth += 1
        elif c in CLOSERS:
            depth -= 1
        i += 1
    return text[start:i]


def _body(text: str, start: int) -> str:
    """A struct, enum or alias from its name to the end of its braces, tuple fields or `;`."""
    head = re.compile(r"[{(=;]").search(text, start)
    if head is None or head.group() == ";":
        return text[start : head.end() if head else len(text)]
    if head.group() == "=":
        return text[start : start + len(_span(text, head.end(), ";"))]
    close = _span(text, head.end(), "}" if head.group() == "{" else ")")
    return text[start : head.end() + len(close) + 1]


def _definitions(sources: dict[str, str]) -> dict[str, dict[str, str]]:
    return {path: {m.group(1): _body(text, m.end()) for m in TYPE_DEFINITION.finditer(text)} for path, text in sources.items()}


def _resolved(type_text: str, own: dict[str, str], everywhere: dict[str, str]) -> str:
    seen: set[str] = set()
    pending = re.findall(r"\b[A-Z]\w*\b", type_text)
    parts = [type_text]
    while pending:
        name = pending.pop()
        body = own.get(name) or everywhere.get(name)
        if name in seen or body is None:
            continue
        seen.add(name)
        parts.append(body)
        pending.extend(re.findall(r"\b[A-Z]\w*\b", body))
    return "\n".join(parts)


def table_violations(sources: dict[str, str]) -> tuple[list[str], list[str]]:
    sources = {path: _blank(text) for path, text in sources.items()}
    definitions = _definitions(sources)
    everywhere = {name: body for defined in definitions.values() for name, body in defined.items()}
    checked, violations = [], []
    for path, text in sources.items():
        for m in DECLARATION.finditer(text):
            name = m.group(1)
            type_text = _span(text, m.end(), "=", angles=True)
            if not COLLECTION.search(type_text + name):
                continue
            resolved = _resolved(type_text, definitions[path], everywhere)
            if not LANGUAGE_KEY.search(resolved + name):
                continue
            checked.append(f"{path}::{name}")
            value = _span(text, m.end() + len(type_text) + 1, ";")
            what = CODE_IN_TYPE.search(resolved) or (None if "Lazy" in type_text else CLOSURE.search(value))
            if what:
                violations.append(f"{path}::{name} holds code ({what.group(0).strip()!r})")
    return checked, violations


def _product_sources() -> dict[str, str]:
    return {str(p.relative_to(ROOT)): p.read_text(encoding="utf-8") for p in sorted(ROOT.glob("crates/*/src/**/*.rs"))}


def test_language_tables_hold_data_only():
    checked, violations = table_violations(_product_sources())
    assert checked, "no language table found: the scan has stopped seeing the tree"
    assert not violations, "; ".join(violations) + f"; tables checked ({len(checked)}): {', '.join(checked)}"


def test_a_planted_function_pointer_or_closure_in_a_table_is_found():
    clean = 'struct Row { family: Family, ext: &\'static str }\nstatic TABLE: &[Row] = &[Row { family: Family::C, ext: ".c" }];\n'
    assert table_violations({"a.rs": clean}) == (["a.rs::TABLE"], [])
    pointer = clean.replace("ext: &'static str", "extract: fn(&str) -> bool").replace('ext: ".c"', "extract: c::extract")
    assert table_violations({"a.rs": pointer})[1] == ["a.rs::TABLE holds code ('fn(')"]
    boxed = "static HANDLERS: &[(Family, Box<dyn Fn(&str)>)] = &[];\n"
    assert table_violations({"b.rs": boxed})[1] == ["b.rs::HANDLERS holds code ('Box<dyn')"]
    closure = "const GROUPS: &[(Family, bool)] = &[(Family::C, (|x| x)(true))];\n"
    assert table_violations({"c.rs": closure})[1] == ["c.rs::GROUPS holds code ('|x|')"]
    assert table_violations({"d.rs": 'const NOTE: &[&str] = &["fn(x)"];\n'}) == ([], [])
