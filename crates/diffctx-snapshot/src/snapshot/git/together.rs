//! Read-only questions one git child answers for several askers. A run asks the
//! same repository a few related questions (which directory and top level, which
//! index entries, which files spell a word and in which case); each is a
//! process, and one process prints the answers of all of them. The question
//! asked first runs the combined command and the run's memo keeps every
//! question's answer, byte for byte what its own command prints. A refusal, or
//! an answer that is not exactly the shape expected, is nothing: the question
//! then runs as itself.

use std::path::Path;
use std::process::{Command, Output};

use super::{Asked, git_command};

/// One question's own arguments, and the stdout its command prints.
type Part = (Vec<String>, Vec<u8>);

pub(super) struct Together {
    pub(super) command: Command,
    pub(super) answers: Answers,
}

pub(super) struct Answers {
    root: String,
    subcommand: &'static str,
    split: Box<dyn Fn(&[u8]) -> Option<Vec<Part>>>,
}

impl Answers {
    /// The questions the combined command answered, from its output.
    pub(super) fn of(&self, combined: &Output) -> Option<Vec<(Asked, Output)>> {
        let parts = (self.split)(&combined.stdout)?;
        Some(
            parts
                .into_iter()
                .filter_map(|(arguments, stdout)| {
                    let mut cmd = git_command(Path::new(&self.root));
                    cmd.arg(self.subcommand).args(arguments);
                    let output = Output {
                        status: combined.status,
                        stdout,
                        stderr: Vec::new(),
                    };
                    Some((Asked::of(&cmd)?, output))
                })
                .collect(),
        )
    }
}

