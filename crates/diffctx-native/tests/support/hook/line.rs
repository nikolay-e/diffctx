//! The grammar of agent command lines. A `Line` is a few `Part`s joined by
//! shell connectors; each part reaches one scratch repository in one of the
//! ways an agent does (cwd, `cd`, `-C`, a subshell, a variable, `--git-dir`,
//! `GIT_DIR`) and runs one operation. Rendering fixes the text only: what a
//! line recorded is read from git afterwards, never from this model.

use std::fmt::Write as _;

use super::world::World;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Repo {
    A,
    B,
}

impl Repo {
    pub const BOTH: [Self; 2] = [Self::A, Self::B];

    pub fn name(self) -> &'static str {
        match self {
            Self::A => "A",
            Self::B => "B",
        }
    }
}

/// Where the Bash tool starts the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cwd {
    In(Repo),
    Neutral,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// The directory the shell is in; becomes `Cd` when it is another one.
    Here,
    Cd,
    DashC,
    Subshell,
    Variable,
    GitDirFlag,
    GitDirEnv,
}

impl Reach {
    fn effective(self, repo: Repo, shell_in: Option<Repo>) -> Self {
        match self {
            Self::Here if shell_in != Some(repo) => Self::Cd,
            other => other,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Join {
    And,
    Semi,
    Or,
    Amp,
}

impl Join {
    fn text(self) -> &'static str {
        match self {
            Self::And => " && ",
            Self::Semi => "; ",
            Self::Or => " || ",
            Self::Amp => " & ",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Text {
    Plain,
    Quotes,
    Apostrophe,
    Verbs,
    Substitutions,
}

impl Text {
    fn body(self) -> &'static str {
        match self {
            Self::Plain => "adjust the rate",
            Self::Quotes => "the \"rate\" is 19%",
            Self::Apostrophe => "it's the rate",
            Self::Verbs => "fix: git push origin main && git merge topic; done",
            Self::Substitutions => "$(date) `id` $HOME",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Message {
    Plain(Text),
    /// `-F - <<'EOF'`, the body after the last line.
    Stdin(Text),
    /// `-m "$(cat <<'EOF' … EOF)"`, the shape Claude Code writes itself.
    Substituted(Text),
    Reuse,
}

#[derive(Clone, Copy, Debug)]
pub enum Stage {
    Add,
    AddAll,
    CommitAll,
    Pathspec,
    Loop,
    Xargs,
}

#[derive(Clone, Copy, Debug)]
pub enum PushForm {
    Bare,
    Named,
    Head,
    Refspec,
    Upstream,
    Quiet,
    Piped,
}

#[derive(Clone, Copy, Debug)]
pub enum Force {
    Short,
    Long,
    Lease,
    Plus,
}

#[derive(Clone, Copy, Debug)]
pub enum RebaseForm {
    Switch,
    Checkout,
    Autostash,
}

#[derive(Clone, Copy, Debug)]
pub enum Hidden {
    BashC,
    Eval,
    Substitution,
}

#[derive(Clone, Copy, Debug)]
pub enum Wrapper {
    Bash,
    Exec,
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Commit {
        stage: Stage,
        message: Message,
        amend: bool,
        alias: bool,
    },
    Empty,
    /// A commit of a clean tree: it fails and records nothing.
    Nothing,
    Push(PushForm),
    /// Amends the tip `main` already has on the remote, then forces it.
    ForcePush(Force),
    /// A new branch on top of one the remote already has, committed and pushed.
    ChildOfPushed,
    Fetch,
    Rebase(RebaseForm),
    CherryPick,
    Revert,
    MergeNoFf,
    Am,
    /// A committing script: the line shows no git.
    Script(Wrapper),
    /// Both repositories in a `for` loop over literal paths.
    LoopBoth,
    /// A git only the shell runs.
    Hidden(Hidden),
}

#[derive(Clone, Copy, Debug)]
pub struct Part {
    pub repo: Repo,
    pub reach: Reach,
    /// `GIT_AUTHOR_NAME=… git commit`.
    pub env_prefix: bool,
    pub op: Op,
}

/// Another process changing the same repository around the line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Peer {
    None,
    /// A commit in the checkout before the hook's `PreToolUse`.
    CommitBefore(Repo),
    /// A commit in the checkout after the line ended, before its `PostToolUse`.
    CommitAfter(Repo),
    /// A push to the remote's other branch, from a second clone, after the
    /// `PreToolUse`.
    PushBefore(Repo),
}

#[derive(Clone, Debug)]
pub struct Line {
    pub cwd: Cwd,
    pub parts: Vec<Part>,
    pub joins: Vec<Join>,
    /// Ends with `; false`: the line exits non-zero.
    pub fails: bool,
    /// Sent as `run_in_background`: the `PostToolUse` comes at launch.
    pub background: bool,
    pub peer: Peer,
}

impl Line {
    pub fn new(cwd: Cwd, parts: Vec<Part>, joins: Vec<Join>) -> Self {
        Self {
            cwd,
            parts,
            joins,
            fails: false,
            background: false,
            peer: Peer::None,
        }
    }

    pub fn failing(mut self) -> Self {
        self.fails = true;
        self
    }

    pub fn in_background(mut self) -> Self {
        self.background = true;
        self
    }

    pub fn with_peer(mut self, peer: Peer) -> Self {
        self.peer = peer;
        self
    }

    pub fn render(&self, world: &World) -> String {
        let mut shell_in = match self.cwd {
            Cwd::In(repo) => Some(repo),
            Cwd::Neutral => None,
        };
        let mut heredocs: Vec<(String, String)> = Vec::new();
        let mut out = String::new();
        for (i, part) in self.parts.iter().enumerate() {
            if i > 0 {
                let join = self.joins.get((i - 1) % self.joins.len().max(1));
                out.push_str(join.map_or(" && ", |j| j.text()));
            }
            out.push_str(&part.render(world, &mut shell_in, &mut heredocs, i + 1));
        }
        if self.fails {
            out.push_str("; false");
        }
        for (delimiter, body) in heredocs {
            let _ = write!(out, "\n{body}\n{delimiter}");
        }
        out
    }
}

/// Single quotes around `text`, however many it holds.
fn sq(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// What the statements of one part are built from.
struct Ctx {
    git: String,
    env: &'static str,
    path: String,
    patch: String,
    reach: Reach,
    /// The part's place in the line: each edit differs from the others.
    unit: usize,
}

impl Ctx {
    fn stage(&self, stage: Stage, amend: bool) -> Stage {
        match stage {
            Stage::AddAll if matches!(self.reach, Reach::GitDirFlag | Reach::GitDirEnv) => {
                Stage::Add
            }
            Stage::Xargs if self.reach == Reach::GitDirEnv => Stage::Add,
            Stage::Pathspec if amend => Stage::Add,
            other => other,
        }
    }

    fn message(&self, message: Message, heredocs: &mut Vec<(String, String)>) -> String {
        match message {
            Message::Plain(text) => format!(" -m {}", sq(text.body())),
            Message::Stdin(text) if self.reach != Reach::Subshell => {
                let delimiter = format!("EOF{}", heredocs.len());
                heredocs.push((delimiter.clone(), text.body().to_string()));
                format!(" -F - <<'{delimiter}'")
            }
            Message::Stdin(text) | Message::Substituted(text) => {
                // bash 3.2 reads an apostrophe in a here-document inside `$(` as
                // an open quote.
                let text = match text {
                    Text::Apostrophe => Text::Quotes,
                    other => other,
                };
                format!(" -m \"$(cat <<'EOF'\n{}\nEOF\n)\"", text.body())
            }
            Message::Reuse => " -C HEAD".to_string(),
        }
    }
}

impl Op {
    /// Operations that name their own repositories and ignore the reach.
    fn standalone(self, world: &World, repo: Repo, path: &str) -> Option<String> {
        let edit_in = |dir: &str| {
            format!("printf 'def total(items):\\n    return sum(items) + 7\\n' > {dir}/tracked.py")
        };
        match self {
            Self::Script(Wrapper::Bash) => Some(format!("bash {}", sq(&world.script(repo)))),
            Self::Script(Wrapper::Exec) => Some(sq(&world.script(repo))),
            Self::LoopBoth => {
                let paths: Vec<String> = Repo::BOTH
                    .iter()
                    .map(|r| sq(&world.path(*r).to_string_lossy()))
                    .collect();
                Some(format!(
                    "for d in {}; do ( cd \"$d\" && {} && git add -A && git commit -q -m 'loop' ); done",
                    paths.join(" "),
                    edit_in("\"$d\"")
                ))
            }
            Self::Hidden(form) => {
                let inner = format!(
                    "cd {path} && {} && git add -A && git commit -q -m hidden",
                    edit_in(path)
                );
                Some(match form {
                    Hidden::BashC => format!("bash -c \"{inner}\""),
                    Hidden::Eval => format!("eval \"{inner}\""),
                    Hidden::Substitution => format!("out=$({inner})"),
                })
            }
            _ => None,
        }
    }

    fn statements(self, c: &Ctx, heredocs: &mut Vec<(String, String)>) -> Vec<String> {
        let tracked = sq(&format!("{}/tracked.py", c.path));
        let edit = format!(
            "printf 'def total(items):\\n    return sum(items) + {}\\n' > {tracked}",
            c.unit
        );
        let add = format!("{} add {tracked}", c.git);
        let (env, git) = (c.env, &c.git);
        match self {
            Self::Commit {
                stage,
                message,
                amend,
                alias,
            } => {
                let verb = if alias { "ci" } else { "commit" };
                let mut out = vec![edit];
                let mut flags = String::new();
                match c.stage(stage, amend) {
                    Stage::Add => out.push(add),
                    Stage::AddAll => out.push(format!("{git} add -A")),
                    Stage::CommitAll => flags.push_str(" -a"),
                    Stage::Pathspec => {
                        let list = sq(&format!("{}/.pathspec", c.path));
                        out.push(format!("printf '%s\\n' {tracked} > {list}"));
                        let _ = write!(flags, " --pathspec-from-file={list}");
                    }
                    Stage::Loop => {
                        out.push(format!("for f in {tracked}; do {git} add \"$f\"; done"));
                    }
                    Stage::Xargs => out.push(format!("printf '%s\\n' {tracked} | xargs {git} add")),
                }
                let amend = if amend { " --amend" } else { "" };
                let message = c.message(message, heredocs);
                out.push(format!("{env}{git} {verb}{amend}{flags}{message}"));
                out
            }
            Self::Empty => vec![format!("{env}{git} commit -q --allow-empty -m 'empty'")],
            Self::Nothing => vec![format!("{env}{git} commit -q -m 'nothing'")],
            Self::Push(form) => vec![match form {
                PushForm::Bare => format!("{git} push"),
                PushForm::Named => format!("{git} push origin main"),
                PushForm::Head => format!("{git} push origin HEAD"),
                PushForm::Refspec => format!("{git} push origin HEAD:refs/heads/main"),
                PushForm::Upstream => format!("{git} push -u origin HEAD"),
                PushForm::Quiet => format!("{git} push -q origin main"),
                PushForm::Piped => format!("{git} push origin main 2>&1 | tail -n 3"),
            }],
            Self::ForcePush(force) => vec![
                edit,
                add,
                format!("{env}{git} commit -q --amend --no-edit"),
                match force {
                    Force::Short => format!("{git} push -f origin main"),
                    Force::Long => format!("{git} push --force origin main"),
                    Force::Lease => format!("{git} push --force-with-lease origin main"),
                    Force::Plus => format!("{git} push origin +main"),
                },
            ],
            Self::ChildOfPushed => vec![
                format!("{git} checkout -q -b child pushed"),
                edit,
                add,
                format!("{env}{git} commit -q -m child"),
                format!("{git} push origin child"),
            ],
            Self::Fetch => vec![format!("{git} fetch -q origin")],
            Self::Rebase(form) => match form {
                RebaseForm::Switch => vec![format!("{git} rebase -q main topic")],
                RebaseForm::Checkout => vec![
                    format!("{git} checkout -q topic"),
                    format!("{git} rebase -q main"),
                ],
                RebaseForm::Autostash => vec![
                    format!("{git} checkout -q topic"),
                    format!("{git} rebase -q --autostash main"),
                ],
            },
            Self::CherryPick => vec![format!("{env}{git} cherry-pick topic")],
            Self::Revert => vec![format!("{env}{git} revert --no-edit HEAD")],
            Self::MergeNoFf => vec![format!("{env}{git} merge -q --no-ff --no-edit topic")],
            Self::Am => vec![format!("{git} am -q {}", sq(&c.patch))],
            Self::Script(_) | Self::LoopBoth | Self::Hidden(_) => unreachable!("standalone"),
        }
    }
}

impl Part {
    pub fn new(repo: Repo, reach: Reach, op: Op) -> Self {
        Self {
            repo,
            reach,
            env_prefix: false,
            op,
        }
    }

    fn render(
        self,
        world: &World,
        shell_in: &mut Option<Repo>,
        heredocs: &mut Vec<(String, String)>,
        unit: usize,
    ) -> String {
        let path = world.path(self.repo).to_string_lossy().into_owned();
        if let Some(text) = self.op.standalone(world, self.repo, &path) {
            return text;
        }
        let quoted = sq(&path);
        let reach = self.reach.effective(self.repo, *shell_in);
        let git_dir = sq(&format!("{path}/.git"));
        let git = match reach {
            Reach::DashC => format!("git -C {quoted}"),
            Reach::GitDirFlag => format!("git --git-dir={git_dir} --work-tree={quoted}"),
            Reach::GitDirEnv => format!("GIT_DIR={git_dir} GIT_WORK_TREE={quoted} git"),
            _ => "git".to_string(),
        };
        let ctx = Ctx {
            git,
            env: if self.env_prefix {
                "GIT_AUTHOR_NAME='agent' GIT_COMMITTER_NAME='agent' "
            } else {
                ""
            },
            path,
            patch: world.patch(self.repo),
            reach,
            unit,
        };
        let steps = self.op.statements(&ctx, heredocs).join(" && ");
        match reach {
            Reach::Cd => {
                *shell_in = Some(self.repo);
                format!("cd {quoted} && {steps}")
            }
            Reach::Variable => {
                *shell_in = Some(self.repo);
                format!("R={quoted} && cd \"$R\" && {steps}")
            }
            Reach::Subshell => format!("( cd {quoted} && {steps} )"),
            _ => steps,
        }
    }
}
