from __future__ import annotations

from pathlib import Path
from typing import Any

from tests.framework.pygit2_backend import Pygit2Repo
from treemapper.diffctx import build_diff_context


def _extract_content(context: dict[str, Any]) -> str:
    parts = []
    for frag in context.get("fragments", []):
        if "content" in frag:
            parts.append(frag["content"])
        if "path" in frag:
            parts.append(frag["path"])
    return "\n".join(parts)


def _extract_paths(context: dict[str, Any]) -> set[str]:
    return {frag["path"] for frag in context.get("fragments", []) if "path" in frag}


class TestBinaryFileExclusion:
    def test_png_file_excluded_from_diffctx(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "binary_repo")

        g.add_file("app.py", "def main():\n    return 'v1'\n")
        g.add_file_binary("logo.png", b"\x89PNG\r\n\x1a\n" + b"\x00" * 100 + bytes(range(256)))
        g.commit("init")

        g.add_file("app.py", "def main():\n    return 'v2'\n")
        g.add_file_binary("logo.png", b"\x89PNG\r\n\x1a\n" + b"\xff" * 100 + bytes(reversed(range(256))))
        g.commit("update app and logo")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "main" in all_content, "Changed Python file must be included"
        assert "\x89PNG" not in all_content, "Binary PNG content must not appear in context"

    def test_jar_file_excluded_from_diffctx(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "jar_repo")

        g.add_file("Main.java", "public class Main {\n    public static void main(String[] args) {}\n}\n")
        g.add_file_binary("lib.jar", b"PK\x03\x04" + b"\x00" * 200)
        g.commit("init")

        g.add_file(
            "Main.java",
            'public class Main {\n    public static void main(String[] args) {\n        System.out.println("hello");\n    }\n}\n',
        )
        g.add_file_binary("lib.jar", b"PK\x03\x04" + b"\x01" * 200)
        g.commit("update")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "Main" in all_content, "Changed Java file must be included"
        assert "PK" not in all_content, "Binary JAR content must not appear"

    def test_keystore_file_excluded_from_diffctx(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "keystore_repo")

        g.add_file("build.gradle", "apply plugin: 'java'\n")
        g.add_file_binary("release.keystore", b"\x00\x01\x02\x03" * 50)
        g.commit("init")

        g.add_file("build.gradle", "apply plugin: 'java'\napply plugin: 'kotlin'\n")
        g.add_file_binary("release.keystore", b"\x04\x05\x06\x07" * 50)
        g.commit("update")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert any("build.gradle" in p for p in paths), "Changed build file must be included"
        assert not any("keystore" in p for p in paths), "Keystore file must not appear in context"

    def test_binary_content_with_null_bytes_excluded(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "null_repo")

        g.add_file("code.py", "x = 1\n")
        g.add_file_binary("data.bin", b"HEADER\x00\x00" + bytes(range(128)))
        g.commit("init")

        g.add_file("code.py", "x = 2\n")
        g.add_file_binary("data.bin", b"HEADER\x00\x00" + bytes(reversed(range(128))))
        g.commit("update")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "x = 2" in all_content or "code.py" in all_content, "Changed code file must be included"
        assert "HEADER" not in all_content, "Binary content with null bytes must not appear"


class TestDeletedFileExclusion:
    def test_deleted_file_content_not_in_output(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "delete_repo")

        g.add_file("keep.py", "def keep():\n    return 'keep_marker_xyz'\n")
        g.add_file("remove.py", "def removed_function():\n    REMOVED_MARKER_12345 = True\n    return 'deleted_content_marker'\n")
        g.commit("init")

        g.add_file("keep.py", "def keep():\n    return 'keep_marker_xyz_v2'\n")
        g.remove_file("remove.py")
        g.commit("remove file and update keep")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "keep_marker_xyz" in all_content, "Modified file must be in context"
        assert "REMOVED_MARKER_12345" not in all_content, "Deleted file content must not appear in context"
        assert "deleted_content_marker" not in all_content, "Deleted file content must not appear in context"

    def test_multiple_deleted_files_excluded(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "multi_delete_repo")

        g.add_file("active.py", "def active():\n    return 'active_marker'\n")
        for i in range(3):
            g.add_file(f"old_module_{i}.py", f"def old_func_{i}():\n    OLD_DELETE_MARKER_{i} = True\n    return 'old_{i}'\n")
        g.commit("init")

        g.add_file("active.py", "def active():\n    return 'active_marker_v2'\n")
        for i in range(3):
            g.remove_file(f"old_module_{i}.py")
        g.commit("cleanup old modules")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "active_marker" in all_content, "Modified file must be in context"
        for i in range(3):
            assert f"OLD_DELETE_MARKER_{i}" not in all_content, f"Deleted file {i} content must not appear"


