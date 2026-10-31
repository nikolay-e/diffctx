//! One strategy per family of lines. A family holds the lines one property
//! speaks about: the baseline in `hook_property.rs` says per family whether
//! every such line is reviewed (green) or the hook is known to miss some
//! (red), and a red family is built so that every line in it shows the miss.

use proptest::prelude::*;

use super::line::{
    Cwd, Force, Hidden, Join, Line, Message, Op, Part, Peer, PushForm, Reach, RebaseForm, Repo,
    Stage, Text, Wrapper,
};

fn repos() -> BoxedStrategy<Repo> {
    prop_oneof![Just(Repo::A), Just(Repo::B)].boxed()
}

fn cwds() -> BoxedStrategy<Cwd> {
    prop_oneof![
        Just(Cwd::In(Repo::A)),
        Just(Cwd::In(Repo::B)),
        Just(Cwd::Neutral)
    ]
    .boxed()
}

fn texts() -> BoxedStrategy<Text> {
    prop_oneof![
        Just(Text::Plain),
        Just(Text::Quotes),
        Just(Text::Apostrophe),
        Just(Text::Verbs),
        Just(Text::Substitutions)
    ]
    .boxed()
}

fn messages() -> BoxedStrategy<Message> {
    prop_oneof![
        texts().prop_map(Message::Plain),
        texts().prop_map(Message::Stdin),
        texts().prop_map(Message::Substituted),
        Just(Message::Reuse)
    ]
    .boxed()
}

fn stages() -> BoxedStrategy<Stage> {
    prop_oneof![
        Just(Stage::Add),
        Just(Stage::AddAll),
        Just(Stage::CommitAll),
        Just(Stage::Pathspec),
        Just(Stage::Loop),
        Just(Stage::Xargs)
    ]
    .boxed()
}

/// The ways a line reaches a repository whose directory the line names.
fn named_reaches() -> BoxedStrategy<Reach> {
    prop_oneof![
        Just(Reach::Here),
        Just(Reach::Cd),
        Just(Reach::DashC),
        Just(Reach::Subshell),
        Just(Reach::Variable)
    ]
    .boxed()
}

fn git_dir_reaches() -> BoxedStrategy<Reach> {
    prop_oneof![Just(Reach::GitDirFlag), Just(Reach::GitDirEnv)].boxed()
}

fn joins() -> BoxedStrategy<Join> {
    prop_oneof![Just(Join::And), Just(Join::Semi), Just(Join::Or)].boxed()
}

fn pushes() -> BoxedStrategy<PushForm> {
    prop_oneof![
        Just(PushForm::Bare),
        Just(PushForm::Named),
        Just(PushForm::Head),
        Just(PushForm::Refspec),
        Just(PushForm::Upstream),
        Just(PushForm::Quiet),
        Just(PushForm::Piped)
    ]
    .boxed()
}

fn forces() -> BoxedStrategy<Force> {
    prop_oneof![
        Just(Force::Short),
        Just(Force::Long),
        Just(Force::Lease),
        Just(Force::Plus)
    ]
    .boxed()
}

fn commits() -> BoxedStrategy<Op> {
    (stages(), messages(), any::<bool>(), any::<bool>())
        .prop_map(|(stage, message, amend, alias)| Op::Commit {
            stage,
            message,
            amend,
            alias,
        })
        .boxed()
}

fn part(op: BoxedStrategy<Op>, reach: BoxedStrategy<Reach>) -> BoxedStrategy<Part> {
    (repos(), reach, any::<bool>(), op)
        .prop_map(|(repo, reach, env_prefix, op)| Part {
            repo,
            reach,
            env_prefix,
            op,
        })
        .boxed()
}

/// A line of one repository's parts in order: the same repository and reach
/// for each of `ops`.
fn sequence(
    reach: BoxedStrategy<Reach>,
    ops: BoxedStrategy<Vec<Op>>,
    cwd: BoxedStrategy<Cwd>,
) -> BoxedStrategy<Line> {
    (repos(), reach, any::<bool>(), ops, cwd, joins())
        .prop_map(|(repo, reach, env_prefix, ops, cwd, join)| {
            let parts = ops
                .into_iter()
                .map(|op| Part {
                    repo,
                    reach,
                    env_prefix,
                    op,
                })
                .collect();
            Line::new(cwd, parts, vec![join])
        })
        .boxed()
}

