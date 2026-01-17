from __future__ import annotations

import math
import re
import subprocess
from collections import Counter, defaultdict
from collections.abc import Callable
from dataclasses import dataclass, field
from heapq import nlargest
from pathlib import Path

from .constants import CODE_EXTENSIONS, CONFIG_EXTENSIONS
from .edges import collect_all_edges
from .edges.base import path_to_module
from .embeddings import _build_embedding_edges
from .stopwords import TokenProfile, filter_idents
from .types import Fragment, FragmentId, extract_identifier_list

_TOP_K_NEIGHBORS = 10
_MIN_SIMILARITY = 0.1
_BACKWARD_WEIGHT_FACTOR = 0.7
_LEXICAL_WEIGHT_MIN = 0.1
_LEXICAL_WEIGHT_MAX = 0.2
_HUB_PERCENTILE = 0.95

_MAX_DF_RATIO = 0.20
_MIN_IDF = 1.6
_MAX_POSTINGS = 200

_CONTAINMENT_WEIGHT = 0.50
_CONTAINMENT_REVERSE_WEIGHT = 0.35

_IMPORT_WEIGHT = 0.5
_IMPORT_REVERSE_WEIGHT = 0.35
_IMPORT_RE = re.compile(r"(?:from\s+([\w.]+)\s+import|import\s+([\w.]+))")

_TEST_WEIGHT_DIRECT = 0.60
_TEST_WEIGHT_NAMING = 0.50
_TEST_REVERSE_WEIGHT = 0.30

_CONFIG_CODE_WEIGHT = 0.45
_CONFIG_KEY_RE = re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*:", re.MULTILINE)
_TOML_KEY_RE = re.compile(r"^([a-zA-Z_][a-zA-Z0-9_-]*)\s*=", re.MULTILINE)
_TOML_SECTION_RE = re.compile(r"^\[([a-zA-Z_][a-zA-Z0-9_.-]*)\]", re.MULTILINE)

_SIBLING_WEIGHT = 0.05

_COCHANGE_WEIGHT = 0.40
_COCHANGE_MIN_COUNT = 2
_COCHANGE_MAX_FILES_PER_COMMIT = 30
_COCHANGE_COMMITS = 500
_COCHANGE_TIMEOUT = 10

_JAVA_EXT = ".java"
_SCALA_EXT = ".scala"
_JVM_EXTENSIONS = {_JAVA_EXT, ".kt", ".kts", _SCALA_EXT}


@dataclass(frozen=True)
class LangWeights:
    call: float
    symbol_ref: float
    type_ref: float
    lexical_min: float
    lexical_max: float


_LANG_WEIGHTS: dict[str, LangWeights] = {
    "python": LangWeights(0.55, 0.60, 0.50, 0.20, 0.35),
    "javascript": LangWeights(0.50, 0.55, 0.45, 0.25, 0.35),
    "typescript": LangWeights(0.70, 0.75, 0.65, 0.15, 0.25),
    "rust": LangWeights(0.90, 0.95, 0.85, 0.10, 0.15),
    "java": LangWeights(0.85, 0.90, 0.80, 0.10, 0.15),
    "kotlin": LangWeights(0.80, 0.85, 0.75, 0.12, 0.18),
    "scala": LangWeights(0.80, 0.85, 0.75, 0.12, 0.18),
    "go": LangWeights(0.80, 0.85, 0.75, 0.12, 0.20),
}

_DEFAULT_LANG_WEIGHTS = LangWeights(0.55, 0.60, 0.50, 0.15, 0.25)


_SUFFIX_TO_LANG: dict[str, str] = {
    ".py": "python",
    ".js": "javascript",
    ".jsx": "javascript",
    ".ts": "typescript",
    ".tsx": "typescript",
    ".rs": "rust",
    ".go": "go",
    _JAVA_EXT: "java",
    ".kt": "kotlin",
    ".kts": "kotlin",
    _SCALA_EXT: "scala",
}


