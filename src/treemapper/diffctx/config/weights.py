from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class EdgeWeightConfig:
    forward: float
    reverse_factor: float = 0.7


EDGE_WEIGHTS = {
    "containment": EdgeWeightConfig(0.50, 0.70),
    "import": EdgeWeightConfig(0.50, 0.70),
    "test_direct": EdgeWeightConfig(0.60, 0.50),
    "test_naming": EdgeWeightConfig(0.50, 0.50),
    "test_reverse": EdgeWeightConfig(0.30, 1.0),
    "config_code": EdgeWeightConfig(0.45, 0.70),
    "sibling": EdgeWeightConfig(0.05, 1.0),
    "cochange": EdgeWeightConfig(0.40, 1.0),
    "doc_structure": EdgeWeightConfig(0.30, 0.83),
    "anchor_link": EdgeWeightConfig(0.55, 0.64),
    "citation": EdgeWeightConfig(0.25, 1.0),
    "python_call": EdgeWeightConfig(0.55, 0.70),
    "python_symbol": EdgeWeightConfig(0.60, 0.70),
    "python_type": EdgeWeightConfig(0.50, 0.70),
    "javascript_call": EdgeWeightConfig(0.70, 0.50),
    "javascript_symbol": EdgeWeightConfig(0.75, 0.50),
    "javascript_type": EdgeWeightConfig(0.65, 0.50),
    "go_import": EdgeWeightConfig(0.70, 0.40),
    "go_type": EdgeWeightConfig(0.65, 0.40),
    "go_func": EdgeWeightConfig(0.60, 0.40),
    "go_same_package": EdgeWeightConfig(0.55, 0.40),
    "rust_mod": EdgeWeightConfig(0.70, 0.40),
    "rust_use": EdgeWeightConfig(0.65, 0.40),
    "rust_type": EdgeWeightConfig(0.65, 0.40),
    "rust_fn": EdgeWeightConfig(0.60, 0.40),
    "rust_same_crate": EdgeWeightConfig(0.50, 0.40),
    "jvm_import": EdgeWeightConfig(0.70, 0.50),
    "jvm_type": EdgeWeightConfig(0.65, 0.50),
    "jvm_call": EdgeWeightConfig(0.60, 0.50),
    "c_include": EdgeWeightConfig(0.65, 0.50),
    "c_symbol": EdgeWeightConfig(0.55, 0.50),
    "dotnet_using": EdgeWeightConfig(0.65, 0.50),
    "dotnet_type": EdgeWeightConfig(0.60, 0.50),
    "ruby_require": EdgeWeightConfig(0.60, 0.50),
    "ruby_symbol": EdgeWeightConfig(0.55, 0.50),
    "php_use": EdgeWeightConfig(0.60, 0.50),
    "php_symbol": EdgeWeightConfig(0.55, 0.50),
    "shell_source": EdgeWeightConfig(0.55, 0.50),
    "swift_import": EdgeWeightConfig(0.65, 0.50),
    "swift_symbol": EdgeWeightConfig(0.60, 0.50),
}


@dataclass(frozen=True)
class LangWeights:
    call: float
    symbol_ref: float
    type_ref: float
    lexical_min: float
    lexical_max: float


LANG_WEIGHTS: dict[str, LangWeights] = {
    "python": LangWeights(0.55, 0.60, 0.50, 0.20, 0.35),
    "javascript": LangWeights(0.50, 0.55, 0.45, 0.25, 0.35),
    "typescript": LangWeights(0.70, 0.75, 0.65, 0.15, 0.25),
    "rust": LangWeights(0.90, 0.95, 0.85, 0.10, 0.15),
    "java": LangWeights(0.85, 0.90, 0.80, 0.10, 0.15),
    "kotlin": LangWeights(0.80, 0.85, 0.75, 0.12, 0.18),
    "scala": LangWeights(0.80, 0.85, 0.75, 0.12, 0.18),
    "go": LangWeights(0.80, 0.85, 0.75, 0.12, 0.20),
}

DEFAULT_LANG_WEIGHTS = LangWeights(0.55, 0.60, 0.50, 0.15, 0.25)
