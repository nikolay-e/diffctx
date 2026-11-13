"""Self-test of bench_hook.py on a synthetic log, a real git repository and a stub hook binary."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

import bench_hook

STUB = """#!/bin/sh
cat > /dev/null
case "$STUB_MODE" in
  slow) sleep 0 ;;
esac
printf '%s %s %s %s %s %s %sms %s call=stub\\n' 1 bench PostToolUse inspect - ok 5 "$PWD" >> "$DIFFCTX_CACHE_DIR/diffctx/hook-runs.log"
if [ "$STUB_MODE" = deadline ]; then echo "(partial: deadline)"; fi
"""


def git(repo: Path, *args: str) -> str:
    done = subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@t", *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return done.stdout.strip()


class BenchTest(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        (self.repo / "a.txt").write_text("a\n")
        git(self.repo, "add", "a.txt")
        git(self.repo, "commit", "-q", "-m", "one")
        self.old = git(self.repo, "rev-parse", "HEAD")
        (self.repo / "a.txt").write_text("b\n")
        git(self.repo, "commit", "-q", "-am", "two")
        self.new = git(self.repo, "rev-parse", "HEAD")
        (self.repo / "bulk").mkdir()
        (self.repo / "bulk" / "f.txt").write_text("x\n")
        self.binary = self.root / "stub"
        self.binary.write_text(STUB)
        self.binary.chmod(self.binary.stat().st_mode | stat.S_IXUSR)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def log(self, lines: list[str]) -> Path:
        path = self.root / "hook-runs.log"
        path.write_text("\n".join(lines) + "\n")
        return path

    def line(self, session: str, verb: str, rng: str, repo: str, event: str = "PostToolUse") -> str:
        return f"1 {session} {event} {verb} {rng} ok 12ms {repo} call=x"

    def run_bench(self, log: Path, *extra: str, mode: str = "") -> tuple[int, dict[str, object]]:
        out = self.root / "out"
        os.environ["STUB_MODE"] = mode
        code = bench_hook.main(["--bin", str(self.binary), "--out", str(out), "--log", str(log), *extra])
        return code, json.loads((out / "summary.json").read_text())

    def test_replays_and_counts_what_it_skips(self) -> None:
        log = self.log(
            [
                self.line("s1", "push", f"{self.old[:12]}..{self.new[:12]}", str(self.repo)),
                self.line("s1", "inspect", "-", str(self.repo)),
                self.line("s1", "grep", "-", str(self.repo)),
                self.line(
                    "s1",
                    "push",
                    f"{self.old[:12]}..{self.new[:12]}",
                    str(self.root / "gone"),
                ),
                self.line("s1", "push", "deadbeef0000..deadbeef1111", str(self.repo)),
                self.line("s9", "inspect", "-", str(self.repo)),
                self.line("s1", "-", "-", str(self.repo)),
                self.line("s1", "inspect", "-", "-", event="PreToolUse"),
            ]
        )
        code, summary = self.run_bench(log)
        self.assertEqual(code, 0)
        self.assertEqual(summary["replayed"], 4)  # push, inspect, inspect-owned, grep
        self.assertEqual(
            summary["skipped"],
            {
                "excluded-session": 1,
                "repo-gone": 1,
                "range-gone": 1,
                "no-trigger": 1,
                "not-a-posttooluse-run": 1,
            },
        )
        self.assertEqual(summary["partial_deadline"], 0)

    def test_a_partial_deadline_is_listed_and_fails_the_gate(self) -> None:
        log = self.log([self.line("s1", "inspect", "-", str(self.repo))])
        code, summary = self.run_bench(log, mode="deadline")
        self.assertEqual(code, 1)
        self.assertEqual(summary["partial_deadline"], 2)
        exceptions = summary["exceptions"]
        assert isinstance(exceptions, list)
        self.assertEqual(len(exceptions), 2)
        self.assertIn("partial: deadline", exceptions[0]["cause"])

    def test_the_owned_transcript_names_changed_files_and_unread_directories(
        self,
    ) -> None:
        names = bench_hook.changed_paths(str(self.repo))
        self.assertIn("bulk/", names)
        transcript = self.root / "t.jsonl"
        bench_hook.write_transcript(transcript, str(self.repo))
        written = [json.loads(line) for line in transcript.read_text().splitlines()]
        targets = [
            c["input"]["file_path"]
            for w in written
            for c in (w["message"]["content"] if isinstance(w["message"]["content"], list) else [])
        ]
        self.assertTrue(any(t.endswith("bulk/.bench") or "/bulk/" in t for t in targets))

    def test_percentiles(self) -> None:
        self.assertEqual(bench_hook.percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.5), 5)
        self.assertEqual(bench_hook.percentile([1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 0.95), 10)

    def test_window_takes_the_last_events_after_excluding_a_session(self) -> None:
        entries, _ = bench_hook.parse_log([self.log([self.line(f"s{i}", "inspect", "-", "/x") for i in range(1, 12)])])
        kept, skipped = bench_hook.window(entries, {"s9"}, 3, False)
        self.assertEqual([e.session for e in kept], ["s8", "s10", "s11"])
        self.assertEqual(skipped["excluded-session"], 1)


if __name__ == "__main__":
    unittest.main()