def _get_lang_weights(path: Path) -> LangWeights:
    suffix = path.suffix.lower()
    lang = _SUFFIX_TO_LANG.get(suffix)
    return _LANG_WEIGHTS.get(lang, _DEFAULT_LANG_WEIGHTS) if lang else _DEFAULT_LANG_WEIGHTS


_DOC_STRUCTURE_WEIGHT = 0.30
_DOC_STRUCTURE_REVERSE_WEIGHT = 0.25
_ANCHOR_LINK_WEIGHT = 0.55
_ANCHOR_LINK_REVERSE_WEIGHT = 0.35
_CITATION_WEIGHT = 0.25

_CITATION_RE = re.compile(r"\[@([a-zA-Z0-9_:-]+)\]")
_MD_INTERNAL_LINK_RE = re.compile(r"\[([^\]]+)\]\(#([^)]+)\)")
_MD_HEADING_RE = re.compile(r"^#{1,6}\s+([^\n]+)$", re.MULTILINE)  # NOSONAR(S5852)


def _slugify(text: str) -> str:
    text = text.lower()
    text = re.sub(r"[^\w\s-]", "", text)
    text = re.sub(r"[\s_]+", "-", text)
    return text.strip("-")


@dataclass
class Graph:
    adjacency: dict[FragmentId, dict[FragmentId, float]] = field(default_factory=dict)
    nodes: set[FragmentId] = field(default_factory=set)

    def add_node(self, node: FragmentId) -> None:
        self.nodes.add(node)

    def add_edge(self, src: FragmentId, dst: FragmentId, weight: float) -> None:
        if math.isnan(weight) or math.isinf(weight) or weight <= 0:
            return
        if src not in self.adjacency:
            self.adjacency[src] = {}
        existing = self.adjacency[src].get(dst, 0.0)
        self.adjacency[src][dst] = max(existing, weight)
        self.nodes.add(src)
        self.nodes.add(dst)

    def neighbors(self, node: FragmentId) -> dict[FragmentId, float]:
        return self.adjacency.get(node, {})


def build_graph(fragments: list[Fragment], repo_root: Path | None = None) -> Graph:
    graph = Graph()

    for frag in fragments:
        graph.nodes.add(frag.id)

    # Collect all edges from various sources
    all_edges: dict[tuple[FragmentId, FragmentId], float] = {}

    for edge_dict in [
        _build_containment_edges(fragments),
        _build_import_edges(fragments),
        _build_test_edges(fragments),
        _build_document_structure_edges(fragments),
        _build_anchor_link_edges(fragments),
        _build_citation_edges_sparse(fragments),
        _build_config_code_edges(fragments),
        _build_package_sibling_edges(fragments),
        _build_lexical_edges_sparse(fragments),
        _build_embedding_edges(fragments, _clamp_lexical_weight),
        _build_cochange_edges(fragments, repo_root),
        collect_all_edges(fragments, repo_root),
    ]:
        for (src, dst), weight in edge_dict.items():
            all_edges[(src, dst)] = max(all_edges.get((src, dst), 0.0), weight)

    # Apply hub suppression to ALL edges
    all_edges = _apply_hub_suppression(all_edges)

    for (src, dst), weight in all_edges.items():
        graph.add_edge(src, dst, weight)

    return graph


def _build_containment_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    by_path: dict[Path, list[Fragment]] = defaultdict(list)
    for f in fragments:
        by_path[f.path].append(f)

    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    for _path, frags in by_path.items():
        if len(frags) >= 2:
            _process_file_containment(frags, edges)

    return edges


def _process_file_containment(frags: list[Fragment], edges: dict[tuple[FragmentId, FragmentId], float]) -> None:
    frags_sorted = sorted(frags, key=lambda x: (x.start_line, -x.end_line))
    stack: list[Fragment] = []

    for f in frags_sorted:
        while stack and f.start_line > stack[-1].end_line:
            stack.pop()

        if stack:
            _add_containment_edge(f, stack[-1], edges)

        stack.append(f)


