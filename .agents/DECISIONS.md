# Decisions

## 2026-10-21 — Text says where to look; git says what landed (#508)

**Decision.** A line's `PreToolUse` records the refs of each repository
that one of its statements may record in, unless the verb is known to land
nothing (status, diff, log, fetch, pull, rebase, …). An alias, `am`, a
verb the hook does not know, and a git only the shell runs (in the cwd's
repository) all count. Each repository records what its statements can
move: `HEAD` for commit-like verbs, remote refs for a push, either for an
unknown verb. `PostToolUse` reviews every repository whose reflog shows a
movement one of those statements could have made. A line whose
`PostToolUse` comes at a background launch keeps its record, per session,
for the next hook event that finds it landed.

**Reason.** The 10-14 decision moved "what landed" onto git's record, but
three choices stayed on the command text. Whether to record at all came
from a five-verb list, so `am`, aliases and shell-run git were missed.
Which repositories to watch was the cwd's always, plus named ones, and only
the first that moved was reviewed. Whose movement it was was never asked.
The field paid for each choice: 57 background commits, an `am` landing of
ten commits and two-repository lines went unreviewed, and other sessions'
pushes and pulls were reviewed as this line's (#607, #608, #617, #618, #556).

**Rejected.** Recording before every git line, read-only verbs included:
another session's commit in a shared worktree would then be claimed by a
`git status`, which #575 already ruled out. Waiting at `PostToolUse` until a
background line lands: the hook's deadline is seconds, and a pre-commit
runs minutes.

**Consequences.** A pending record lives two hours. A foreground line that
reviews the same repository drops it, so the record never claims that
commit. Within one repository, a push of several ranges still reviews the
first and counts the rest.

## 2026-10-14 — The hook reviews what git recorded (#508)

**Decision.** The plugin hook answers a commit, merge, cherry-pick, revert
or push in `PostToolUse`, from git's own record: `PreToolUse` stores the refs
of each repository the line names, and once the line ran the hook reads the
range the reflog shows moved (`HEAD`'s last entry; a remote-tracking ref's
"update by push"). Nothing about what a line will stage is parsed any more.
The opt-in strict gate is the one review before git runs, and it reviews only
what needs no prediction: the index a plain `git commit` records. A commit
line that stages for itself (an add on the same line, paths, `-a`, `-i`,
`--pathspec-from-file`) is denied with the instruction to stage as its own
command.

**Reason.** The planner replayed a line's staging against a copy of the
index, read from the command text. Fifteen parser commits in nine days
(members #405, #422, #429, #436 and #501 among them) did not close the
class, and `--pathspec-from-file` reviewed another session's staged file
instead of the paths the commit took. Claude Code delivers a `PreToolUse`
note together with the tool's result anyway (#429), so the preview bought no
earlier reading, only a chance to be wrong.

**Rejected.** A git `pre-commit` hook in the reviewed repository: it would
see the exact index, but installing hooks into users' repositories is out of
the plugin's scope and the agent never reads its output. Keeping the planner
and patching each shape: the corpus shows every patch followed by a sibling.

**Consequences.** Without the gate, the impact arrives after the commit
landed: a caller it names is fixed in a follow-up commit, not before. A push
of several refs reviews the first and counts the rest. A line whose shell
cannot run it (bash 3.2 on a heredoc inside `$(…)`) records nothing and gets
nothing. The command parser remains for the gate, for `cd`/`-C` resolution
and for inspections and searches, where no record exists to read.
