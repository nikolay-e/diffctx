from __future__ import annotations

from pathlib import Path

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo


def _repo_with_import_neighbour(tmp_path, ignore_patterns: str | None):
    """app.py is the only *changed* file across the two commits; neighbor.py is
    committed once and never touched again, so it can only ever enter the
    rendered output as neighbour context discovered because app.py imports it -
    never via the changed_files path (already filtered since #85)."""
    repo = Pygit2Repo(tmp_path / "repo")
    if ignore_patterns is not None:
        repo.add_file(".diffctx/ignore", ignore_patterns)
    repo.add_file("neighbor.py", "NEIGHBOR_MARKER_INITIAL = 1\n")
    repo.add_file(
        "app.py",
        "import neighbor\n\ndef use():\n    return neighbor.NEIGHBOR_MARKER_INITIAL\n",
    )
    repo.commit("initial")

    repo.add_file(
        "app.py",
        "import neighbor\n\ndef use():\n    return neighbor.NEIGHBOR_MARKER_INITIAL + 1\n",
    )
    repo.commit("change app only")
    return repo


def test_diff_context_excludes_diffctx_ignore_neighbour_file(tmp_path):
    """The discovery universe (candidate_files::collect_candidate_files) used to be
    filtered only by language, never by .diffctx/ignore or is_secret_path - unlike
    changed_files (pipeline::compute_scored_state). Because .diffctx/ignore is not a
    gitignore, neighbor.py was still listed by `git ls-files -z`, entered
    all_candidate_files, and was discovered (app.py imports it) and rendered as
    neighbour context even though the user explicitly excluded it. This test fails
    before the collect_candidate_files fix and passes after."""
    repo = _repo_with_import_neighbour(tmp_path, ignore_patterns="neighbor.py\n")

    rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1"))
    assert "neighbor.py" not in rendered
    assert "app.py" in rendered


def test_diff_context_includes_neighbour_file_without_ignore_rule(tmp_path):
    """Control for the test above: with no .diffctx/ignore rule at all, the same
    import-driven discovery DOES surface neighbor.py as neighbour context - proving
    the assertion above exercises the ignore filter rather than a coincidentally
    empty/broken selection."""
    repo = _repo_with_import_neighbour(tmp_path, ignore_patterns=None)

    rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1"))
    assert "neighbor.py" in rendered
    assert "NEIGHBOR_MARKER_INITIAL" in rendered


def test_diff_context_never_leaks_unchanged_private_key_neighbour(tmp_path):
    """Parity check for is_secret_path in the discovery universe: an unchanged
    id_rsa file must never render, no matter why app.py changed. In the current
    tree this is double-guarded - candidate_files::is_candidate_file already
    excludes id_rsa because get_language_for_file("id_rsa") has no registered
    extension/filename mapping (verified: EXTENSION_TO_LANGUAGE and
    FILENAME_TO_LANGUAGE in languages.rs contain no pem/key/pfx/p12/keystore/jks/
    id_rsa entry), so id_rsa never becomes a discovery candidate at all - and the
    is_secret_path check added to collect_candidate_files (mirroring the
    changed_files filter) is a second, independent guard for the day a language
    grammar starts recognising one of those extensions. Because of the first
    guard, this assertion does not flip if the second guard is reverted; it is
    regression coverage for the invariant, not a repro of a live leak."""
    repo = Pygit2Repo(tmp_path / "repo")
    repo.add_file("id_rsa", "PRIVATE_KEY_LEAK_MARKER\n")  # pragma: allowlist secret
    repo.add_file("app.py", "import os\n\nKEY_PATH = 'id_rsa'\n")
    repo.commit("initial")

    repo.add_file("app.py", "import os\n\nKEY_PATH = 'id_rsa'\nTOKEN = os.environ['T']\n")
    repo.commit("change app only")

    rendered = diffctx.to_yaml(diffctx.build_diff_context(root_dir=repo.path, diff_range="HEAD~1"))
    assert "PRIVATE_KEY_LEAK_MARKER" not in rendered
    assert "app.py" in rendered


def test_no_default_ignores_fails_loudly_with_diff():
    """Regression: pybridge.rs silently dropped no_default_ignores with only a
    tracing::warn! that never surfaces (tracing_subscriber is only installed in
    the native binary, never in the extension module) - exit 0, default ignore
    set still applied, no signal to the caller. ignore_file/whitelist_file already
    raised NotImplementedError from the Python wrapper; no_default_ignores now
    does the same instead of being silently discarded."""
    with pytest.raises(NotImplementedError, match="no-default-ignores"):
        diffctx.build_diff_context(root_dir=Path("."), diff_range="HEAD", no_default_ignores=True)