def _add_containment_edge(child: Fragment, parent: Fragment, edges: dict[tuple[FragmentId, FragmentId], float]) -> None:
    if parent.start_line <= child.start_line and child.end_line <= parent.end_line and parent.id != child.id:
        edges[(child.id, parent.id)] = max(edges.get((child.id, parent.id), 0.0), _CONTAINMENT_WEIGHT)
        edges[(parent.id, child.id)] = max(edges.get((parent.id, child.id), 0.0), _CONTAINMENT_REVERSE_WEIGHT)


def _build_import_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    if not fragments:
        return {}

    module_to_frags = _build_module_index(fragments)
    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    for frag in fragments:
        _add_import_edges_for_fragment(frag, module_to_frags, edges)

    return edges


def _build_module_index(fragments: list[Fragment]) -> dict[str, list[FragmentId]]:
    module_to_frags: dict[str, list[FragmentId]] = defaultdict(list)

    for frag in fragments:
        module_name = path_to_module(frag.path)
        if module_name:
            module_to_frags[module_name].append(frag.id)
            parts = module_name.split(".")
            for i in range(1, len(parts)):
                partial = ".".join(parts[:i])
                module_to_frags[partial].append(frag.id)

    return module_to_frags


def _add_import_edges_for_fragment(
    frag: Fragment, module_to_frags: dict[str, list[FragmentId]], edges: dict[tuple[FragmentId, FragmentId], float]
) -> None:
    for match in _IMPORT_RE.finditer(frag.content):
        imported = match.group(1) or match.group(2)
        if not imported:
            continue

        target_frags = _resolve_import(imported, module_to_frags, frag.path)
        for target_id in target_frags:
            if target_id != frag.id:
                edges[(frag.id, target_id)] = _IMPORT_WEIGHT
                edges[(target_id, frag.id)] = _IMPORT_REVERSE_WEIGHT


def _resolve_relative_import(imported: str, source_path: Path, module_to_frags: dict[str, list[FragmentId]]) -> list[FragmentId]:
    base_module = path_to_module(source_path.parent)
    if not base_module:
        return []

    dots = len(imported) - len(imported.lstrip("."))
    relative_part = imported[dots:]
    parts = base_module.split(".")

    if dots > len(parts):
        return []

    resolved = ".".join(parts[:-dots]) if dots > 0 else base_module
    if relative_part:
        resolved = f"{resolved}.{relative_part}" if resolved else relative_part

    return module_to_frags.get(resolved, [])


def _resolve_partial_import(imported: str, module_to_frags: dict[str, list[FragmentId]]) -> list[FragmentId]:
    parts = imported.split(".")
    for i in range(len(parts), 0, -1):
        partial = ".".join(parts[:i])
        if partial in module_to_frags:
            return module_to_frags[partial]
    return []


def _fuzzy_match_module(imported: str, module_to_frags: dict[str, list[FragmentId]]) -> list[FragmentId]:
    base_name = imported.split(".")[-1]
    matches: list[FragmentId] = []
    for module_name, frags in module_to_frags.items():
        if module_name.endswith(base_name) or module_name.endswith(f".{base_name}"):
            matches.extend(frags)
    return matches


def _resolve_import(
    imported: str,
    module_to_frags: dict[str, list[FragmentId]],
    source_path: Path | None = None,
) -> list[FragmentId]:
    if imported.startswith(".") and source_path:
        result = _resolve_relative_import(imported, source_path, module_to_frags)
        if result:
            return result

    if imported in module_to_frags:
        return module_to_frags[imported]

    result = _resolve_partial_import(imported, module_to_frags)
    if result:
        return result

    return _fuzzy_match_module(imported, module_to_frags)


def _is_python_test(name: str) -> bool:
    return name.startswith("test_") or name.endswith("_test.py")


def _is_js_test(name: str, path_str: str) -> bool:
    return ".test." in name or ".spec." in name or "__tests__" in path_str


