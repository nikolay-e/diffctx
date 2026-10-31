"""The label-driven close job and the release manifest (T7, #595).

Both scripts speak to a tracker's REST API, which is outside our control: a
local HTTP server stands in for GitHub and Forgejo, keeps real state, and
answers the way the two platforms do (page sizes, `type=issues`, pull requests
listed among GitHub's issues).
"""

import json
import os
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import pytest

RELEASE = Path(__file__).parent.parent / "scripts" / "release"
LABEL = "fixed-unreleased"
PAGE_SIZE = {"github": 100, "forgejo": 50}
PAGE_PARAM = {"github": "per_page", "forgejo": "limit"}
REPO = "nikolay-e/diffctx"


def _issue(number, labels=(LABEL,), state="open", pull_request=False, title=None):
    issue = {"number": number, "title": title or f"issue {number}", "state": state, "labels": [{"name": n} for n in labels]}
    if pull_request:
        issue["pull_request"] = {}
    return issue


class FakeTrackers(ThreadingHTTPServer):
    """GitHub under /github and Forgejo under /forgejo, one repository each."""

    def __init__(self):
        super().__init__(("127.0.0.1", 0), _Handler)
        self.issues = {"github": {}, "forgejo": {}}
        self.comments = {"github": {}, "forgejo": {}}
        self.requests = []
        self.lock = threading.Lock()

    def add(self, kind, issue, comments=()):
        self.issues[kind][issue["number"]] = issue
        self.comments[kind][issue["number"]] = [{"body": body} for body in comments]

    def url(self, kind):
        return f"http://127.0.0.1:{self.server_address[1]}/{kind}"


class _Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def _route(self):
        parsed = urlparse(self.path)
        kind, _, rest = parsed.path.lstrip("/").partition("/")
        parts = rest.split("/")
        assert parts[:3] == ["repos", "nikolay-e", "diffctx"], self.path
        return kind, parts[3:], {k: v[0] for k, v in parse_qs(parsed.query).items()}

    def _send(self, payload, status=200):
        body = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _body(self):
        return json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"{}")

    def _page(self, kind, rows, query):
        size = min(int(query[PAGE_PARAM[kind]]), PAGE_SIZE[kind])
        start = (int(query["page"]) - 1) * size
        return rows[start : start + size]

    def do_GET(self):
        kind, parts, query = self._route()
        server = self.server
        with server.lock:
            server.requests.append(("GET", self.path, self.headers.get("Authorization")))
            if parts == ["issues"]:
                assert (kind == "forgejo") == (query.get("type") == "issues"), query
                rows = [
                    i
                    for i in server.issues[kind].values()
                    if i["state"] == query["state"] and query["labels"] in [n["name"] for n in i["labels"]]
                ]
                return self._send(self._page(kind, sorted(rows, key=lambda i: i["number"]), query))
            if parts[0] == "issues" and parts[2:] == ["comments"]:
                return self._send(self._page(kind, server.comments[kind][int(parts[1])], query))
        self._send({"message": "not found"}, 404)

    def do_POST(self):
        kind, parts, _ = self._route()
        server = self.server
        with server.lock:
            server.requests.append(("POST", self.path, self.headers.get("Authorization")))
            if parts[0] == "issues" and parts[2:] == ["comments"]:
                server.comments[kind][int(parts[1])].append(self._body())
                return self._send({}, 201)
        self._send({"message": "not found"}, 404)

    def do_PATCH(self):
        kind, parts, _ = self._route()
        server = self.server
        with server.lock:
            body = self._body()
            server.requests.append(("PATCH", self.path, self.headers.get("Authorization"), body))
            if parts[0] == "issues" and len(parts) == 2:
                server.issues[kind][int(parts[1])]["state"] = body["state"]
                return self._send({})
        self._send({"message": "not found"}, 404)


@pytest.fixture
def trackers():
    server = FakeTrackers()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield server
    server.shutdown()
    server.server_close()
    thread.join()


