from typing import Any

class GitError(Exception): ...

class PyProjectGraph:
    @property
    def fragment_count(self) -> int: ...
    @property
    def edge_count(self) -> int: ...

class PyScoredState: ...

class PyQuotientGraph:
    @property
    def node_count(self) -> int: ...
    @property
    def edge_count(self) -> int: ...

class PyModuleMetrics:
    @property
    def name(self) -> str: ...
    @property
    def cohesion(self) -> float: ...
    @property
    def coupling(self) -> float: ...
    @property
    def instability(self) -> float: ...
    @property
    def fan_in(self) -> int: ...
    @property
    def fan_out(self) -> int: ...

def build_diff_context(
    root_dir: str,
    diff_range: str,
    budget_tokens: int | None = ...,
    alpha: float = ...,
    tau: float = ...,
    no_content: bool = ...,
    ignore_file: str | None = ...,
    no_default_ignores: bool = ...,
    full: bool = ...,
    whitelist_file: str | None = ...,
    scoring_mode: str = ...,
    timeout: int = ...,
) -> dict[str, Any]: ...
def build_locate(
    root_dir: str,
    diff_range: str,
    budget_tokens: int | None = ...,
    alpha: float = ...,
    tau: float = ...,
    scoring_mode: str = ...,
    timeout: int = ...,
) -> str: ...
def compute_scored_state(
    root_dir: str,
    diff_range: str,
    alpha: float = ...,
    scoring_mode: str = ...,
    timeout: int = ...,
) -> PyScoredState: ...
def select_with_params(
    state: PyScoredState,
    budget_tokens: int | None = ...,
    tau: float = ...,
    no_content: bool = ...,
) -> dict[str, Any]: ...
def get_raw_diff_text(root_dir: str, diff_range: str, timeout: int = ...) -> str: ...
def resolve_diff_range(root_dir: str, diff_range: str) -> str: ...
def get_language_for_file(path: str) -> str | None: ...
def count_tokens(text: str) -> int: ...
def build_project_graph(root_dir: str) -> PyProjectGraph: ...
def detect_cycles(
    pg: PyProjectGraph,
    level: str = ...,
    edge_types: list[str] | None = ...,
) -> list[list[str]]: ...
def hotspots(
    pg: PyProjectGraph,
    top: int,
    edge_types: list[str] | None = ...,
) -> list[tuple[str, float, dict[str, Any]]]: ...
def coupling_metrics(
    pg: PyProjectGraph,
    level: str = ...,
    edge_types: list[str] | None = ...,
) -> list[PyModuleMetrics]: ...
def quotient_graph(
    pg: PyProjectGraph,
    level: str = ...,
    edge_types: list[str] | None = ...,
) -> PyQuotientGraph: ...
def to_mermaid(qg: PyQuotientGraph, top_n: int) -> str: ...
def graph_summary(pg: PyProjectGraph, top_n: int) -> dict[str, Any]: ...
def graph_to_json_string(pg: PyProjectGraph) -> str: ...
def graph_to_graphml_string(pg: PyProjectGraph) -> str: ...

# The shipped defaults. Exported so the CLI, the MCP server and the eval
# harnesses read them instead of each restating the number (#175).
DEFAULT_TAU: float
DEFAULT_ALPHA: float
DEFAULT_CORE_BUDGET_FRACTION: float
DEFAULT_SCORING: str
SCORING_MODES: list[str]
