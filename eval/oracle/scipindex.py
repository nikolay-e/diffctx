"""A reader for SCIP indexes: the protobuf wire format decoded by hand, field numbers from scip.proto v0.10.0.

The oracle needs documents, occurrences and a few symbol facts, so it reads those fields and skips the rest.
Reading the wire format directly keeps the instrument free of a protobuf runtime and of generated code.
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path

ROLE_DEFINITION = 0x1
ROLE_IMPORT = 0x2
ROLE_WRITE = 0x4
ROLE_FORWARD_DEFINITION = 0x40

ENCODING_UTF8 = 1
ENCODING_UTF32 = 3

VARINT, FIXED64, LENGTH, FIXED32 = 0, 1, 2, 5


@dataclass(slots=True)
class Occurrence:
    line: int
    col: int
    end_line: int
    end_col: int
    symbol: str
    roles: int

    @property
    def is_definition(self) -> bool:
        return bool(self.roles & (ROLE_DEFINITION | ROLE_FORWARD_DEFINITION))


@dataclass(slots=True)
class SymbolInfo:
    symbol: str
    kind: int = 0
    implements: list[str] = field(default_factory=list)


@dataclass(slots=True)
class Document:
    path: str
    position_encoding: int = 0
    occurrences: list[Occurrence] = field(default_factory=list)
    symbols: list[SymbolInfo] = field(default_factory=list)


@dataclass(slots=True)
class Index:
    tool_name: str = ""
    tool_version: str = ""
    documents: list[Document] = field(default_factory=list)


def _varint(buf: bytes, pos: int) -> tuple[int, int]:
    value = shift = 0
    while True:
        byte = buf[pos]
        pos += 1
        value |= (byte & 0x7F) << shift
        if byte < 0x80:
            return value, pos
        shift += 7


def _fields(buf: bytes) -> Iterator[tuple[int, int | bytes]]:
    """(field number, value) pairs of one message: an int for a varint, bytes for a length-delimited field."""
    pos, end = 0, len(buf)
    while pos < end:
        tag, pos = _varint(buf, pos)
        number, wire = tag >> 3, tag & 7
        if wire == VARINT:
            value, pos = _varint(buf, pos)
            yield number, value
        elif wire == LENGTH:
            size, pos = _varint(buf, pos)
            yield number, buf[pos : pos + size]
            pos += size
        elif wire == FIXED64:
            pos += 8
        elif wire == FIXED32:
            pos += 4
        else:
            raise ValueError(f"unsupported wire type {wire} in field {number}")


def _packed(buf: bytes) -> list[int]:
    out: list[int] = []
    pos = 0
    while pos < len(buf):
        value, pos = _varint(buf, pos)
        out.append(value)
    return out


def _text(value: int | bytes) -> str:
    return value.decode("utf-8", "replace") if isinstance(value, bytes) else ""


def _single_line(buf: bytes) -> tuple[int, int, int, int]:
    line = start = end = 0
    for number, value in _fields(buf):
        if number == 1:
            line = int(value) if isinstance(value, int) else 0
        elif number == 2:
            start = int(value) if isinstance(value, int) else 0
        elif number == 3:
            end = int(value) if isinstance(value, int) else 0
    return line, start, line, end


def _multi_line(buf: bytes) -> tuple[int, int, int, int]:
    got = [0, 0, 0, 0]
    for number, value in _fields(buf):
        if 1 <= number <= 4 and isinstance(value, int):
            got[number - 1] = value
    return got[0], got[1], got[2], got[3]


def _legacy_range(values: list[int]) -> tuple[int, int, int, int] | None:
    if len(values) == 3:
        return values[0], values[1], values[0], values[2]
    if len(values) == 4:
        return values[0], values[1], values[2], values[3]
    return None


def _occurrence(buf: bytes) -> Occurrence | None:
    rng: tuple[int, int, int, int] | None = None
    legacy: list[int] = []
    symbol, roles = "", 0
    for number, value in _fields(buf):
        if number == 1:
            legacy += _packed(value) if isinstance(value, bytes) else [value]
        elif number == 2:
            symbol = _text(value)
        elif number == 3 and isinstance(value, int):
            roles = value
        elif number == 8 and isinstance(value, bytes):
            rng = _single_line(value)
        elif number == 9 and isinstance(value, bytes):
            rng = _multi_line(value)
    rng = rng or _legacy_range(legacy)
    if rng is None:
        return None
    return Occurrence(rng[0], rng[1], rng[2], rng[3], symbol, roles)


def _symbol_info(buf: bytes) -> SymbolInfo:
    info = SymbolInfo("")
    for number, value in _fields(buf):
        if number == 1:
            info.symbol = _text(value)
        elif number == 5 and isinstance(value, int):
            info.kind = value
        elif number == 4 and isinstance(value, bytes):
            related, implements = "", False
            for sub, item in _fields(value):
                if sub == 1:
                    related = _text(item)
                elif sub == 3 and item:
                    implements = True
            if implements and related:
                info.implements.append(related)
    return info


def _document(buf: bytes) -> Document:
    doc = Document("")
    for number, value in _fields(buf):
        if number == 1:
            doc.path = _text(value)
        elif number == 2 and isinstance(value, bytes):
            occurrence = _occurrence(value)
            if occurrence is not None:
                doc.occurrences.append(occurrence)
        elif number == 3 and isinstance(value, bytes):
            doc.symbols.append(_symbol_info(value))
        elif number == 6 and isinstance(value, int):
            doc.position_encoding = value
    return doc


def read_index(path: Path) -> Index:
    index = Index()
    for number, value in _fields(path.read_bytes()):
        if number == 2 and isinstance(value, bytes):
            index.documents.append(_document(value))
        elif number == 1 and isinstance(value, bytes):
            for sub, item in _fields(value):
                if sub == 2 and isinstance(item, bytes):
                    for tool_field, tool_value in _fields(item):
                        if tool_field == 1:
                            index.tool_name = _text(tool_value)
                        elif tool_field == 2:
                            index.tool_version = _text(tool_value)
    return index


def byte_column(line: bytes, col: int, encoding: int) -> int:
    """A SCIP column in the document's position encoding as a byte offset into `line`.

    An unspecified encoding is read as UTF-16, the unit both pinned indexers count in for non-ASCII text.
    """
    if encoding == ENCODING_UTF8 or line.isascii():
        return col
    text = line.decode("utf-8", "replace")
    counted = 0
    for offset, char in enumerate(text):
        if counted >= col:
            return len(text[:offset].encode("utf-8"))
        counted += 2 if encoding != ENCODING_UTF32 and ord(char) > 0xFFFF else 1
    return len(line)


@dataclass(frozen=True)
class SymbolPath:
    """A SCIP symbol read as the oracle names a definition: container chain and name."""

    container: tuple[str, ...]
    name: str
    suffix: str
    module: str


_SIMPLE = set("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_+-$")


def _split_header(symbol: str) -> tuple[list[str], str] | None:
    """`scheme manager package version descriptors`, a double space standing for a space inside a part."""
    parts: list[str] = []
    current: list[str] = []
    i = 0
    while i < len(symbol) and len(parts) < 4:
        char = symbol[i]
        if char == " ":
            if symbol[i + 1 : i + 2] == " ":
                current.append(" ")
                i += 2
                continue
            parts.append("".join(current))
            current = []
        else:
            current.append(char)
        i += 1
    return (parts, symbol[i:]) if len(parts) == 4 else None


def _read_name(text: str, i: int) -> tuple[str, int]:
    if text[i : i + 1] == "`":
        i += 1
        out: list[str] = []
        while i < len(text):
            if text[i] == "`":
                if text[i + 1 : i + 2] == "`":
                    out.append("`")
                    i += 2
                    continue
                return "".join(out), i + 1
            out.append(text[i])
            i += 1
        return "".join(out), i
    start = i
    while i < len(text) and text[i] in _SIMPLE:
        i += 1
    return text[start:i], i


def parse_symbol(symbol: str) -> SymbolPath | None:
    """None for a local symbol or one this reader cannot place; the descriptor grammar is in scip.proto."""
    if symbol.startswith("local "):
        return None
    header = _split_header(symbol)
    if header is None:
        return None
    descriptors = header[1]
    chain: list[tuple[str, str]] = []
    i = 0
    while i < len(descriptors):
        if descriptors[i] in "([":
            closer = ")" if descriptors[i] == "(" else "]"
            end = descriptors.find(closer, i)
            if end < 0:
                return None
            chain.append((descriptors[i + 1 : end], descriptors[i]))
            i = end + 1
            continue
        name, i = _read_name(descriptors, i)
        suffix = descriptors[i : i + 1]
        i += 1
        if suffix == "(":
            end = descriptors.find(")", i)
            if end < 0:
                return None
            i = end + 2
            suffix = "()."
        chain.append((name, suffix))
    members = [c for c in chain if c[1] in ("#", ".", "().", ":", "(", "[")]
    modules = [c[0] for c in chain if c[1] == "/"]
    if not members:
        return None
    *outer, last = members
    return SymbolPath(tuple(name for name, _ in outer), last[0], last[1], modules[-1] if modules else "")
