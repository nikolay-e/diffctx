from __future__ import annotations

from .types import Fragment, FragmentId

_SIGNATURE_ELIGIBLE_KINDS = frozenset({"function", "class", "method", "struct", "interface", "enum"})
_MIN_LINES_FOR_SIGNATURE = 5


def _count_brackets_outside_strings(line: str) -> tuple[int, int, int, int]:
    open_parens = 0
    close_parens = 0
    open_braces = 0
    close_braces = 0
    in_string: str | None = None
    escaped = False
    for ch in line:
        if in_string is not None:
            if escaped:
                escaped = False
            elif ch == "\\":
                escaped = True
            elif ch == in_string:
                in_string = None
            continue
        if ch in ("'", '"', "`"):
            in_string = ch
            escaped = False
            continue
        if ch == "(":
            open_parens += 1
        elif ch == ")":
            close_parens += 1
        elif ch == "{":
            open_braces += 1
        elif ch == "}":
            close_braces += 1
    return open_parens, close_parens, open_braces, close_braces


def _find_signature_end(lines: list[str]) -> int:
    paren_depth = 0
    for i, line in enumerate(lines):
        op, cp, ob, cb = _count_brackets_outside_strings(line)
        paren_depth += op - cp
        if ob - cb > 0:
            return i + 1
        if paren_depth <= 0 and i > 0:
            return i + 1
    return min(2, len(lines))


def _generate_signature_variants(fragments: list[Fragment]) -> list[Fragment]:
    signatures: list[Fragment] = []
    seen: set[FragmentId] = set()
    for frag in fragments:
        if frag.kind not in _SIGNATURE_ELIGIBLE_KINDS:
            continue
        if frag.line_count < _MIN_LINES_FOR_SIGNATURE:
            continue
        lines = frag.content.splitlines()
        sig_end = _find_signature_end(lines)
        sig_content = "\n".join(lines[:sig_end])
        sig_id = FragmentId(frag.path, frag.start_line, frag.start_line + sig_end - 1)
        if sig_id in seen:
            continue
        seen.add(sig_id)
        signatures.append(
            Fragment(
                id=sig_id,
                kind=f"{frag.kind}_signature",
                content=sig_content,
                identifiers=frag.identifiers,
                symbol_name=frag.symbol_name,
            )
        )
    return signatures
