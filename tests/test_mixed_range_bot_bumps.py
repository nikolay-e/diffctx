"""#263: a range mixing one-line bot bumps with hand-written manifests.

The reporter's shape, a GitOps repo: an image updater commits dozens of single-line
`tag:` bumps on top of a human commit that rewrote four multi-line manifests.
diffctx 1.15.0 kept the bumps, dropped the four manifests, said nothing
about the drop, and titled the range by the bot commit. Three contracts pinned
here: every hand-written change is represented, an omission is disclosed on
every structured surface, and a multi-commit range is not titled by whichever
commit happens to be last.
"""

from __future__ import annotations

import json

import pytest

import diffctx
from tests.framework.pygit2_backend import Pygit2Repo

BOT_SUBJECT = "build: automatic update of image tags"
HUMAN_SUBJECT = "move staging volume off the node disk, add alert and retention"

BUMP_FILES = [f"apps/svc-{i}/values.images.yaml" for i in range(6)]
MANIFESTS = {
    "infra/backup-cronjob.yaml": (
        "apiVersion: batch/v1\nkind: CronJob\nmetadata:\n  name: backup\n  namespace: infra\nspec:\n"
        "  schedule: '0 3 * * *'\n  jobTemplate:\n    spec:\n      template:\n        spec:\n"
        "          containers:\n            - name: backup\n              image: registry/backup:1\n"
        "              volumeMounts:\n                - name: staging\n                  mountPath: /staging\n"
        "          volumes:\n            - name: staging\n              {VOLUME}\n          restartPolicy: OnFailure\n"
    ),
    "infra/namespace.yaml": ("apiVersion: v1\nkind: Namespace\nmetadata:\n  name: infra\n  labels:\n    team: platform\n{PSS}"),
    "infra/alerts.yaml": (
        "apiVersion: monitoring.coreos.com/v1\nkind: PrometheusRule\nmetadata:\n  name: infra-alerts\n"
        "spec:\n  groups:\n    - name: infra\n      rules:\n        - alert: DiskFull\n"
        "          expr: node_filesystem_avail_bytes < 1e9\n          for: 10m\n{RULE}"
    ),
    "infra/db-cluster.yaml": (
        "apiVersion: postgresql.cnpg.io/v1\nkind: Cluster\nmetadata:\n  name: db\nspec:\n  instances: 3\n"
        "  storage:\n    size: 50Gi\n  backup:\n    barmanObjectStore:\n      destinationPath: s3://backups/db\n"
        "    retentionPolicy: '{RETENTION}'\n"
    ),
}
BEFORE = {"VOLUME": "hostPath:\n                path: /var/staging", "PSS": "", "RULE": "", "RETENTION": "7d"}
AFTER = {
    "VOLUME": "persistentVolumeClaim:\n                claimName: staging",
    "PSS": "    pod-security.kubernetes.io/enforce: privileged\n    pod-security.kubernetes.io/audit: baseline\n",
    "RULE": "".join(
        f"        - alert: {name}\n          expr: {expr}\n          for: 1h\n          labels:\n"
        f"            severity: page\n          annotations:\n            summary: {summary}\n"
        for name, expr, summary in (
            ("BackupMissing", "time() - backup_last_success_seconds > 86400", "no successful backup in a day"),
            ("BackupStaging", "backup_staging_bytes > 5e10", "staging volume above 50 GB"),
            ("BackupSlow", "backup_duration_seconds > 7200", "backup took more than two hours"),
        )
    ),
    "RETENTION": "30d",
}


def _bump(i: int, sha: str) -> str:
    # One values file per app, twelve images each: the updater rewrites every
    # `tag:` line in one commit, so a file yields a dozen ~60-token cores.
    entries = "".join(
        f"  {name}:\n    repository: registry/svc-{i}/{name}\n    tag: main-{sha}\n"
        for name in ("api", "web", "worker", "cron", "migrate", "proxy", "cache", "search", "mailer", "queue", "auth", "docs")
    )
    return f"images:\n{entries}"


@pytest.fixture
def gitops_repo(tmp_path):
    repo = Pygit2Repo(tmp_path / "repo")
    for i, path in enumerate(BUMP_FILES):
        repo.add_file(path, _bump(i, "aaaaaaa"))
    for path, template in MANIFESTS.items():
        repo.add_file(path, template.format(**BEFORE))
    repo.commit("initial")
    for path, template in MANIFESTS.items():
        repo.add_file(path, template.format(**AFTER))
    repo.commit(HUMAN_SUBJECT)
    for i, path in enumerate(BUMP_FILES):
        repo.add_file(path, _bump(i, "bbbbbbb"))
    repo.commit(BOT_SUBJECT)
    return repo


@pytest.mark.parametrize("budget", [None, 4000])
def test_hand_written_manifests_survive_the_bot_bumps(gitops_repo, budget):
    kwargs = {"budget_tokens": budget} if budget is not None else {}
    result = diffctx.build_diff_context(root_dir=gitops_repo.path, diff_range="HEAD~2", **kwargs)
    files = {f["path"] for f in result.get("fragments") or []}
    missing = sorted(set(MANIFESTS) - files)
    assert not missing, f"hand-written manifests dropped in favour of bot bumps: {missing}; emitted {sorted(files)}"


def test_an_unavoidable_omission_is_disclosed_on_every_structured_surface(gitops_repo):
    result = diffctx.build_diff_context(root_dir=gitops_repo.path, diff_range="HEAD~2", budget_tokens=300)
    represented = {f["path"] for f in result.get("fragments") or []}
    omitted = sorted(set(result["changed_files"]) - represented)
    assert omitted, "a 300-token budget over 13 changed files must omit something"
    inventory = {c["path"]: c for c in result["changes"]}
    assert sorted(inventory) == sorted(result["changed_files"]), "every changed file has an inventory row"
    assert sorted(p for p, c in inventory.items() if not c["represented"]) == omitted
    assert {inventory[p]["class"] for p in BUMP_FILES} == {"mechanical"}
    assert inventory["infra/alerts.yaml"]["class"] == "content"
    # A rendering may trim further to hold the budget; whatever it emits, its
    # inventory rows agree with its own fragments.
    document = json.loads(diffctx.to_json(result))
    emitted = {f["path"] for f in document["fragments"]}
    assert all(c["represented"] == (c["path"] in emitted) for c in document["changes"])
    assert "represented: false" in diffctx.to_yaml(result)
    for rendered in (diffctx.to_markdown(result), diffctx.to_text(result)):
        assert "omitted" in rendered


def test_a_multi_commit_range_is_not_titled_by_the_last_commit(gitops_repo):
    result = diffctx.build_diff_context(root_dir=gitops_repo.path, diff_range="HEAD~2")
    subjects = result.get("commit_messages") or []
    assert subjects == [BOT_SUBJECT, HUMAN_SUBJECT], subjects
    rendered = diffctx.to_markdown(result)
    assert HUMAN_SUBJECT in rendered
    assert "2 commits" in rendered
