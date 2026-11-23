"""Synthetic two-commit shapes for reported impact gaps; the answer is `--diff HEAD~1..HEAD --mode impact` in md.
Usage: shapes.py <out-dir> <binary>... [shape...]   Output files are <shape>.<binary dir name>.md.
"""

import subprocess, sys, tempfile
from pathlib import Path

SHAPES = {
    "mcp_tool_docstring": (
        {
            "server.py": 'from mcp import FastMCP\nmcp = FastMCP("s")\n\n@mcp.tool()\ndef get_workouts(day: str) -> list:\n    """Workouts of one day."""\n    return []\n'
        },
        {
            "server.py": 'from mcp import FastMCP\nmcp = FastMCP("s")\n\n@mcp.tool()\ndef get_workouts(day: str) -> list:\n    """Workouts of one day, newest first; day is ISO-8601 and required."""\n    return []\n'
        },
    ),
    "dataclass_common_fields": (
        {
            "model.py": "from dataclasses import dataclass\n\n@dataclass\nclass Reading:\n    date: str\n    value: float\n",
            **{
                f"mod{i}.py": f"def f{i}(row, other):\n    return row.date, other.value, row.value\n"
                for i in range(12)
            },
            "use.py": "from model import Reading\n\ndef latest(r: Reading):\n    return r.date\n",
        },
        {
            "model.py": "from dataclasses import dataclass\n\n@dataclass\nclass Reading:\n    date: str\n    value: float\n    unit: str = 'kg'\n"
        },
    ),
    "underscore_name_in_nginx": (
        {
            "app.py": "def _session(token):\n    return token\n\ndef handler(t):\n    return _session(t)\n",
            "deploy/nginx.conf": 'server {\n  location /api/ {\n    proxy_set_header Cookie $http_cookie; # session cookie\n    proxy_cookie_path / "/; session";\n  }\n}\n',
        },
        {
            "app.py": "def _session(token):\n    return token.strip()\n\ndef handler(t):\n    return _session(t)\n"
        },
    ),
    "lazy_import_then_named": (
        {
            "src/DashboardLayout.tsx": "export function DashboardLayout() {\n  return null;\n}\n",
            "src/App.tsx": 'import { lazy } from "react";\nconst Layout = lazy(() => import("./DashboardLayout").then((m) => ({ default: m.DashboardLayout })));\nexport const App = () => Layout;\n',
        },
        {
            "src/DashboardLayout.tsx": "export function DashboardLayout() {\n  return 1;\n}\n"
        },
    ),
}

SHAPES["dataclass_field_retyped"] = (
    {"model.py": "from dataclasses import dataclass\n\n@dataclass\nclass Reading:\n    date: str\n    value: float\n",
     **{f"mod{i}.py": f"def f{i}(row, other):\n    return row.date, other.value, row.value\n" for i in range(40)},
     "use.py": "from model import Reading\n\ndef latest(r: Reading):\n    return r.date\n"},
    {"model.py": "from dataclasses import dataclass\n\n@dataclass\nclass Reading:\n    date: int\n    value: int\n"},
)
SHAPES["underscore_name_in_ssl_directive"] = (
    {"app.py": "def _session(token):\n    return token\n\ndef handler(t):\n    return _session(t)\n",
     "deploy/nginx.conf": "server {\n  ssl_session_cache shared:SSL:10m;\n  ssl_session_timeout 1d;\n}\n"},
    {"app.py": "def _session(token, ttl=0):\n    return token\n\ndef handler(t):\n    return _session(t)\n"},
)
SHAPES["underscore_name_removed_nginx"] = (
    {"app.py": "def _session(token):\n    return token\n\ndef keep():\n    return 1\n",
     "deploy/nginx.conf": "server {\n  ssl_session_cache shared:SSL:10m;\n}\n"},
    {"app.py": "def keep():\n    return 1\n"},
)
SHAPES["lazy_import_then_bare"] = (
    {"src/layouts/DashboardLayout.tsx": "export function DashboardLayout() {\n  return null;\n}\n",
     "tsconfig.json": '{"compilerOptions": {"baseUrl": ".", "paths": {"@/*": ["src/*"]}}}\n',
     "src/App.tsx": 'import { lazy } from "react";\nconst DashboardLayout = lazy(() =>\n  import("@/layouts/DashboardLayout").then(m => ({ default: m.DashboardLayout }))\n);\nexport function App() {\n  return <DashboardLayout />;\n}\n'},
    {"src/layouts/DashboardLayout.tsx": "export function DashboardLayout() {\n  return 1;\n}\n"},
)
SHAPES["lazy_import_then_relative_jsx"] = (
    {"src/layouts/DashboardLayout.tsx": "export function DashboardLayout() {\n  return null;\n}\n",
     "src/App.tsx": 'import { lazy } from "react";\nconst DashboardLayout = lazy(() =>\n  import("./layouts/DashboardLayout").then(m => ({ default: m.DashboardLayout }))\n);\nexport function App() {\n  return <DashboardLayout />;\n}\n'},
    {"src/layouts/DashboardLayout.tsx": "export function DashboardLayout() {\n  return 1;\n}\n"},
)

