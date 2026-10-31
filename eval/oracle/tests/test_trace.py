import importlib.util
import json
import sys
from pathlib import Path

TRACE = Path(__file__).resolve().parent.parent / "trace"


def load(name: str):
    spec = importlib.util.spec_from_file_location(name, TRACE / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def test_profile_records_edges_between_repository_functions_only(tmp_path: Path):
    profile = load("python_profile")
    package = tmp_path / "pkg"
    package.mkdir()
    (package / "__init__.py").write_text("")
    (package / "mod.py").write_text(
        "def leaf():\n    return 1\n\n\ndef middle():\n    def inner():\n        return leaf()\n    return inner() + sorted([2])[0]\n"
    )
    sys.path.insert(0, str(tmp_path))
    try:
        import pkg.mod

        recorder = profile.record(tmp_path, pkg.mod.middle)
    finally:
        sys.path.remove(str(tmp_path))
        sys.modules.pop("pkg.mod", None)
        sys.modules.pop("pkg", None)

    edges = {(r["from"]["qualname"], r["to"]["qualname"]) for r in recorder.rows()}
    assert edges == {("middle", "middle.inner"), ("middle.inner", "leaf")}
    assert all(r["from"]["path"] == "pkg/mod.py" for r in recorder.rows())


def test_coverage_dumps_become_function_rows_with_utf16_aware_lines(tmp_path: Path):
    coverage = load("ts_coverage")
    root = tmp_path / "repo"
    (root / "src").mkdir(parents=True)
    source = "// 🚀\nfunction used() {}\nfunction unused() {}\n"
    (root / "src" / "a.js").write_text(source, encoding="utf-8")
    used_at = len("// 🚀\n".encode("utf-16-le")) // 2
    unused_at = used_at + len("function used() {}\n")
    dump = {
        "result": [
            {
                "url": (root / "src" / "a.js").resolve().as_uri(),
                "functions": [
                    {"functionName": "used", "ranges": [{"startOffset": used_at, "endOffset": used_at + 5, "count": 3}]},
                    {"functionName": "unused", "ranges": [{"startOffset": unused_at, "endOffset": unused_at + 5, "count": 0}]},
                    {"functionName": "", "ranges": [{"startOffset": 0, "endOffset": 1, "count": 1}]},
                ],
            },
            {
                "url": "node:internal/x",
                "functions": [{"functionName": "internal", "ranges": [{"startOffset": 0, "endOffset": 1, "count": 1}]}],
            },
        ]
    }
    scratch = tmp_path / "v8"
    scratch.mkdir()
    (scratch / "coverage-1.json").write_text(json.dumps(dump))

    rows = coverage.read_coverage(scratch, root)

    assert rows == [
        {"path": "src/a.js", "name": "used", "line": 2, "count": 3},
        {"path": "src/a.js", "name": "unused", "line": 3, "count": 0},
    ]
