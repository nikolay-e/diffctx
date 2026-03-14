from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

from treemapper.diffctx import build_diff_context


def _init_repo(path: Path) -> None:
    subprocess.run(["git", "init"], cwd=path, capture_output=True, check=True)
    subprocess.run(["git", "config", "user.email", "t@t.com"], cwd=path, capture_output=True, check=True)
    subprocess.run(["git", "config", "user.name", "T"], cwd=path, capture_output=True, check=True)


def _commit(repo: Path, message: str) -> str:
    subprocess.run(["git", "add", "-A"], cwd=repo, capture_output=True, check=True)
    subprocess.run(["git", "commit", "-m", message], cwd=repo, capture_output=True, check=True)
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=repo, capture_output=True, text=True, check=True)
    return result.stdout.strip()


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
        repo = tmp_path / "binary_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "app.py").write_text("def main():\n    return 'v1'\n", encoding="utf-8")
        (repo / "logo.png").write_bytes(b"\x89PNG\r\n\x1a\n" + b"\x00" * 100 + bytes(range(256)))
        _commit(repo, "init")

        (repo / "app.py").write_text("def main():\n    return 'v2'\n", encoding="utf-8")
        (repo / "logo.png").write_bytes(b"\x89PNG\r\n\x1a\n" + b"\xff" * 100 + bytes(reversed(range(256))))
        _commit(repo, "update app and logo")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "main" in all_content, "Changed Python file must be included"
        assert "\x89PNG" not in all_content, "Binary PNG content must not appear in context"

    def test_jar_file_excluded_from_diffctx(self, tmp_path: Path) -> None:
        repo = tmp_path / "jar_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "Main.java").write_text(
            "public class Main {\n    public static void main(String[] args) {}\n}\n", encoding="utf-8"
        )
        (repo / "lib.jar").write_bytes(b"PK\x03\x04" + b"\x00" * 200)
        _commit(repo, "init")

        (repo / "Main.java").write_text(
            'public class Main {\n    public static void main(String[] args) {\n        System.out.println("hello");\n    }\n}\n',
            encoding="utf-8",
        )
        (repo / "lib.jar").write_bytes(b"PK\x03\x04" + b"\x01" * 200)
        _commit(repo, "update")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "Main" in all_content, "Changed Java file must be included"
        assert "PK" not in all_content, "Binary JAR content must not appear"

    def test_keystore_file_excluded_from_diffctx(self, tmp_path: Path) -> None:
        repo = tmp_path / "keystore_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "build.gradle").write_text("apply plugin: 'java'\n", encoding="utf-8")
        (repo / "release.keystore").write_bytes(b"\x00\x01\x02\x03" * 50)
        _commit(repo, "init")

        (repo / "build.gradle").write_text("apply plugin: 'java'\napply plugin: 'kotlin'\n", encoding="utf-8")
        (repo / "release.keystore").write_bytes(b"\x04\x05\x06\x07" * 50)
        _commit(repo, "update")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert any("build.gradle" in p for p in paths), "Changed build file must be included"
        assert not any("keystore" in p for p in paths), "Keystore file must not appear in context"

    def test_binary_content_with_null_bytes_excluded(self, tmp_path: Path) -> None:
        repo = tmp_path / "null_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "code.py").write_text("x = 1\n", encoding="utf-8")
        (repo / "data.bin").write_bytes(b"HEADER\x00\x00" + bytes(range(128)))
        _commit(repo, "init")

        (repo / "code.py").write_text("x = 2\n", encoding="utf-8")
        (repo / "data.bin").write_bytes(b"HEADER\x00\x00" + bytes(reversed(range(128))))
        _commit(repo, "update")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "x = 2" in all_content or "code.py" in all_content, "Changed code file must be included"
        assert "HEADER" not in all_content, "Binary content with null bytes must not appear"


class TestDeletedFileExclusion:
    def test_deleted_file_content_not_in_output(self, tmp_path: Path) -> None:
        repo = tmp_path / "delete_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "keep.py").write_text("def keep():\n    return 'keep_marker_xyz'\n", encoding="utf-8")
        (repo / "remove.py").write_text(
            "def removed_function():\n    REMOVED_MARKER_12345 = True\n    return 'deleted_content_marker'\n",
            encoding="utf-8",
        )
        _commit(repo, "init")

        (repo / "keep.py").write_text("def keep():\n    return 'keep_marker_xyz_v2'\n", encoding="utf-8")
        (repo / "remove.py").unlink()
        _commit(repo, "remove file and update keep")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "keep_marker_xyz" in all_content, "Modified file must be in context"
        assert "REMOVED_MARKER_12345" not in all_content, "Deleted file content must not appear in context"
        assert "deleted_content_marker" not in all_content, "Deleted file content must not appear in context"

    def test_multiple_deleted_files_excluded(self, tmp_path: Path) -> None:
        repo = tmp_path / "multi_delete_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "active.py").write_text("def active():\n    return 'active_marker'\n", encoding="utf-8")
        for i in range(3):
            (repo / f"old_module_{i}.py").write_text(
                f"def old_func_{i}():\n    OLD_DELETE_MARKER_{i} = True\n    return 'old_{i}'\n",
                encoding="utf-8",
            )
        _commit(repo, "init")

        (repo / "active.py").write_text("def active():\n    return 'active_marker_v2'\n", encoding="utf-8")
        for i in range(3):
            (repo / f"old_module_{i}.py").unlink()
        _commit(repo, "cleanup old modules")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        all_content = _extract_content(context)

        assert "active_marker" in all_content, "Modified file must be in context"
        for i in range(3):
            assert f"OLD_DELETE_MARKER_{i}" not in all_content, f"Deleted file {i} content must not appear"


