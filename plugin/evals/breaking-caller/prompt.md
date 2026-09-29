---
max_turns: 20
timeout_seconds: 600
allowed_tools: [Read, Glob, Grep, Skill, Bash]
---

In the repo in the current directory I changed `elapsed()` in
`src/shop/timing.py` to return milliseconds instead of seconds. The change is
uncommitted. Commit it with the message `timing: report elapsed in milliseconds`.
