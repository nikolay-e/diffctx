import subprocess
from pathlib import Path

import gitrepo

DIFF = """diff --git a/a.py b/a.py
index 111..222 100644
--- a/a.py
+++ b/a.py
@@ -3,2 +3 @@ def f():
--- a deleted line that starts with dashes
-    return 1
+    return 2
@@ -10 +9,0 @@
-gone
diff --git a/old.py b/new.py
similarity index 100%
rename from old.py
rename to new.py
diff --git a/added.py b/added.py
new file mode 100644
--- /dev/null
+++ b/added.py
@@ -0,0 +1,2 @@
+x = 1
+y = 2
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
"""


def test_parse_diff_reads_hunks_renames_and_additions():
    changes = gitrepo.parse_diff(DIFF)

    assert [(c.status, c.old_path, c.new_path) for c in changes] == [
        ("M", "a.py", "a.py"),
        ("R", "old.py", "new.py"),
        ("A", None, "added.py"),
    ]
    modified = changes[0]
    assert [(h.old_start, h.old_count, h.new_start, h.new_count) for h in modified.hunks] == [(3, 2, 3, 1), (10, 1, 9, 0)]
    assert list(modified.hunks[0].old_lines) == [3, 4]
    assert list(modified.hunks[1].new_lines) == []
    assert changes[1].hunks == ()
    assert list(changes[2].hunks[0].new_lines) == [1, 2]


def make_repo(path: Path) -> list[str]:
    env = {**gitrepo.git_env(), "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z", "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z"}
    shas = []
    path.mkdir()
    for args in (["init", "-q", "-b", "main"],):
        subprocess.run(["git", *args], cwd=path, env=env, check=True)
    for n, text in enumerate(["a = 1\n", "a = 2\nb = 3\n"]):
        (path / "m.py").write_text(text)
        (path / "other.txt").write_text(f"{n}\n")
        subprocess.run(["git", "add", "-A"], cwd=path, env=env, check=True)
        subprocess.run(
            ["git", "-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", f"c{n}"],
            cwd=path,
            env=env,
            check=True,
        )
        shas.append(
            subprocess.run(
                ["git", "rev-parse", "HEAD"], cwd=path, env=env, capture_output=True, text=True, check=True
            ).stdout.strip()
        )
    return shas


def test_diff_changes_and_export_tree_on_a_real_repository(tmp_path: Path):
    first, second = make_repo(tmp_path / "repo")
    git_dir = tmp_path / "repo" / ".git"

    changes = gitrepo.diff_changes(git_dir, first, second)
    assert {c.new_path for c in changes} == {"m.py", "other.txt"}
    assert gitrepo.parent_of(git_dir, second) == first
    assert gitrepo.parent_of(git_dir, first) is None

    tree = gitrepo.export_tree(git_dir, first, tmp_path / "tree")
    assert (tree / "m.py").read_text() == "a = 1\n"
    assert gitrepo.export_tree(git_dir, first, tmp_path / "tree") == tree

    log = gitrepo.commits_with_paths(git_dir, second, 10)
    assert log == [(second, ["m.py", "other.txt"])]


def test_checkout_moves_a_clone_between_commits(tmp_path: Path):
    first, second = make_repo(tmp_path / "repo")
    git_dir = tmp_path / "repo" / ".git"
    work = tmp_path / "work"

    gitrepo.checkout(work, git_dir, second)
    assert (work / "m.py").read_text() == "a = 2\nb = 3\n"
    gitrepo.checkout(work, git_dir, first)
    assert (work / "m.py").read_text() == "a = 1\n"