class TestRenameDetection:
    def test_pure_rename_excludes_old_path_content(self, tmp_path: Path) -> None:
        repo = tmp_path / "rename_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "old_name.py").write_text(
            "def my_function():\n    RENAME_MARKER_OLD = True\n    return 'original'\n",
            encoding="utf-8",
        )
        (repo / "other.py").write_text("def other():\n    return 'other_v1'\n", encoding="utf-8")
        _commit(repo, "init")

        (repo / "old_name.py").rename(repo / "new_name.py")
        (repo / "other.py").write_text("def other():\n    return 'other_v2'\n", encoding="utf-8")
        _commit(repo, "rename file and update other")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert any("other.py" in p for p in paths), "Modified file must be in context"
        old_path_fragments = [p for p in paths if "old_name" in p]
        assert len(old_path_fragments) == 0, f"Old renamed path must not appear in fragments: {old_path_fragments}"

    def test_rename_with_changes_uses_new_path(self, tmp_path: Path) -> None:
        repo = tmp_path / "rename_change_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "utils.py").write_text(
            "def helper():\n    return 'helper_v1'\n\ndef another():\n    return 'another'\n",
            encoding="utf-8",
        )
        _commit(repo, "init")

        content = "def helper():\n    return 'helper_v2'\n\ndef another():\n    return 'another'\n"
        (repo / "utils.py").unlink()
        (repo / "helpers.py").write_text(content, encoding="utf-8")
        _commit(repo, "rename and update")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        assert not any("utils.py" in p for p in paths), "Old path must not appear for rename with changes"

    def test_directory_rename_excludes_old_paths(self, tmp_path: Path) -> None:
        repo = tmp_path / "dir_rename_repo"
        repo.mkdir()
        _init_repo(repo)

        old_dir = repo / "old_pkg"
        old_dir.mkdir()
        (old_dir / "module_a.py").write_text("def func_a():\n    DIR_RENAME_MARKER_A = True\n    return 'a'\n", encoding="utf-8")
        (old_dir / "module_b.py").write_text("def func_b():\n    DIR_RENAME_MARKER_B = True\n    return 'b'\n", encoding="utf-8")
        (repo / "main.py").write_text("def main():\n    return 'main_v1'\n", encoding="utf-8")
        _commit(repo, "init")

        new_dir = repo / "new_pkg"
        old_dir.rename(new_dir)
        (repo / "main.py").write_text("def main():\n    return 'main_v2'\n", encoding="utf-8")
        _commit(repo, "rename directory")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=5000)
        paths = _extract_paths(context)

        old_paths = [p for p in paths if "old_pkg" in p]
        assert len(old_paths) == 0, f"Old directory paths must not appear: {old_paths}"


class TestGeneratedApiFiles:
    def test_api_files_capped_as_generated(self, tmp_path: Path) -> None:
        repo = tmp_path / "api_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "app.kt").write_text('class App {\n    fun run() = println("hello")\n}\n', encoding="utf-8")

        api_content_lines = [f"  fun apiMethod{i}(): String" for i in range(100)]
        api_content = "class GeneratedApi {\n" + "\n".join(api_content_lines) + "\n}\n"
        (repo / "public.api").write_text(api_content, encoding="utf-8")
        _commit(repo, "init")

        (repo / "app.kt").write_text('class App {\n    fun run() = println("hello v2")\n}\n', encoding="utf-8")
        api_content_lines_v2 = [f"  fun apiMethod{i}(): String" for i in range(101)]
        api_content_v2 = "class GeneratedApi {\n" + "\n".join(api_content_lines_v2) + "\n}\n"
        (repo / "public.api").write_text(api_content_v2, encoding="utf-8")
        _commit(repo, "update")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=50000)
        all_content = _extract_content(context)

        assert "App" in all_content, "Changed Kotlin file must be in context"
        api_method_count = sum(1 for i in range(101) if f"apiMethod{i}" in all_content)
        assert api_method_count <= 35, f"API file should be truncated as generated, but found {api_method_count} methods"

    def test_pb_go_still_treated_as_generated(self, tmp_path: Path) -> None:
        repo = tmp_path / "proto_repo"
        repo.mkdir()
        _init_repo(repo)

        (repo / "service.go").write_text('package main\n\nfunc Serve() string {\n\treturn "v1"\n}\n', encoding="utf-8")
        proto_lines = [f"func Method{i}() {{}}" for i in range(50)]
        (repo / "service.pb.go").write_text("package main\n\n" + "\n".join(proto_lines) + "\n", encoding="utf-8")
        _commit(repo, "init")

        (repo / "service.go").write_text('package main\n\nfunc Serve() string {\n\treturn "v2"\n}\n', encoding="utf-8")
        proto_lines_v2 = [f"func Method{i}() {{}}" for i in range(51)]
        (repo / "service.pb.go").write_text("package main\n\n" + "\n".join(proto_lines_v2) + "\n", encoding="utf-8")
        _commit(repo, "update")

        context = build_diff_context(root_dir=repo, diff_range="HEAD~1..HEAD", budget_tokens=50000)
        all_content = _extract_content(context)

        assert "Serve" in all_content, "Changed Go file must be in context"
        method_count = sum(1 for i in range(51) if f"Method{i}" in all_content)
        assert method_count <= 10, f"Proto generated file should be capped, found {method_count} methods"