class TestRenameDetection:
    def test_pure_rename_excludes_old_path_content(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "rename_repo")

        g.add_file("old_name.py", "def my_function():\n    RENAME_MARKER_OLD = True\n    return 'original'\n")
        g.add_file("other.py", "def other():\n    return 'other_v1'\n")
        g.commit("init")

        (g.path / "old_name.py").rename(g.path / "new_name.py")
        g.add_file("other.py", "def other():\n    return 'other_v2'\n")
        g.commit("rename file and update other")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert any("other.py" in p for p in paths), "Modified file must be in context"
        old_path_fragments = [p for p in paths if "old_name" in p]
        assert len(old_path_fragments) == 0, f"Old renamed path must not appear in fragments: {old_path_fragments}"

    def test_rename_with_changes_uses_new_path(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "rename_change_repo")

        g.add_file("utils.py", "def helper():\n    return 'helper_v1'\n\ndef another():\n    return 'another'\n")
        g.commit("init")

        g.remove_file("utils.py")
        g.add_file("helpers.py", "def helper():\n    return 'helper_v2'\n\ndef another():\n    return 'another'\n")
        g.commit("rename and update")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert not any("utils.py" in p for p in paths), "Old path must not appear for rename with changes"

    def test_directory_rename_excludes_old_paths(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "dir_rename_repo")

        g.add_file("old_pkg/module_a.py", "def func_a():\n    DIR_RENAME_MARKER_A = True\n    return 'a'\n")
        g.add_file("old_pkg/module_b.py", "def func_b():\n    DIR_RENAME_MARKER_B = True\n    return 'b'\n")
        g.add_file("main.py", "def main():\n    return 'main_v1'\n")
        g.commit("init")

        (g.path / "old_pkg").rename(g.path / "new_pkg")
        g.add_file("main.py", "def main():\n    return 'main_v2'\n")
        g.commit("rename directory")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        old_paths = [p for p in paths if "old_pkg" in p]
        assert len(old_paths) == 0, f"Old directory paths must not appear: {old_paths}"

    def test_pure_rename_new_path_excluded(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "pure_rename_new")

        g.add_file(
            "original.py",
            "def process():\n    PURE_RENAME_NEW_MARKER = True\n    return 42\n",
        )
        g.add_file("other.py", "def other():\n    return 'v1'\n")
        g.commit("init")

        (g.path / "original.py").rename(g.path / "renamed.py")
        g.add_file("other.py", "def other():\n    return 'v2'\n")
        g.commit("pure rename + unrelated change")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)
        all_content = _extract_content(context)

        assert any("other.py" in p for p in paths), "Modified file must be in context"
        assert not any("renamed.py" in p for p in paths), "Pure-rename new path must not appear in context"
        assert "PURE_RENAME_NEW_MARKER" not in all_content, "Pure-rename content must not appear"

    def test_rename_with_content_changes_included(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "rename_modified")

        g.add_file(
            "original.py",
            "def process():\n    RENAME_MODIFIED_MARKER = True\n    return 1\n",
        )
        g.commit("init")

        g.add_file(
            "renamed_modified.py",
            "def process():\n    RENAME_MODIFIED_MARKER = True\n    return 2\n\ndef new_func():\n    pass\n",
        )
        g.remove_file("original.py")
        g.commit("rename with content changes")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert any("renamed_modified.py" in p for p in paths), "Rename with changes must appear in context"


class TestGeneratedApiFiles:
    def test_api_files_capped_as_generated(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "api_repo")

        g.add_file("app.kt", 'class App {\n    fun run() = println("hello")\n}\n')
        api_content_lines = [f"  fun apiMethod{i}(): String" for i in range(100)]
        g.add_file("public.api", "class GeneratedApi {\n" + "\n".join(api_content_lines) + "\n}\n")
        g.commit("init")

        g.add_file("app.kt", 'class App {\n    fun run() = println("hello v2")\n}\n')
        api_content_lines_v2 = [f"  fun apiMethod{i}(): String" for i in range(101)]
        g.add_file("public.api", "class GeneratedApi {\n" + "\n".join(api_content_lines_v2) + "\n}\n")
        g.commit("update")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=50000)
        all_content = _extract_content(context)

        assert "App" in all_content, "Changed Kotlin file must be in context"
        api_method_count = sum(1 for i in range(101) if f"apiMethod{i}" in all_content)
        assert api_method_count <= 35, f"API file should be truncated as generated, but found {api_method_count} methods"

    def test_pb_go_still_treated_as_generated(self, tmp_path: Path) -> None:
        g = Pygit2Repo(tmp_path / "proto_repo")

        g.add_file("service.go", 'package main\n\nfunc Serve() string {\n\treturn "v1"\n}\n')
        proto_lines = [f"func Method{i}() {{}}" for i in range(50)]
        g.add_file("service.pb.go", "package main\n\n" + "\n".join(proto_lines) + "\n")
        g.commit("init")

        g.add_file("service.go", 'package main\n\nfunc Serve() string {\n\treturn "v2"\n}\n')
        proto_lines_v2 = [f"func Method{i}() {{}}" for i in range(51)]
        g.add_file("service.pb.go", "package main\n\n" + "\n".join(proto_lines_v2) + "\n")
        g.commit("update")

        context = build_diff_context(root_dir=g.path, diff_range="HEAD~1..HEAD", budget_tokens=50000)
        all_content = _extract_content(context)

        assert "Serve" in all_content, "Changed Go file must be in context"
        method_count = sum(1 for i in range(51) if f"Method{i}" in all_content)
        assert method_count <= 10, f"Proto generated file should be capped, found {method_count} methods"