def _run(script, trackers, *args, **env):
    environment = {k: v for k, v in os.environ.items() if k not in {"GH_TOKEN", "GITHUB_TOKEN", "FORGEJO_TOKEN"}}
    environment.update(GITHUB_API_URL=trackers.url("github"), FORGEJO_API_URL=trackers.url("forgejo"), **env)
    return subprocess.run(
        [sys.executable, str(RELEASE / script), *args], capture_output=True, text=True, env=environment, check=False
    )


def _states(trackers, kind):
    return {number: issue["state"] for number, issue in trackers.issues[kind].items()}


class TestCloseReleased:
    def test_it_closes_exactly_the_labelled_open_issues_after_telling_them(self, trackers):
        trackers.add("github", _issue(1))
        trackers.add("github", _issue(2, labels=("bug",)))
        trackers.add("github", _issue(3, pull_request=True))
        trackers.add("github", _issue(4, state="closed"))
        result = _run("close-released.py", trackers, "--tracker", "github", "--tag", "v2.0.0", GH_TOKEN="s3cret")
        assert result.returncode == 0, result.stderr
        assert _states(trackers, "github") == {1: "closed", 2: "open", 3: "open", 4: "closed"}
        assert trackers.comments["github"][1] == [{"body": "Released in v2.0.0"}]
        assert trackers.comments["github"][2] == []
        patches = [r for r in trackers.requests if r[0] == "PATCH"]
        assert patches == [
            ("PATCH", f"/github/repos/{REPO}/issues/1", "token s3cret", {"state": "closed", "state_reason": "completed"})
        ]
        assert "s3cret" not in result.stdout + result.stderr

    def test_a_dry_run_names_the_issues_and_changes_nothing(self, trackers):
        trackers.add("github", _issue(7, title="a fixed thing"))
        result = _run("close-released.py", trackers, "--tracker", "github", "--tag", "v2.0.0", "--dry-run")
        assert result.returncode == 0, result.stderr
        assert "would close github#7: a fixed thing" in result.stdout
        assert _states(trackers, "github") == {7: "open"}
        assert trackers.comments["github"][7] == []
        assert {r[0] for r in trackers.requests} == {"GET"}

    def test_a_rerun_finishes_what_a_failed_run_left_without_telling_twice(self, trackers):
        # The first run commented on #5 and died before closing it.
        trackers.add("github", _issue(5), comments=["Released in v2.0.0"])
        trackers.add("github", _issue(6))
        assert _run("close-released.py", trackers, "--tracker", "github", "--tag", "v2.0.0").returncode == 0
        assert _states(trackers, "github") == {5: "closed", 6: "closed"}
        assert trackers.comments["github"][5] == [{"body": "Released in v2.0.0"}]
        again = _run("close-released.py", trackers, "--tracker", "github", "--tag", "v2.0.0")
        assert "0 issue(s)" in again.stdout

    def test_every_page_is_closed_not_only_the_first(self, trackers):
        for number in range(1, 121):
            trackers.add("github", _issue(number))
        assert _run("close-released.py", trackers, "--tracker", "github", "--tag", "v2.0.0").returncode == 0
        assert set(_states(trackers, "github").values()) == {"closed"}

    def test_forgejo_is_asked_in_its_own_dialect_with_a_token_from_a_file(self, trackers, tmp_path):
        for number in range(1, 61):
            trackers.add("forgejo", _issue(number))
        token = tmp_path / "token"
        token.write_text("fj-token\n", encoding="utf-8")
        token.chmod(0o600)
        result = _run("close-released.py", trackers, "--tracker", "forgejo", "--tag", "v2.0.0", "--token-file", str(token))
        assert result.returncode == 0, result.stderr
        assert set(_states(trackers, "forgejo").values()) == {"closed"}
        assert all(r[2] == "token fj-token" for r in trackers.requests)
        patches = [r[3] for r in trackers.requests if r[0] == "PATCH"]
        assert patches and all(body == {"state": "closed"} for body in patches)
        assert "fj-token" not in result.stdout + result.stderr

    def test_forgejo_without_an_address_is_refused_not_guessed(self, trackers):
        result = subprocess.run(
            [sys.executable, str(RELEASE / "close-released.py"), "--tracker", "forgejo", "--tag", "v2.0.0"],
            capture_output=True,
            text=True,
            env={k: v for k, v in os.environ.items() if k != "FORGEJO_API_URL"},
            check=False,
        )
        assert result.returncode != 0
        assert "FORGEJO_API_URL" in result.stderr

    @pytest.mark.parametrize("tag", ["2.0.0", "v2.0", "v2.0.0-rc1", "main"])
    def test_only_a_release_tag_closes_anything(self, trackers, tag):
        trackers.add("github", _issue(1))
        result = _run("close-released.py", trackers, "--tracker", "github", "--tag", tag)
        assert result.returncode != 0
        assert _states(trackers, "github") == {1: "open"}
        assert trackers.requests == []


