from __future__ import annotations

from .ansible import AnsibleEdgeBuilder
from .bazel import BazelEdgeBuilder
from .c_family import CFamilyEdgeBuilder
from .cargo import CargoEdgeBuilder
from .clojure import ClojureEdgeBuilder
from .css import CssEdgeBuilder
from .dart import DartEdgeBuilder
from .dbt import DbtEdgeBuilder
from .dotnet import DotNetEdgeBuilder
from .elixir import ElixirEdgeBuilder
from .erlang import ErlangEdgeBuilder
from .go import GoEdgeBuilder
from .graphql import GraphqlEdgeBuilder
from .haskell import HaskellEdgeBuilder
from .javascript import JavaScriptEdgeBuilder
from .julia import JuliaEdgeBuilder
from .jvm import JVMEdgeBuilder
from .latex import LatexEdgeBuilder
from .lua import LuaEdgeBuilder
from .nim import NimEdgeBuilder
from .nix import NixEdgeBuilder
from .ocaml import OCamlEdgeBuilder
from .openapi import OpenapiEdgeBuilder
from .perl import PerlEdgeBuilder
from .php import PHPEdgeBuilder
from .prisma import PrismaEdgeBuilder
from .protobuf import ProtobufEdgeBuilder
from .python import PythonEdgeBuilder
from .r_lang import RLangEdgeBuilder
from .ruby import RubyEdgeBuilder
from .rust import RustEdgeBuilder
from .shell import ShellEdgeBuilder
from .sql import SqlEdgeBuilder
from .swift import SwiftEdgeBuilder
from .tags import TagsEdgeBuilder
from .zig import ZigEdgeBuilder


def get_semantic_builders() -> list[type]:
    return [
        TagsEdgeBuilder,
        AnsibleEdgeBuilder,
        BazelEdgeBuilder,
        CFamilyEdgeBuilder,
        CargoEdgeBuilder,
        ClojureEdgeBuilder,
        CssEdgeBuilder,
        DartEdgeBuilder,
        DbtEdgeBuilder,
        DotNetEdgeBuilder,
        ElixirEdgeBuilder,
        ErlangEdgeBuilder,
        GoEdgeBuilder,
        GraphqlEdgeBuilder,
        HaskellEdgeBuilder,
        JavaScriptEdgeBuilder,
        JuliaEdgeBuilder,
        JVMEdgeBuilder,
        LatexEdgeBuilder,
        LuaEdgeBuilder,
        NimEdgeBuilder,
        NixEdgeBuilder,
        OCamlEdgeBuilder,
        OpenapiEdgeBuilder,
        PerlEdgeBuilder,
        PHPEdgeBuilder,
        PrismaEdgeBuilder,
        ProtobufEdgeBuilder,
        PythonEdgeBuilder,
        RLangEdgeBuilder,
        RubyEdgeBuilder,
        RustEdgeBuilder,
        ShellEdgeBuilder,
        SqlEdgeBuilder,
        SwiftEdgeBuilder,
        ZigEdgeBuilder,
    ]


__all__ = [
    "AnsibleEdgeBuilder",
    "BazelEdgeBuilder",
    "CFamilyEdgeBuilder",
    "CargoEdgeBuilder",
    "ClojureEdgeBuilder",
    "CssEdgeBuilder",
    "DartEdgeBuilder",
    "DbtEdgeBuilder",
    "DotNetEdgeBuilder",
    "ElixirEdgeBuilder",
    "ErlangEdgeBuilder",
    "GoEdgeBuilder",
    "GraphqlEdgeBuilder",
    "HaskellEdgeBuilder",
    "JVMEdgeBuilder",
    "JavaScriptEdgeBuilder",
    "JuliaEdgeBuilder",
    "LatexEdgeBuilder",
    "LuaEdgeBuilder",
    "NimEdgeBuilder",
    "NixEdgeBuilder",
    "OCamlEdgeBuilder",
    "OpenapiEdgeBuilder",
    "PHPEdgeBuilder",
    "PerlEdgeBuilder",
    "PrismaEdgeBuilder",
    "ProtobufEdgeBuilder",
    "PythonEdgeBuilder",
    "RLangEdgeBuilder",
    "RubyEdgeBuilder",
    "RustEdgeBuilder",
    "ShellEdgeBuilder",
    "SqlEdgeBuilder",
    "SwiftEdgeBuilder",
    "ZigEdgeBuilder",
    "get_semantic_builders",
]