fn owned(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The combined command for `cmd`, when `cmd` is one of the questions that
/// have one, built by `git_command` with nothing else set.
pub(super) fn of(cmd: &Command) -> Option<Together> {
    let args: Vec<&str> = cmd.get_args().map(|a| a.to_str()).collect::<Option<_>>()?;
    let ["-C", root, subcommand, arguments @ ..] = args.as_slice() else {
        return None;
    };
    let plain = git_command(Path::new(root));
    if !cmd.get_envs().eq(plain.get_envs()) || cmd.get_current_dir().is_some() {
        return None;
    }
    let (subcommand, (combined, split)) = match *subcommand {
        "rev-parse" => ("rev-parse", rev_parse(arguments)?),
        "ls-files" => ("ls-files", ls_files(arguments)?),
        "grep" => ("grep", grep(arguments)?),
        _ => return None,
    };
    let mut command = plain;
    command.arg(subcommand).args(combined);
    Some(Together {
        command,
        answers: Answers {
            root: (*root).to_string(),
            subcommand,
            split,
        },
    })
}

type Rule = (Vec<String>, Box<dyn Fn(&[u8]) -> Option<Vec<Part>>>);

fn lines<const N: usize>(stdout: &[u8]) -> Option<[&str; N]> {
    let Ok(text) = std::str::from_utf8(stdout) else {
        return None;
    };
    let Ok(lines) = <[&str; N]>::try_from(text.split_terminator('\n').collect::<Vec<_>>()) else {
        return None;
    };
    Some(lines)
}

/// `--git-dir`, `--show-toplevel`, the common directory with its bare flag and
/// the tree and commit of `HEAD` are six lines of one command; a bare repository
/// refuses the top level, a repository with no commit `HEAD`, and the questions
/// then run as themselves. A tree and a commit of one revision are two lines of
/// one command; a revision that is neither refuses.
fn rev_parse(own: &[&str]) -> Option<Rule> {
    const COMMON: [&str; 3] = [
        "--path-format=absolute",
        "--git-common-dir",
        "--is-bare-repository",
    ];
    match own {
        ["--git-dir"] | ["--show-toplevel"] | ["--path-format=absolute", ..] => {
            if own != ["--git-dir"] && own != ["--show-toplevel"] && own != COMMON.as_slice() {
                return None;
            }
            let combined = owned(&[
                "--git-dir",
                "--show-toplevel",
                COMMON[0],
                COMMON[1],
                COMMON[2],
                "HEAD^{tree}",
                "HEAD^{commit}",
            ]);
            Some((
                combined,
                Box::new(|stdout: &[u8]| {
                    let [dir, top, common, bare, tree, commit] = lines(stdout)?;
                    let mut parts = vec![
                        (owned(&["--git-dir"]), format!("{dir}\n").into_bytes()),
                        (owned(&["--show-toplevel"]), format!("{top}\n").into_bytes()),
                        (owned(&COMMON), format!("{common}\n{bare}\n").into_bytes()),
                    ];
                    parts.extend(commit_pair("HEAD", tree, commit)?);
                    Some(parts)
                }),
            ))
        }
        ["--verify", "--quiet", spec] => {
            let base = spec
                .strip_suffix("^{tree}")
                .or_else(|| spec.strip_suffix("^{commit}"))
                .filter(|base| !base.is_empty() && !base.starts_with('-'))?
                .to_string();
            let combined = vec![format!("{base}^{{tree}}"), format!("{base}^{{commit}}")];
            Some((
                combined,
                Box::new(move |stdout: &[u8]| {
                    let [tree, commit] = lines(stdout)?;
                    commit_pair(&base, tree, commit)
                }),
            ))
        }
        _ => None,
    }
}

/// The answers about `base`, from the oids of its tree and of its commit.
fn commit_pair(base: &str, tree: &str, commit: &str) -> Option<Vec<Part>> {
    let is_oid = |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if !is_oid(tree) || !is_oid(commit) {
        return None;
    }
    let verify = |spec: String| owned(&["--verify", "--quiet", &spec]);
    let answer = |oid: &str| format!("{oid}\n").into_bytes();
    // The commit's own spelling is asked about as well, by the askers that
    // took the full name from the answer.
    Some(vec![
        (verify(format!("{base}^{{tree}}")), answer(tree)),
        (verify(format!("{base}^{{commit}}")), answer(commit)),
        (verify(format!("{commit}^{{tree}}")), answer(tree)),
        (verify(format!("{commit}^{{commit}}")), answer(commit)),
    ])
}

/// The untracked listing, the tags of the index entries, the `.diffctx/ignore`
/// files and the staged entries are four views of one listing: `-t -s` prints
/// every index entry as its tag and its staged fields, `--others` the rest as
/// `?` records, and each view is those records filtered.
const VIEWS: [&[&str]; 4] = [
    &["-z", "--others", "--exclude-standard", "--directory"],
    &["-z", "-t", "--cached"],
    &["-z", "--cached", "--", ":(glob)**/.diffctx/ignore"],
    &["-s", "-z"],
];

fn ls_files(own: &[&str]) -> Option<Rule> {
    if !VIEWS.contains(&own) {
        return None;
    }
    let combined = owned(&[
        "-z",
        "-t",
        "-s",
        "--cached",
        "--others",
        "--exclude-standard",
        "--directory",
    ]);
    Some((
        combined,
        Box::new(|stdout: &[u8]| {
            let mut untracked: Vec<u8> = Vec::new();
            let mut tags: Vec<u8> = Vec::new();
            let mut policy: Vec<u8> = Vec::new();
            let mut staged: Vec<u8> = Vec::new();
            for record in stdout.split(|b| *b == 0).filter(|r| !r.is_empty()) {
                let [tag, b' ', rest @ ..] = record else {
                    return None;
                };
                let record_end = |out: &mut Vec<u8>, parts: &[&[u8]]| {
                    for part in parts {
                        out.extend_from_slice(part);
                    }
                    out.push(0);
                };
                if *tag == b'?' {
                    record_end(&mut untracked, &[rest]);
                    continue;
                }
                let tab = rest.iter().position(|b| *b == b'\t')?;
                let path = &rest[tab + 1..];
                record_end(&mut tags, &[&[*tag, b' '], path]);
                record_end(&mut staged, &[rest]);
                if path == b".diffctx/ignore" || path.ends_with(b"/.diffctx/ignore") {
                    record_end(&mut policy, &[path]);
                }
            }
            Some(
                VIEWS
                    .iter()
                    .zip([untracked, tags, policy, staged])
                    .map(|(view, stdout)| (owned(view), stdout))
                    .collect(),
            )
        }),
    ))
}

/// The files with a whole-word match and those with one in any case are the
/// same search: its lines, and the ones that match with the case kept.
fn grep(own: &[&str]) -> Option<Rule> {
    let ["--untracked", "-l", "-z", "-I", "-w", "-F", rest @ ..] = own else {
        return None;
    };
    let rest = rest.strip_prefix(&["-i"]).unwrap_or(rest);
    let ["-e", word, tail @ ..] = rest else {
        return None;
    };
    if !tail.is_empty() && tail[0] != "--" {
        return None;
    }
    if word.is_empty() || !word.is_ascii() || word.contains('\n') {
        return None;
    }
    let tail: Vec<String> = tail.iter().map(|t| (*t).to_string()).collect();
    let word = (*word).to_string();
    let search = |extra: &[&str], spelled: &str| {
        let mut args = owned(&["--untracked"]);
        args.extend(owned(extra));
        args.extend(owned(&["-e", spelled]));
        args.extend(tail.iter().cloned());
        args
    };
    let combined = search(&["-n", "-z", "-I", "-w", "-F", "-i"], &word);
    let any_case = search(&["-l", "-z", "-I", "-w", "-F", "-i"], &word);
    let exact = search(&["-l", "-z", "-I", "-w", "-F"], &word);
    Some((
        combined,
        Box::new(move |stdout: &[u8]| {
            let mut any: Vec<u8> = Vec::new();
            let mut kept: Vec<u8> = Vec::new();
            let mut last: Option<&[u8]> = None;
            let mut last_kept = false;
            for line in stdout.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
                let mut fields = line.splitn(3, |b| *b == 0);
                let (path, number, text) = (fields.next()?, fields.next()?, fields.next()?);
                if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
                    return None;
                }
                if last != Some(path) {
                    any.extend_from_slice(path);
                    any.push(0);
                    last = Some(path);
                    last_kept = false;
                }
                if !last_kept && spells_word(text, word.as_bytes()) {
                    kept.extend_from_slice(path);
                    kept.push(0);
                    last_kept = true;
                }
            }
            Some(vec![(exact.clone(), kept), (any_case.clone(), any)])
        }),
    ))
}

/// `git grep -w -F`: an occurrence of `word` with no word character before it
/// and none after it, searched from every offset of the line.
fn spells_word(text: &[u8], word: &[u8]) -> bool {
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if word.len() > text.len() {
        return false;
    }
    (0..=text.len() - word.len()).any(|at| {
        text[at..at + word.len()] == *word
            && (at == 0 || !is_word(text[at - 1]))
            && text.get(at + word.len()).is_none_or(|b| !is_word(*b))
    })
}