class TestManifest:
    def _fixed(self, sha="0123abc", gate="tests/cases/impact/python/body_only.yaml"):
        return f"Fixed on relay/arch in {sha}; gate: {gate}"

    def test_it_writes_issue_tracker_root_gate_and_sha_for_both_trackers(self, trackers, tmp_path):
        trackers.add("github", _issue(10, labels=(LABEL, "root:python-callers")), comments=["thanks", self._fixed()])
        trackers.add(
            "forgejo", _issue(3, labels=(LABEL,)), comments=[self._fixed("abcdef0123", "metamorphic::rename_keeps_callers")]
        )
        trackers.add("github", _issue(11, labels=("bug",)))
        out = tmp_path / "release-manifest.json"
        result = _run("manifest.py", trackers, "--tracker", "github", "--tracker", "forgejo", "--output", str(out))
        assert result.returncode == 0, result.stderr
        assert json.loads(out.read_text(encoding="utf-8")) == {
            "issues": [
                {
                    "issue": 3,
                    "tracker": "forgejo",
                    "root": None,
                    "gate": "metamorphic::rename_keeps_callers",
                    "fixing_sha": "abcdef0123",
                },
                {
                    "issue": 10,
                    "tracker": "github",
                    "root": "python-callers",
                    "gate": "tests/cases/impact/python/body_only.yaml",
                    "fixing_sha": "0123abc",
                },
            ]
        }

    def test_the_latest_fixing_comment_wins(self, trackers, tmp_path):
        trackers.add("github", _issue(1), comments=[self._fixed("1111111", "old gate"), self._fixed("2222222", "new gate")])
        out = tmp_path / "m.json"
        assert _run("manifest.py", trackers, "--tracker", "github", "--output", str(out)).returncode == 0
        [row] = json.loads(out.read_text(encoding="utf-8"))["issues"]
        assert (row["fixing_sha"], row["gate"]) == ("2222222", "new gate")

    @pytest.mark.parametrize(
        "comment",
        [
            "Fixed on relay/arch in 0123abc; gate: ",
            "Fixed on relay/arch in 0123abc",
            "Fixed in 0123abc; gate: tests/x.yaml",
            "fixed on relay/arch in 0123abc; gate: tests/x.yaml",
        ],
    )
    def test_a_labelled_issue_without_a_gate_fails_the_run_and_writes_nothing(self, trackers, tmp_path, comment):
        trackers.add("github", _issue(1), comments=[self._fixed()])
        trackers.add("github", _issue(2), comments=[comment])
        out = tmp_path / "m.json"
        result = _run("manifest.py", trackers, "--tracker", "github", "--output", str(out))
        assert result.returncode == 1
        assert "github#2" in result.stderr
        assert "github#1" not in result.stderr
        assert not out.exists()
