from __future__ import annotations

import re
from pathlib import Path

from ...config.weights import EDGE_WEIGHTS
from ...types import Fragment
from ..base import EdgeBuilder, EdgeDict, FragmentIndex, discover_files_by_refs

_LATEX_EXTS = {".tex", ".sty", ".cls", ".bib", ".bst", ".dtx"}

_INPUT_RE = re.compile(r"\\(?:input|include|subfile)\s*\{([^}]{1,300})\}", re.MULTILINE)
_USEPACKAGE_RE = re.compile(
    r"\\(?:usepackage|RequirePackage)(?:\[[^\]]*\])?\s*\{([^}]{1,300})\}",
    re.MULTILINE,
)
_DOCUMENTCLASS_RE = re.compile(r"\\documentclass(?:\[[^\]]*\])?\s*\{([^}]{1,300})\}", re.MULTILINE)
_BIBLIOGRAPHY_RE = re.compile(r"\\(?:bibliography|addbibresource)\s*\{([^}]{1,300})\}", re.MULTILINE)


def _is_latex_file(path: Path) -> bool:
    return path.suffix.lower() in _LATEX_EXTS


def _extract_input_refs(content: str) -> set[str]:
    refs: set[str] = set()
    for m in _INPUT_RE.finditer(content):
        refs.add(m.group(1))
    return refs


def _extract_package_refs(content: str) -> set[str]:
    pkgs: set[str] = set()
    for m in _USEPACKAGE_RE.finditer(content):
        for pkg in m.group(1).split(","):
            pkgs.add(pkg.strip())
    for m in _DOCUMENTCLASS_RE.finditer(content):
        pkgs.add(m.group(1).strip())
    return pkgs


def _extract_bib_refs(content: str) -> set[str]:
    bibs: set[str] = set()
    for m in _BIBLIOGRAPHY_RE.finditer(content):
        for bib in m.group(1).split(","):
            bibs.add(bib.strip())
    return bibs


def _add_tex_extension(name: str) -> str:
    if "." not in name:
        return name + ".tex"
    return name


def _package_to_filenames(pkg: str) -> list[str]:
    results = []
    clean = pkg.strip()
    results.append(f"{clean}.sty")
    results.append(f"{clean}.cls")
    return results


def _bib_to_filename(bib: str) -> str:
    clean = bib.strip()
    if not clean.endswith(".bib"):
        return f"{clean}.bib"
    return clean


class LatexEdgeBuilder(EdgeBuilder):
    weight = 0.60
    input_weight = EDGE_WEIGHTS["latex_input"].forward
    package_weight = EDGE_WEIGHTS["latex_package"].forward
    bib_weight = EDGE_WEIGHTS["latex_bib"].forward
    reverse_weight_factor = EDGE_WEIGHTS["latex_input"].reverse_factor

    def discover_related_files(
        self,
        changed_files: list[Path],
        all_candidate_files: list[Path],
        repo_root: Path | None = None,
    ) -> list[Path]:
        latex_changed = [f for f in changed_files if _is_latex_file(f)]
        if not latex_changed:
            return []

        refs: set[str] = set()
        for f in latex_changed:
            try:
                content = f.read_text(encoding="utf-8")
                for inp in _extract_input_refs(content):
                    tex_name = _add_tex_extension(inp)
                    refs.add(tex_name)
                    refs.add(tex_name.split("/")[-1].lower())

                for pkg in _extract_package_refs(content):
                    for fname in _package_to_filenames(pkg):
                        refs.add(fname.lower())

                for bib in _extract_bib_refs(content):
                    refs.add(_bib_to_filename(bib).lower())
            except (OSError, UnicodeDecodeError):
                continue

        self._discover_reverse_refs(latex_changed, all_candidate_files, refs)

        return discover_files_by_refs(refs, changed_files, all_candidate_files, repo_root)

    def _discover_reverse_refs(
        self,
        latex_changed: list[Path],
        all_candidate_files: list[Path],
        refs: set[str],
    ) -> None:
        changed_stems: set[str] = set()
        for f in latex_changed:
            changed_stems.add(f.stem.lower())
            changed_stems.add(f.name.lower())

        for candidate in all_candidate_files:
            if not _is_latex_file(candidate):
                continue
            try:
                content = candidate.read_text(encoding="utf-8")
                self._check_candidate_refs(content, candidate, changed_stems, refs)
            except (OSError, UnicodeDecodeError):
                continue

    def _check_candidate_refs(
        self,
        content: str,
        candidate: Path,
        changed_stems: set[str],
        refs: set[str],
    ) -> None:
        for inp in _extract_input_refs(content):
            inp_stem = inp.split("/")[-1].replace(".tex", "").lower()
            if inp_stem in changed_stems:
                refs.add(candidate.name.lower())
        for pkg in _extract_package_refs(content):
            if pkg.lower() in changed_stems:
                refs.add(candidate.name.lower())
        for bib in _extract_bib_refs(content):
            bib_name = bib.strip().lower()
            if bib_name in changed_stems or f"{bib_name}.bib" in changed_stems:
                refs.add(candidate.name.lower())

    def build(self, fragments: list[Fragment], repo_root: Path | None = None) -> EdgeDict:
        latex_frags = [f for f in fragments if _is_latex_file(f.path)]
        if not latex_frags:
            return {}

        edges: EdgeDict = {}
        idx = FragmentIndex(fragments, repo_root)

        for lf in latex_frags:
            self._add_fragment_edges(lf, idx, edges)

        return edges

    def _add_fragment_edges(self, lf: Fragment, idx: FragmentIndex, edges: EdgeDict) -> None:
        for inp in _extract_input_refs(lf.content):
            tex_name = _add_tex_extension(inp)
            self.link_by_name_or_path(lf.id, tex_name, idx, edges, self.input_weight)

        for pkg in _extract_package_refs(lf.content):
            for fname in _package_to_filenames(pkg):
                self.link_by_name_or_path(lf.id, fname, idx, edges, self.package_weight)

        for bib in _extract_bib_refs(lf.content):
            bib_file = _bib_to_filename(bib)
            self.link_by_name_or_path(lf.id, bib_file, idx, edges, self.bib_weight)