fn one(op: Op) -> BoxedStrategy<Vec<Op>> {
    Just(vec![op]).boxed()
}

fn commit_then_maybe_push() -> BoxedStrategy<Vec<Op>> {
    (commits(), pushes(), any::<bool>())
        .prop_map(|(commit, push, with_push)| {
            let mut ops = vec![commit];
            if with_push {
                ops.push(Op::Push(push));
            }
            ops
        })
        .boxed()
}

/// Commits in every spelling of staging, message, flag and way to the
/// repository, one or two to a line.
pub fn commit_forms() -> BoxedStrategy<Line> {
    (
        cwds(),
        prop::collection::vec(part(commits(), named_reaches()), 1..=2),
        prop::collection::vec(joins(), 1..=2),
    )
        .prop_map(|(cwd, parts, joins)| Line::new(cwd, parts, joins))
        .boxed()
}

/// Both repositories in one line: two parts, or a loop over literal paths.
pub fn two_repositories() -> BoxedStrategy<Line> {
    let pair = (
        cwds(),
        any::<bool>(),
        named_reaches(),
        named_reaches(),
        commits(),
        prop_oneof![commits(), pushes().prop_map(Op::Push)],
        prop_oneof![
            Just(Join::And),
            Just(Join::Semi),
            Just(Join::Or),
            Just(Join::Amp)
        ],
    )
        .prop_map(
            |(cwd, swap, first_reach, second_reach, first, second, join)| {
                let (one, other) = if swap {
                    (Repo::B, Repo::A)
                } else {
                    (Repo::A, Repo::B)
                };
                let parts = vec![
                    Part::new(one, first_reach, first),
                    Part::new(other, second_reach, second),
                ];
                Line::new(cwd, parts, vec![join])
            },
        )
        .boxed();
    let looped = Just(Line::new(
        Cwd::Neutral,
        vec![Part::new(Repo::A, Reach::Here, Op::LoopBoth)],
        vec![Join::And],
    ));
    prop_oneof![3 => pair, 1 => looped].boxed()
}