SHAPES["underscore_name_in_nginx_url"] = (
    {"api/app.py": "def _session(token):\n    return token\n\ndef handler(t):\n    return _session(t)\n",
     "web/nginx.conf": "server {\n  location /api/ {\n    auth_request /_session;\n  }\n  location = /_session {\n    proxy_pass http://auth/session;\n  }\n}\n"},
    {"api/app.py": "def _session(token):\n    return token.strip()\n\ndef handler(t):\n    return _session(t)\n"},
)
SHAPES["binary_over_cap_is_code"] = (
    {"app.py": "def f(x):\n    return x\n\ndef g():\n    return f(1)\n",
     "other/data/sweep.npy": "\x93NUMPY" + "0" * 3_000_000,
     "other/fonts/Noto.ttf": "\x00\x01" + "1" * 3_000_000},
    {"app.py": "def f(x):\n    return x + 1\n\ndef g():\n    return f(1)\n"},
)


def git(repo, *args):
    subprocess.run(
        ["git", "-C", str(repo), *args],
        check=True,
        capture_output=True,
        env={
            "GIT_AUTHOR_DATE": "2026-01-01T00:00:00Z",
            "GIT_COMMITTER_DATE": "2026-01-01T00:00:00Z",
            "PATH": "/usr/bin:/bin:/opt/homebrew/bin",
            "HOME": "/nonexistent",
        },
    )


def build(root, before, after):
    git(root, "init", "-q", "-b", "main")
    git(root, "config", "user.email", "t@example.com")
    git(root, "config", "user.name", "t")
    for files in (before, after):
        for path, text in files.items():
            (root / path).parent.mkdir(parents=True, exist_ok=True)
            (root / path).write_text(text)
        git(root, "add", "-A")
        git(root, "commit", "-q", "-m", "c")


def main():
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    binaries = [a for a in sys.argv[2:] if "/" in a]
    shapes = [a for a in sys.argv[2:] if "/" not in a] or list(SHAPES)
    for name in shapes:
        with tempfile.TemporaryDirectory() as tmp:
            repo = Path(tmp) / name
            repo.mkdir()
            build(repo, *SHAPES[name])
            for b in binaries:
                tag = Path(b).parent.name
                r = subprocess.run(
                    [
                        b,
                        str(repo),
                        "--diff",
                        "HEAD~1..HEAD",
                        "--mode",
                        "impact",
                        "-f",
                        "md",
                        "--quiet",
                    ],
                    capture_output=True,
                    text=True,
                )
                (out / f"{name}.{tag}.md").write_text(
                    r.stdout
                    + (
                        f"\n[stderr rc={r.returncode}]\n{r.stderr}"
                        if r.returncode
                        else ""
                    )
                )
                print(f"--- {name} [{tag}]\n{r.stdout[:1500]}")


main()
