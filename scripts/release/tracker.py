"""The issues of one tracker as the release scripts read them.

GitHub and Forgejo answer the calls used here in the same shape, so one client
serves both. Nothing here names a host: GitHub's API address is the platform's
(`GITHUB_API_URL` in Actions), and a Forgejo instance is given by the person
running the script.
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

LABEL = "fixed-unreleased"
DEFAULT_REPO = "nikolay-e/diffctx"
PAGE = {"github": ("per_page", 100), "forgejo": ("limit", 50)}
API_URL_ENV = {"github": "GITHUB_API_URL", "forgejo": "FORGEJO_API_URL"}
TOKEN_ENV = {"github": ("GH_TOKEN", "GITHUB_TOKEN"), "forgejo": ("FORGEJO_TOKEN",)}


class Tracker:
    def __init__(self, kind: str, repo: str, api_url: str, token: str | None) -> None:
        self.kind = kind
        self.repo = repo
        self.api_url = api_url.rstrip("/")
        self.token = token

    @classmethod
    def configured(
        cls, kind: str, repo: str = DEFAULT_REPO, api_url: str | None = None, token_file: str | None = None
    ) -> Tracker:
        api_url = api_url or os.environ.get(API_URL_ENV[kind]) or ("https://api.github.com" if kind == "github" else None)
        if not api_url:
            raise SystemExit(f"{kind}: no API address; pass --api-url or set {API_URL_ENV[kind]}")
        if not api_url.startswith(("https://", "http://")):
            raise SystemExit(f"{kind}: the API address must be http(s): {api_url!r}")
        if token_file:
            token = Path(token_file).read_text(encoding="utf-8").strip()
        else:
            token = next((os.environ[name] for name in TOKEN_ENV[kind] if os.environ.get(name)), None)
        return cls(kind, repo, api_url, token)

    def _call(self, method: str, path: str, query: dict[str, Any] | None = None, body: dict[str, Any] | None = None) -> Any:
        url = f"{self.api_url}/repos/{self.repo}/{path}"
        if query:
            url += "?" + urllib.parse.urlencode(query)
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"token {self.token}"
        data = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=60) as response:
                return json.loads(response.read() or b"null")
        except urllib.error.HTTPError as error:
            raise SystemExit(f"{self.kind}: {method} {path} answered {error.code}: {error.read()[:200]!r}") from error

    def _paged(self, path: str, query: dict[str, Any]) -> list[dict[str, Any]]:
        size_name, size = PAGE[self.kind]
        rows: list[dict[str, Any]] = []
        page = 1
        while True:
            batch = self._call("GET", path, {**query, size_name: size, "page": page})
            if not batch:
                return rows
            rows.extend(batch)
            page += 1

    def labelled_issues(self, label: str = LABEL) -> list[dict[str, Any]]:
        """Open issues carrying `label`, pull requests excluded."""
        query: dict[str, Any] = {"labels": label, "state": "open"}
        if self.kind == "forgejo":
            query["type"] = "issues"
        return [i for i in self._paged("issues", query) if "pull_request" not in i]

    def comments(self, number: int) -> list[dict[str, Any]]:
        return self._paged(f"issues/{number}/comments", {})

    def comment(self, number: int, text: str) -> None:
        self._call("POST", f"issues/{number}/comments", body={"body": text})

    def close(self, number: int) -> None:
        body = {"state": "closed"}
        if self.kind == "github":
            body["state_reason"] = "completed"
        self._call("PATCH", f"issues/{number}", body=body)


def label_names(issue: dict[str, Any]) -> list[str]:
    return [label["name"] if isinstance(label, dict) else str(label) for label in issue.get("labels", [])]