/// A commit, then a push in each form, with a fetch before and another
/// checkout pushing to the remote's other branch meanwhile.
pub fn push_forms() -> BoxedStrategy<Line> {
    (
        cwds(),
        repos(),
        named_reaches(),
        commits(),
        pushes(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(
            |(cwd, repo, reach, commit, push, with_commit, with_fetch, peer_pushes)| {
                let mut ops = Vec::new();
                if with_fetch {
                    ops.push(Op::Fetch);
                }
                if with_commit {
                    ops.push(commit);
                }
                ops.push(Op::Push(push));
                let parts = ops
                    .into_iter()
                    .map(|op| Part::new(repo, reach, op))
                    .collect();
                let line = Line::new(cwd, parts, vec![Join::And]);
                if peer_pushes {
                    line.with_peer(Peer::PushBefore(repo))
                } else {
                    line
                }
            },
        )
        .boxed()
}

/// What a merge, a pick, a revert and `git am` record, sometimes pushed.
pub fn integration_verbs() -> BoxedStrategy<Line> {
    let verbs = prop_oneof![
        Just(Op::CherryPick),
        Just(Op::Revert),
        Just(Op::MergeNoFf),
        Just(Op::Am)
    ];
    let ops = (verbs, any::<bool>())
        .prop_map(|(verb, push)| {
            let mut ops = vec![verb];
            if push {
                ops.push(Op::Push(PushForm::Named));
            }
            ops
        })
        .boxed();
    sequence(named_reaches(), ops, cwds())
}

pub fn background_lines() -> BoxedStrategy<Line> {
    sequence(named_reaches(), commit_then_maybe_push(), cwds())
        .prop_map(Line::in_background)
        .boxed()
}

pub fn failing_lines() -> BoxedStrategy<Line> {
    sequence(named_reaches(), commit_then_maybe_push(), cwds())
        .prop_map(Line::failing)
        .boxed()
}

/// A git only the shell runs: `bash -c`, `eval`, a substitution.
pub fn hidden_git() -> BoxedStrategy<Line> {
    let forms = prop_oneof![
        Just(Hidden::BashC),
        Just(Hidden::Eval),
        Just(Hidden::Substitution)
    ];
    (cwds(), repos(), forms)
        .prop_map(|(cwd, repo, form)| {
            Line::new(
                cwd,
                vec![Part::new(repo, Reach::Here, Op::Hidden(form))],
                vec![Join::And],
            )
        })
        .boxed()
}

/// A commit in a repository the line names only by `--git-dir` or
/// `GIT_DIR`, from a directory that is not that repository.
pub fn git_dir_elsewhere() -> BoxedStrategy<Line> {
    (cwds(), part(commits(), git_dir_reaches()))
        .prop_map(|(cwd, part)| {
            let cwd = if cwd == Cwd::In(part.repo) {
                Cwd::Neutral
            } else {
                cwd
            };
            Line::new(cwd, vec![part], vec![Join::And])
        })
        .boxed()
}

/// A rebase records new commits on the branch it rebases.
pub fn rebases() -> BoxedStrategy<Line> {
    let forms = prop_oneof![
        Just(RebaseForm::Switch),
        Just(RebaseForm::Checkout),
        Just(RebaseForm::Autostash)
    ];
    sequence(
        named_reaches(),
        forms.prop_map(|f| vec![Op::Rebase(f)]).boxed(),
        cwds(),
    )
}

/// A script that commits: the line itself shows no git.
pub fn scripts() -> BoxedStrategy<Line> {
    let wrappers = prop_oneof![Just(Wrapper::Bash), Just(Wrapper::Exec)];
    (cwds(), repos(), wrappers)
        .prop_map(|(cwd, repo, wrapper)| {
            Line::new(
                cwd,
                vec![Part::new(repo, Reach::Here, Op::Script(wrapper))],
                vec![Join::And],
            )
        })
        .boxed()
}

/// A commit that changes no file.
pub fn empty_commits() -> BoxedStrategy<Line> {
    let ops = any::<bool>()
        .prop_map(|push| {
            let mut ops = vec![Op::Empty];
            if push {
                ops.push(Op::Push(PushForm::Named));
            }
            ops
        })
        .boxed();
    sequence(named_reaches(), ops, cwds())
}

/// Amend the tip the remote has, then force it.
pub fn force_pushes() -> BoxedStrategy<Line> {
    sequence(
        named_reaches(),
        forces().prop_map(|f| vec![Op::ForcePush(f)]).boxed(),
        cwds(),
    )
}

/// A new branch on top of one the remote has, committed and pushed.
pub fn new_branches() -> BoxedStrategy<Line> {
    sequence(named_reaches(), one(Op::ChildOfPushed), cwds())
}

/// Another session committed in the checkout and did not push; the line
/// commits and pushes both.
pub fn pushes_over_a_peer_commit() -> BoxedStrategy<Line> {
    (cwds(), repos(), named_reaches(), commits())
        .prop_map(|(cwd, repo, reach, commit)| {
            let parts = vec![
                Part::new(repo, reach, commit),
                Part::new(repo, reach, Op::Push(PushForm::Named)),
            ];
            Line::new(cwd, parts, vec![Join::And]).with_peer(Peer::CommitBefore(repo))
        })
        .boxed()
}

/// Another session commits in the checkout after the line ended and before
/// its `PostToolUse`, whether the line committed or not.
pub fn peer_commits_in_the_window() -> BoxedStrategy<Line> {
    let ops = prop_oneof![commits().prop_map(|c| vec![c]), one(Op::Nothing)].boxed();
    sequence(named_reaches(), ops, cwds())
        .prop_map(|line| {
            let repo = line.parts[0].repo;
            line.with_peer(Peer::CommitAfter(repo))
        })
        .boxed()
}
