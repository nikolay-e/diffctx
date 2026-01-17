from __future__ import annotations

from .c_family import CFamilyEdgeBuilder
from .dotnet import DotNetEdgeBuilder
from .go import GoEdgeBuilder
from .javascript import JavaScriptEdgeBuilder
from .jvm import JVMEdgeBuilder
from .php import PHPEdgeBuilder
from .python import PythonEdgeBuilder
from .ruby import RubyEdgeBuilder
from .rust import RustEdgeBuilder
from .shell import ShellEdgeBuilder
from .swift import SwiftEdgeBuilder


def get_semantic_builders() -> list[type]:
    return [
        CFamilyEdgeBuilder,
        GoEdgeBuilder,
        RustEdgeBuilder,
        JVMEdgeBuilder,
        DotNetEdgeBuilder,
        RubyEdgeBuilder,
        PHPEdgeBuilder,
        PythonEdgeBuilder,
        JavaScriptEdgeBuilder,
        ShellEdgeBuilder,
        SwiftEdgeBuilder,
    ]


__all__ = [
    "CFamilyEdgeBuilder",
    "DotNetEdgeBuilder",
    "GoEdgeBuilder",
    "JVMEdgeBuilder",
    "JavaScriptEdgeBuilder",
    "PHPEdgeBuilder",
    "PythonEdgeBuilder",
    "RubyEdgeBuilder",
    "RustEdgeBuilder",
    "ShellEdgeBuilder",
    "SwiftEdgeBuilder",
    "get_semantic_builders",
]
