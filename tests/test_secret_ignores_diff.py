from __future__ import annotations

import diffctx
from tests.conftest import run_diffctx_subprocess
from tests.framework.pygit2_backend import Pygit2Repo


def _build_repo_with_changed_private_keys(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("app.py", "import os\nKEY = os.environ['K']\n")
    repo.add_file("id_rsa", "private-key-material LEAK_RSA_INITIAL\n")  # pragma: allowlist secret
    repo.add_file("tls.key", "private-key-material LEAK_KEY_INITIAL\n")  # pragma: allowlist secret
    repo.add_file("server.pem", "-----BEGIN CERTIFICATE-----\nLEAK_PEM_INITIAL\n")
    repo.commit("initial")

    repo.add_file("app.py", "import os\nKEY = os.environ['K']\nTOKEN = os.environ['T']\n")
    repo.add_file("id_rsa", "private-key-material LEAK_RSA_CHANGED\n")  # pragma: allowlist secret
    repo.add_file("tls.key", "private-key-material LEAK_KEY_CHANGED\n")  # pragma: allowlist secret
    repo.add_file("server.pem", "-----BEGIN CERTIFICATE-----\nLEAK_PEM_CHANGED\n")
    repo.commit("change app and private keys")
    return repo


SECRET_MARKERS = ["LEAK_RSA", "LEAK_KEY", "LEAK_PEM"]


def test_diff_context_excludes_changed_private_keys(tmp_path):
    repo = _build_repo_with_changed_private_keys(tmp_path)

    for full in (False, True):
        rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1", full=full))
        for marker in SECRET_MARKERS:
            assert marker not in rendered, (full, marker)
        assert "id_rsa" not in rendered
        assert "tls.key" not in rendered
        assert "server.pem" not in rendered
        assert "app.py" in rendered


def test_withheld_secret_files_are_disclosed_as_a_count(tmp_path):
    repo = _build_repo_with_changed_private_keys(tmp_path)

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    assert result.get("policy_excluded_count") == 3
    rendered = diffctx.to_yaml(result)
    assert "policy_excluded_count: 3" in rendered
    assert "id_rsa" not in rendered


def test_secret_only_change_still_renders_the_withheld_notice(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("app.py", "import os\nKEY = os.environ['K']\n")
    repo.add_file("id_rsa", "private-key-material LEAK_RSA_INITIAL\n")  # pragma: allowlist secret
    repo.commit("initial")

    repo.add_file("id_rsa", "private-key-material LEAK_RSA_CHANGED\n")  # pragma: allowlist secret
    repo.commit("touch only the secret file")

    result = diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1")
    assert result.get("policy_excluded_count") == 1
    for rendered in (
        diffctx.to_yaml(result),
        diffctx.to_markdown(result),
        diffctx.to_text(result),
    ):
        assert "id_rsa" not in rendered
        assert "LEAK_RSA" not in rendered

    proc = run_diffctx_subprocess([str(repo.path), "--diff", "HEAD~1"], cwd=str(repo.path))
    assert proc.returncode == 0, proc.stderr
    assert "no semantic context" not in proc.stderr
    assert "id_rsa" not in proc.stdout
    assert "withheld by exclusion policy" in proc.stdout
