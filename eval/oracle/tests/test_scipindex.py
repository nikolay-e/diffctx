from pathlib import Path

from scipindex import ROLE_DEFINITION, byte_column, parse_symbol, read_index


def varint(value: int) -> bytes:
    out = bytearray()
    while True:
        low = value & 0x7F
        value >>= 7
        out.append(low | (0x80 if value else 0))
        if not value:
            return bytes(out)


def field_varint(number: int, value: int) -> bytes:
    return varint(number << 3) + varint(value)


def field_bytes(number: int, payload: bytes) -> bytes:
    return varint(number << 3 | 2) + varint(len(payload)) + payload


def field_text(number: int, text: str) -> bytes:
    return field_bytes(number, text.encode())


def occurrence_legacy(rng: list[int], symbol: str, roles: int) -> bytes:
    packed = b"".join(varint(v) for v in rng)
    return field_bytes(1, packed) + field_text(2, symbol) + field_varint(3, roles)


def occurrence_typed(line: int, start: int, end: int, symbol: str) -> bytes:
    single = field_varint(1, line) + field_varint(2, start) + field_varint(3, end)
    return field_bytes(8, single) + field_text(2, symbol)


def test_read_index_decodes_both_range_encodings_and_relationships(tmp_path: Path):
    related = field_text(1, "scip-python python p 1 `m`/Base#run().") + field_varint(3, 1)
    info = field_text(1, "scip-python python p 1 `m`/Sub#run().") + field_bytes(4, related) + field_varint(5, 26)
    document = (
        field_text(1, "m.py")
        + field_bytes(2, occurrence_legacy([3, 4, 9], "sym#a", ROLE_DEFINITION))
        + field_bytes(2, occurrence_legacy([5, 1, 6, 2], "sym#a", 0))
        + field_bytes(2, occurrence_typed(7, 2, 8, "sym#b"))
        + field_bytes(3, info)
        + field_varint(6, 2)
    )
    tool = field_text(1, "scip-python") + field_text(2, "0.6.8")
    path = tmp_path / "index.scip"
    path.write_bytes(field_bytes(1, field_bytes(2, tool)) + field_bytes(2, document))

    index = read_index(path)

    assert (index.tool_name, index.tool_version) == ("scip-python", "0.6.8")
    doc = index.documents[0]
    assert doc.path == "m.py" and doc.position_encoding == 2
    first, second, third = doc.occurrences
    assert (first.line, first.col, first.end_line, first.end_col, first.is_definition) == (3, 4, 3, 9, True)
    assert (second.line, second.end_line, second.is_definition) == (5, 6, False)
    assert (third.line, third.col, third.end_col, third.symbol) == (7, 2, 8, "sym#b")
    assert doc.symbols[0].kind == 26
    assert doc.symbols[0].implements == ["scip-python python p 1 `m`/Base#run()."]


def test_symbol_descriptors_give_container_and_name():
    method = parse_symbol("scip-python python click 8.1 `click.core`/Command#invoke().")
    assert method is not None
    assert (method.container, method.name, method.module) == (("Command",), "invoke", "click.core")

    function = parse_symbol("scip-typescript npm got 14.0.0 `source/core/index.ts`/makeRequest().")
    assert function is not None
    assert (function.container, function.name, function.module) == ((), "makeRequest", "source/core/index.ts")

    constant = parse_symbol("scip-python python shop 0.1 `shop.pricing`/RATE.")
    assert constant is not None and (constant.container, constant.name) == ((), "RATE")

    parameter = parse_symbol("scip-python python shop 0.1 `shop.pricing`/total().(items)")
    assert parameter is not None and (parameter.container, parameter.name, parameter.suffix) == (("total",), "items", "(")


def test_local_and_malformed_symbols_have_no_path():
    assert parse_symbol("local 12") is None
    assert parse_symbol("not a symbol") is None


def test_escaped_spaces_and_backticks_survive():
    parsed = parse_symbol("scip-x my  manager pkg 1 `a``b`/Thing#")
    assert parsed is not None and (parsed.module, parsed.name) == ("a`b", "Thing")


def test_byte_column_follows_the_position_encoding():
    line = "é = 🚀 + total".encode()
    # UTF-16 units: the rocket counts twice, so 'total' starts at unit 9
    assert line[byte_column(line, 9, 2) :].decode() == "total"
    # UTF-32 units: the rocket counts once, so 'total' starts at unit 8
    assert line[byte_column(line, 8, 3) :].decode() == "total"
    assert byte_column(b"plain total", 6, 2) == 6
    assert byte_column(line, 12, 1) == 12