def _is_rust_test(name: str, path_str: str) -> bool:
    return "/tests/" in path_str or name == "tests.rs"


def _is_jvm_test(name: str) -> bool:
    stem = name.rsplit(".", 1)[0] if "." in name else name
    return stem.endswith("test") or stem.startswith("test")


# Test detection by extension. Python/Java/Kotlin use filename patterns only,
# while JS/TS/Rust also check directory structure (__tests__, /tests/).
_TEST_DETECTORS: dict[str, Callable[[str, str], bool]] = {
    ".py": lambda name, _path_str: _is_python_test(name),
    ".js": _is_js_test,
    ".ts": _is_js_test,
    ".jsx": _is_js_test,
    ".tsx": _is_js_test,
    ".rs": _is_rust_test,
    _JAVA_EXT: lambda name, _path_str: _is_jvm_test(name),
    ".kt": lambda name, _path_str: _is_jvm_test(name),
    ".kts": lambda name, _path_str: _is_jvm_test(name),
    _SCALA_EXT: lambda name, _path_str: _is_jvm_test(name),
}


def _is_test_file(path: Path) -> bool:
    name = path.name.lower()
    path_str = str(path).lower()
    suffix = path.suffix.lower()

    detector = _TEST_DETECTORS.get(suffix)
    if detector is not None and detector(name, path_str):
        return True

    return "/tests/" in path_str or "/test/" in path_str


def _has_direct_import(test_frag: Fragment, src_frag: Fragment) -> bool:
    src_module = path_to_module(src_frag.path)
    if not src_module:
        return False
    for match in _IMPORT_RE.finditer(test_frag.content):
        imported = match.group(1) or match.group(2)
        if imported and (imported == src_module or imported.endswith(f".{src_module}")):
            return True
    return False


def _extract_target_name_from_test(test_name: str) -> str | None:
    if test_name.startswith("test_"):
        return test_name[5:]
    if test_name.endswith("_test"):
        return test_name[:-5]
    if ".test" in test_name:
        return test_name.split(".test")[0]
    if ".spec" in test_name:
        return test_name.split(".spec")[0]
    return None


def _add_test_source_edge(
    edges: dict[tuple[FragmentId, FragmentId], float],
    test_frag: Fragment,
    src_frag: Fragment,
) -> None:
    weight = _TEST_WEIGHT_DIRECT if _has_direct_import(test_frag, src_frag) else _TEST_WEIGHT_NAMING
    edges[(test_frag.id, src_frag.id)] = weight
    edges[(src_frag.id, test_frag.id)] = _TEST_REVERSE_WEIGHT


def _build_test_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    by_base: dict[str, list[Fragment]] = defaultdict(list)
    test_frags: list[Fragment] = []

    for f in fragments:
        if _is_test_file(f.path):
            test_frags.append(f)
        else:
            by_base[f.path.stem.lower()].append(f)

    for test_frag in test_frags:
        target_name = _extract_target_name_from_test(test_frag.path.stem.lower())
        if not target_name:
            continue

        for src_frag in by_base.get(target_name, []):
            _add_test_source_edge(edges, test_frag, src_frag)

    return edges


def _build_document_structure_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    by_path: dict[Path, list[Fragment]] = defaultdict(list)
    for f in fragments:
        if f.kind in ("section", "paragraph"):
            by_path[f.path].append(f)

    for _path, frags in by_path.items():
        frags_sorted = sorted(frags, key=lambda x: x.start_line)

        for i in range(len(frags_sorted) - 1):
            curr, next_f = frags_sorted[i], frags_sorted[i + 1]
            edges[(curr.id, next_f.id)] = _DOC_STRUCTURE_WEIGHT
            edges[(next_f.id, curr.id)] = _DOC_STRUCTURE_REVERSE_WEIGHT

    return edges


def _build_anchor_link_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    anchor_index = _build_anchor_index(fragments)
    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    for f in fragments:
        _add_anchor_link_edges_for_fragment(f, anchor_index, edges)

    return edges


