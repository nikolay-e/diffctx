from __future__ import annotations

import logging
from pathlib import Path

from ..types import Fragment, FragmentId, extract_identifiers
from .base import GENERIC_MAX_LINES, MIN_FRAGMENT_WORDS, check_library_available, create_snippet, find_sentence_boundary


def _merge_small_fragments(fragments: list[Fragment], max_lines: int = 100) -> list[Fragment]:
    if len(fragments) <= 1:
        return fragments

    merged: list[Fragment] = []
    buffer: list[Fragment] = []
    buffer_lines = 0

    for frag in fragments:
        if buffer_lines + frag.line_count <= max_lines:
            buffer.append(frag)
            buffer_lines += frag.line_count
        else:
            if buffer:
                merged.append(_combine_fragments(buffer))
            buffer = [frag]
            buffer_lines = frag.line_count

    if buffer:
        merged.append(_combine_fragments(buffer))

    return merged


def _combine_fragments(frags: list[Fragment]) -> Fragment:
    if len(frags) == 1:
        return frags[0]

    combined = "\n".join(f.content.rstrip("\n") for f in frags) + "\n"
    combined_idents = frozenset().union(*(f.identifiers for f in frags))

    return Fragment(
        id=FragmentId(path=frags[0].path, start_line=frags[0].start_line, end_line=frags[-1].end_line),
        kind="section",
        content=combined,
        identifiers=combined_idents,
    )


def _import_pysbd() -> None:
    import pysbd  # noqa: F401


class PySBDTextStrategy:
    priority = 25

    def __init__(self) -> None:
        self._available = check_library_available(_import_pysbd)

    def can_handle(self, path: Path, _content: str) -> bool:
        if not self._available:
            return False
        return path.suffix.lower() in {".txt", ".text", ".rst", ".adoc", ""}

    def _create_paragraph_fragment(self, path: Path, lines: list[str], para_start: int, end_line: int) -> Fragment | None:
        if end_line < para_start:
            return None
        snippet = create_snippet(lines, para_start, end_line)
        if snippet is None:
            return None
        return Fragment(
            id=FragmentId(path=path, start_line=para_start, end_line=end_line),
            kind="paragraph",
            content=snippet,
            identifiers=extract_identifiers(snippet, profile="docs"),
        )

    def _flush_paragraph(
        self,
        path: Path,
        lines: list[str],
        para_sentences: list[str],
        para_start: int,
        end_line: int,
        fragments: list[Fragment],
    ) -> None:
        if not para_sentences:
            return
        combined = " ".join(para_sentences)
        if len(combined.split()) < MIN_FRAGMENT_WORDS:
            return
        frag = self._create_paragraph_fragment(path, lines, para_start, end_line)
        if frag:
            fragments.append(frag)

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        import pysbd

        lines = content.splitlines()
        if not lines:
            return []

        seg = pysbd.Segmenter(language="en", clean=False)
        sentences = seg.segment(content)

        fragments: list[Fragment] = []
        current_line = 1
        para_sentences: list[str] = []
        para_start = 1

        for sentence in sentences:
            sentence_lines = sentence.count("\n")

            if "\n\n" in sentence:
                self._flush_paragraph(path, lines, para_sentences, para_start, current_line - 1, fragments)
                para_sentences = []
                para_start = current_line + sentence_lines
            else:
                para_sentences.append(sentence)

            current_line += sentence_lines

        self._flush_paragraph(path, lines, para_sentences, para_start, len(lines), fragments)

        return _merge_small_fragments(fragments)


class ParagraphStrategy:
    priority = 20

    def can_handle(self, path: Path, _content: str) -> bool:
        return path.suffix.lower() in {".txt", ".text", ".rst", ".adoc", ""}

    def fragment(self, path: Path, content: str) -> list[Fragment]:
        lines = content.splitlines()
        if not lines:
            return []

        fragments: list[Fragment] = []
        para_start = 0
        in_para = False

        for i, line in enumerate(lines):
            is_blank = not line.strip()

            if not is_blank and not in_para:
                para_start = i
                in_para = True
            elif is_blank and in_para:
                if i > para_start:
                    fragments.extend(self._chunk_large_paragraph(path, lines, para_start, i - 1))
                in_para = False

        if in_para:
            fragments.extend(self._chunk_large_paragraph(path, lines, para_start, len(lines) - 1))

        return _merge_small_fragments(fragments)

    def _make_fragment(self, path: Path, lines: list[str], start: int, end: int) -> Fragment | None:
        snippet = create_snippet(lines, start + 1, end + 1)
        if snippet is None:
            logging.debug("Skipping empty fragment at %s:%d-%d", path, start + 1, end + 1)
            return None

        word_count = len(snippet.split())
        if word_count < MIN_FRAGMENT_WORDS:
            logging.debug(
                "Skipping fragment at %s:%d-%d (only %d words, need %d)",
                path,
                start + 1,
                end + 1,
                word_count,
                MIN_FRAGMENT_WORDS,
            )
            return None

        return Fragment(
            id=FragmentId(path=path, start_line=start + 1, end_line=end + 1),
            kind="paragraph",
            content=snippet,
            identifiers=extract_identifiers(snippet, profile="docs"),
        )

    def _chunk_large_paragraph(self, path: Path, lines: list[str], start: int, end: int) -> list[Fragment]:
        length = end - start + 1
        if length <= GENERIC_MAX_LINES:
            frag = self._make_fragment(path, lines, start, end)
            return [frag] if frag else []

        fragments: list[Fragment] = []
        chunk_start = start
        while chunk_start <= end:
            target_end = min(end, chunk_start + GENERIC_MAX_LINES - 1)
            chunk_end = find_sentence_boundary(lines, chunk_start, target_end)
            if chunk_end < chunk_start:
                chunk_end = target_end
            frag = self._make_fragment(path, lines, chunk_start, chunk_end)
            if frag:
                fragments.append(frag)
            chunk_start = chunk_end + 1
        return fragments
