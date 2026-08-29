from __future__ import annotations

import json

import diffctx
from diffctx._diffctx import is_secret_path
from tests.conftest import run_diffctx_subprocess
from tests.framework.pygit2_backend import Pygit2Repo

# Every name the engine's policy withholds, each with a unique marker: the
# tree map used to carry a second, shorter list and printed `.netrc`,
# `credentials` and `*.asc` that diff mode refused (#227).
SECRETS = {
    ".netrc": "LEAK_NETRC",
    "_netrc": "LEAK_UNETRC",
    "credentials": "LEAK_CREDENTIALS",
    ".npmrc": "LEAK_NPMRC",
    ".pypirc": "LEAK_PYPIRC",
    "id_ed25519_sk": "LEAK_SK",
    "deploy.ppk": "LEAK_PPK",
    "auth.p8": "LEAK_P8",
    "backup.asc": "LEAK_ASC",
    "server.pem": "LEAK_PEM",
    "id_rsa": "LEAK_RSA",
}
KEPT = {"id_rsa.pub": "PUBLIC_KEY_OK", ".env": "ENV_OK", "app.py": "APP_OK"}


def _repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    for name, marker in {**SECRETS, **KEPT}.items():
        repo.add_file(name, f"{marker}_INITIAL\n")  # pragma: allowlist secret
    repo.commit("initial")
    for name, marker in {**SECRETS, **KEPT}.items():
        repo.add_file(name, f"{marker}_CHANGED\n")  # pragma: allowlist secret
    repo.commit("change everything")
    return repo


def test_tree_and_diff_mode_withhold_the_same_secret_files(tmp_path):
    repo = _repo(tmp_path)

    diff_out = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1"))
    tree = run_diffctx_subprocess([str(repo.path), "-f", "json"], cwd=str(repo.path))
    assert tree.returncode == 0, tree.stderr
    tree_out = tree.stdout
    tree_names = {c["name"] for c in json.loads(tree_out)["children"]}

    for name, marker in SECRETS.items():
        assert marker not in diff_out, name
        assert marker not in tree_out, f"tree mode printed {name}"
        assert name not in tree_names, f"tree mode listed {name}"
        assert is_secret_path(name), name
    for name, marker in KEPT.items():
        assert marker in tree_out, name
        assert not is_secret_path(name), name
    assert "APP_OK_CHANGED" in diff_out


def test_no_default_ignores_does_not_reopen_secrets(tmp_path):
    repo = _repo(tmp_path)
    tree = run_diffctx_subprocess([str(repo.path), "-f", "json", "--no-default-ignores"], cwd=str(repo.path))
    assert tree.returncode == 0, tree.stderr
    # The flag drops the noise filters (build/, node_modules/...); the secret
    # policy is not a default ignore, it is the same unconditional contract
    # diff mode has.
    for name, marker in SECRETS.items():
        assert marker not in tree.stdout, name