def _build_anchor_index(fragments: list[Fragment]) -> dict[str, FragmentId]:
    anchor_index: dict[str, FragmentId] = {}
    for f in fragments:
        if f.kind == "section":
            first_line = f.content.split("\n")[0]
            heading = re.sub(r"^#+\s*", "", first_line).strip()
            slug = _slugify(heading)
            if slug:
                anchor_index[slug] = f.id
    return anchor_index


def _add_anchor_link_edges_for_fragment(
    f: Fragment, anchor_index: dict[str, FragmentId], edges: dict[tuple[FragmentId, FragmentId], float]
) -> None:
    for match in _MD_INTERNAL_LINK_RE.finditer(f.content):
        target_slug = _slugify(match.group(2))
        target_id = anchor_index.get(target_slug)
        if target_id and target_id != f.id:
            edges[(f.id, target_id)] = _ANCHOR_LINK_WEIGHT
            edges[(target_id, f.id)] = _ANCHOR_LINK_REVERSE_WEIGHT


def _build_citation_edges_sparse(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    citation_to_frags: dict[str, list[FragmentId]] = defaultdict(list)

    for f in fragments:
        for cit in _CITATION_RE.findall(f.content):
            citation_to_frags[cit].append(f.id)

    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    for _cit, frag_ids in citation_to_frags.items():
        if len(frag_ids) < 2:
            continue
        hub = frag_ids[0]
        for other in frag_ids[1:]:
            edges[(hub, other)] = _CITATION_WEIGHT
            edges[(other, hub)] = _CITATION_WEIGHT

    return edges


def _apply_hub_suppression(
    edges: dict[tuple[FragmentId, FragmentId], float],
) -> dict[tuple[FragmentId, FragmentId], float]:
    if not edges:
        return edges

    in_degree: dict[FragmentId, int] = defaultdict(int)
    for src, dst in edges.keys():
        in_degree[dst] += 1

    if not in_degree:
        return edges

    sorted_degrees = sorted(in_degree.values())
    threshold_idx = int(len(sorted_degrees) * _HUB_PERCENTILE)
    threshold = sorted_degrees[min(threshold_idx, len(sorted_degrees) - 1)]

    suppressed: dict[tuple[FragmentId, FragmentId], float] = {}
    for (src, dst), weight in edges.items():
        if in_degree[dst] > threshold:
            weight = weight / math.log(1 + in_degree[dst])
        suppressed[(src, dst)] = weight

    return suppressed


def _clamp_lexical_weight(raw_sim: float, src_path: Path | None = None, dst_path: Path | None = None) -> float:
    if src_path and dst_path:
        src_weights = _get_lang_weights(src_path)
        dst_weights = _get_lang_weights(dst_path)
        lex_max = max(src_weights.lexical_max, dst_weights.lexical_max)
        lex_min = max(src_weights.lexical_min, dst_weights.lexical_min)
    else:
        lex_max = _LEXICAL_WEIGHT_MAX
        lex_min = _LEXICAL_WEIGHT_MIN

    if raw_sim < _MIN_SIMILARITY:
        return 0.0
    normalized = (raw_sim - _MIN_SIMILARITY) / (1.0 - _MIN_SIMILARITY)
    return lex_min + normalized * (lex_max - lex_min)


def _compute_doc_frequencies(fragments: list[Fragment]) -> dict[str, int]:
    doc_freq: dict[str, int] = defaultdict(int)
    for frag in fragments:
        seen_in_doc: set[str] = set()
        profile = TokenProfile.from_path(str(frag.path))
        idents = filter_idents(extract_identifier_list(frag.content, profile=profile), min_len=3, profile=profile)
        for ident in idents:
            if ident not in seen_in_doc:
                doc_freq[ident] += 1
                seen_in_doc.add(ident)
    return doc_freq


def _compute_idf_scores(doc_freq: dict[str, int], n_docs: int) -> dict[str, float]:
    return {term: math.log((n_docs + 1) / (df + 1)) + 1 for term, df in doc_freq.items()}


def _build_tf_idf_vector(frag: Fragment, doc_freq: dict[str, int], idf: dict[str, float], max_df: int) -> dict[str, float]:
    tf: dict[str, int] = defaultdict(int)
    profile = TokenProfile.from_path(str(frag.path))
    idents = filter_idents(extract_identifier_list(frag.content, profile=profile), min_len=3, profile=profile)
    for ident in idents:
        tf[ident] += 1

    vec: dict[str, float] = {}
    for term, count in tf.items():
        df = doc_freq.get(term, 0)
        if df <= 0 or df > max_df:
            continue
        term_idf = idf.get(term, 1.0)
        if term_idf < _MIN_IDF:
            continue
        vec[term] = count * term_idf

    norm = math.sqrt(sum(v * v for v in vec.values())) if vec else 0.0
    if norm > 0:
        for term in vec:
            vec[term] /= norm

    return vec


def _build_postings_index(tf_idf_vectors: dict[FragmentId, dict[str, float]]) -> dict[str, list[tuple[FragmentId, float]]]:
    postings: dict[str, list[tuple[FragmentId, float]]] = defaultdict(list)
    for frag_id, vec in tf_idf_vectors.items():
        for term, weight in vec.items():
            postings[term].append((frag_id, weight))
    return postings


def _compute_dot_products(postings: dict[str, list[tuple[FragmentId, float]]]) -> dict[tuple[FragmentId, FragmentId], float]:
    dot_products: dict[tuple[FragmentId, FragmentId], float] = defaultdict(float)

    for term, posting_list in postings.items():
        if len(posting_list) > _MAX_POSTINGS:
            continue
        for i, (frag_i, weight_i) in enumerate(posting_list):
            for frag_j, weight_j in posting_list[i + 1 :]:
                pair = (frag_i, frag_j) if str(frag_i) < str(frag_j) else (frag_j, frag_i)
                dot_products[pair] += weight_i * weight_j

    return dot_products


def _collect_neighbors(
    dot_products: dict[tuple[FragmentId, FragmentId], float], id_to_path: dict[FragmentId, Path]
) -> dict[FragmentId, list[tuple[float, FragmentId]]]:
    neighbors_by_node: dict[FragmentId, list[tuple[float, FragmentId]]] = defaultdict(list)

    for (src, dst), sim in dot_products.items():
        if sim < _MIN_SIMILARITY:
            continue
        src_path = id_to_path.get(src)
        dst_path = id_to_path.get(dst)
        clamped_forward = _clamp_lexical_weight(sim, src_path, dst_path)
        clamped_backward = _clamp_lexical_weight(sim, dst_path, src_path) * _BACKWARD_WEIGHT_FACTOR
        neighbors_by_node[src].append((clamped_forward, dst))
        neighbors_by_node[dst].append((clamped_backward, src))

    return neighbors_by_node


def _build_lexical_edges_sparse(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    if not fragments:
        return {}

    doc_freq = _compute_doc_frequencies(fragments)
    n_docs = len(fragments)
    max_df = max(1, int(n_docs * _MAX_DF_RATIO))
    idf = _compute_idf_scores(doc_freq, n_docs)

    tf_idf_vectors = {frag.id: _build_tf_idf_vector(frag, doc_freq, idf, max_df) for frag in fragments}
    postings = _build_postings_index(tf_idf_vectors)
    dot_products = _compute_dot_products(postings)

    id_to_path = {frag.id: frag.path for frag in fragments}
    neighbors_by_node = _collect_neighbors(dot_products, id_to_path)

    edges: dict[tuple[FragmentId, FragmentId], float] = {}
    for node, candidates in neighbors_by_node.items():
        top_k = nlargest(_TOP_K_NEIGHBORS, candidates, key=lambda x: x[0])
        for weight, neighbor in top_k:
            edges[(node, neighbor)] = weight

    return edges


_JSON_KEY_RE = re.compile(r'"([a-zA-Z_][a-zA-Z0-9_-]*)"\s*:')
_INI_KEY_RE = re.compile(r"^\s*([a-zA-Z_][a-zA-Z0-9_-]*)\s*=", re.MULTILINE)
_ENV_KEY_RE = re.compile(r"^([A-Za-z_]\w*)\s*=", re.MULTILINE)

_CONFIG_EXTS = CONFIG_EXTENSIONS | {".ini", ".env"}

_CONFIG_KEY_PATTERNS: dict[str, list[re.Pattern[str]]] = {
    ".yaml": [_CONFIG_KEY_RE],
    ".yml": [_CONFIG_KEY_RE],
    ".json": [_JSON_KEY_RE],
    ".toml": [_TOML_KEY_RE, _TOML_SECTION_RE],
    ".ini": [_INI_KEY_RE],
    ".env": [_ENV_KEY_RE],
}


def _extract_config_keys(suffix: str, content: str) -> set[str]:
    patterns = _CONFIG_KEY_PATTERNS.get(suffix, [])
    keys: set[str] = set()
    for pattern in patterns:
        for match in pattern.finditer(content):
            keys.add(match.group(1).lower())
    return keys


def _build_key_to_code_index(
    code_frags: list[Fragment],
    all_keys: set[str],
) -> dict[str, list[FragmentId]]:
    key_patterns = {key: re.compile(rf"\b{re.escape(key)}\b", re.IGNORECASE) for key in all_keys}
    key_to_code_frags: dict[str, list[FragmentId]] = defaultdict(list)

    for code_frag in code_frags:
        for key in all_keys:
            if key_patterns[key].search(code_frag.content):
                key_to_code_frags[key].append(code_frag.id)

    return key_to_code_frags


def _add_config_code_edge(
    edges: dict[tuple[FragmentId, FragmentId], float],
    cfg_id: FragmentId,
    code_id: FragmentId,
    weight: float,
) -> None:
    edges[(cfg_id, code_id)] = max(edges.get((cfg_id, code_id), 0.0), weight)
    edges[(code_id, cfg_id)] = max(edges.get((code_id, cfg_id), 0.0), weight * _BACKWARD_WEIGHT_FACTOR)


def _build_config_code_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    config_frags = [f for f in fragments if f.path.suffix.lower() in _CONFIG_EXTS]
    code_frags = [f for f in fragments if f.path.suffix.lower() in CODE_EXTENSIONS]

    if not config_frags or not code_frags:
        return {}

    config_keys_by_frag = {cfg.id: _extract_config_keys(cfg.path.suffix.lower(), cfg.content) for cfg in config_frags}
    all_keys = {k for keys in config_keys_by_frag.values() for k in keys}
    key_to_code_frags = _build_key_to_code_index(code_frags, all_keys)

    edges: dict[tuple[FragmentId, FragmentId], float] = {}
    for cfg in config_frags:
        for key in config_keys_by_frag.get(cfg.id, set()):
            matching = key_to_code_frags.get(key, [])
            if not matching:
                continue
            adjusted_weight = _CONFIG_CODE_WEIGHT * min(1.0, 3.0 / len(matching))
            for code_id in matching:
                _add_config_code_edge(edges, cfg.id, code_id, adjusted_weight)

    return edges


_MAX_SIBLING_FILES_PER_DIR = 20


def _build_package_sibling_edges(fragments: list[Fragment]) -> dict[tuple[FragmentId, FragmentId], float]:
    by_dir = _group_files_by_dir(fragments)
    file_to_rep = _build_file_representative_map(fragments)
    edges: dict[tuple[FragmentId, FragmentId], float] = {}

    for _dir_path, files in by_dir.items():
        _add_sibling_edges_for_dir(files, file_to_rep, edges)

    return edges


def _group_files_by_dir(fragments: list[Fragment]) -> dict[Path, set[Path]]:
    by_dir: dict[Path, set[Path]] = defaultdict(set)
    for f in fragments:
        by_dir[f.path.parent].add(f.path)
    return by_dir


def _build_file_representative_map(fragments: list[Fragment]) -> dict[Path, FragmentId]:
    file_to_rep: dict[Path, FragmentId] = {}
    for f in fragments:
        _update_file_representative(f, file_to_rep, fragments)
    return file_to_rep


def _update_file_representative(f: Fragment, file_to_rep: dict[Path, FragmentId], fragments: list[Fragment]) -> None:
    if f.path not in file_to_rep:
        file_to_rep[f.path] = f.id
    elif f.token_count > 0:
        existing = file_to_rep[f.path]
        existing_frag = next((fr for fr in fragments if fr.id == existing), None)
        if existing_frag and f.token_count > existing_frag.token_count:
            file_to_rep[f.path] = f.id


def _add_sibling_edges_for_dir(
    files: set[Path], file_to_rep: dict[Path, FragmentId], edges: dict[tuple[FragmentId, FragmentId], float]
) -> None:
    file_list = sorted(files)
    if len(file_list) > _MAX_SIBLING_FILES_PER_DIR:
        file_list = file_list[:_MAX_SIBLING_FILES_PER_DIR]

    if len(file_list) < 2:
        return

    for i, f1_path in enumerate(file_list):
        for f2_path in file_list[i + 1 :]:
            f1_id = file_to_rep.get(f1_path)
            f2_id = file_to_rep.get(f2_path)
            if f1_id and f2_id:
                edges[(f1_id, f2_id)] = _SIBLING_WEIGHT
                edges[(f2_id, f1_id)] = _SIBLING_WEIGHT


def _get_git_log_files(repo_root: Path) -> list[list[str]] | None:
    try:
        result = subprocess.run(
            ["git", "-C", str(repo_root), "log", "--name-only", "--format=", f"-n{_COCHANGE_COMMITS}"],
            capture_output=True,
            text=True,
            check=True,
            timeout=_COCHANGE_TIMEOUT,
        )
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, FileNotFoundError):
        return None
    return [c.strip().split("\n") for c in result.stdout.split("\n\n") if c.strip()]


def _count_cochanges(commits: list[list[str]]) -> Counter[tuple[str, str]]:
    cochange: Counter[tuple[str, str]] = Counter()
    for files in commits:
        files = [f for f in files if f]
        if len(files) > _COCHANGE_MAX_FILES_PER_COMMIT:
            continue
        for i, f1 in enumerate(files):
            for f2 in files[i + 1 :]:
                pair = (f1, f2) if f1 < f2 else (f2, f1)
                cochange[pair] += 1
    return cochange


def _build_path_to_frags_index(fragments: list[Fragment], repo_root: Path) -> dict[str, list[FragmentId]]:
    path_to_frags: dict[str, list[FragmentId]] = defaultdict(list)
    for f in fragments:
        try:
            rel = f.path.relative_to(repo_root).as_posix() if f.path.is_absolute() else f.path.as_posix()
            path_to_frags[rel].append(f.id)
        except ValueError:
            continue
    return path_to_frags


def _build_cochange_edges(fragments: list[Fragment], repo_root: Path | None) -> dict[tuple[FragmentId, FragmentId], float]:
    if repo_root is None:
        return {}

    commits = _get_git_log_files(repo_root)
    if commits is None:
        return {}

    cochange = _count_cochanges(commits)
    path_to_frags = _build_path_to_frags_index(fragments, repo_root)

    edges: dict[tuple[FragmentId, FragmentId], float] = {}
    for (p1, p2), count in cochange.items():
        if count < _COCHANGE_MIN_COUNT:
            continue
        weight = min(_COCHANGE_WEIGHT, 0.1 * math.log(1 + count))
        for fid1 in path_to_frags.get(p1, []):
            for fid2 in path_to_frags.get(p2, []):
                edges[(fid1, fid2)] = max(edges.get((fid1, fid2), 0.0), weight)
                edges[(fid2, fid1)] = max(edges.get((fid2, fid1), 0.0), weight)

    return edges
