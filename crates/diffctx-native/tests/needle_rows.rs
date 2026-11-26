//! The rows a needle search returns are the lines that hold the needle, whatever
//! characters it is spelled with: a plain scan of the files is the oracle, at a
//! revision and in the working tree, over more needles than one search takes.
//! A git built without PCRE refuses `-P`; its rows are the same.

use std::path::Path;
use std::process::Command;

use diffctx_snapshot::snapshot::{Row, needle_lines, word_lines};
use tempfile::TempDir;

const MAIN: &str = "fn {code_head()} {\naxb a.b\nxa.b a.bc\ndecoys x || y back/slash var i\na x|y b\n\
back\\slash $var [i]\ncall(arg\ncall_me callable recall\n(call)\n^start *star 1+1\nwhy? # note\n\
--  -leading -x\ncaf\u{e9} caf\u{e9}x\n(?i) \\d \\Qx\\E\na & b ~ c\n{ }\n\nplain text\ncall\ncall\n";
const FILLER: &str = "needle_7\nneedle_70 needle_1x\nneedle_3\nneedle_\nneedle\nneedle_119 needle_0\n\
needle_55\nneedle_105 needle_3\nneedle_3 again\n";

const COMMITTED: [(&str, &str); 6] = [
    ("src/main.txt", MAIN),
    ("sp ace/n\u{e9}.txt", "a x|y b\ncall\n"),
    ("notes.md", "a.b in notes\ncall\n"),
    ("filler.txt", FILLER),
    (".gitignore", "ignored.txt\n"),
    ("pairs.txt", "x = foobar(1)\ny = foo.bar\nfoobarbaz afoo\n"),
];
const MAIN_NOW: &str = "fn {code_head()} {\nxa.b a.b again\ncall(more)\n";
const UNTRACKED: (&str, &str) = ("new/file.txt", "(open\ncall\n");

/// Needles that are prefixes of one another, in both orders: a whole-word
/// search that stops at the first alternative matching a place misses `foobar`
/// after `foo` and `foo.bar` after `foo.b`.
const PREFIXES: [&str; 5] = ["foo.b", "foo", "foobar", "foo", "foo.b"];

const AWKWARD: [&str; 20] = [
    "{code_head()}",
    "a.b",
    "x|y",
    "back\\slash",
    "$var",
    "[i]",
    "(",
    "^",
    "*",
    "+",
    "?",
    "#",
    "-leading",
    "caf\u{e9}",
    "&",
    "~",
    "(?i)",
    "\\d",
    "\\Qx\\E",
    "",
];

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "user.name=n", "-c", "user.email=n@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(repo: &Path, rel: &str, bytes: &[u8]) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("dir");
    std::fs::write(path, bytes).expect("write");
}

/// What `HEAD` holds and what the working tree holds after it: one tracked file
/// edited, one untracked, one ignored; a binary file holds needles in both.
fn repository() -> (TempDir, Vec<(String, String)>, Vec<(String, String)>) {
    let dir = TempDir::new().expect("tempdir");
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    for (path, text) in COMMITTED {
        write(repo, path, text.as_bytes());
    }
    write(repo, "bin.dat", b"a.b\0\ncall\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "one"]);
    write(repo, "src/main.txt", MAIN_NOW.as_bytes());
    write(repo, UNTRACKED.0, UNTRACKED.1.as_bytes());
    write(repo, "ignored.txt", b"a.b\ncall\n");
    let owned = |files: &[(&str, &str)]| {
        files
            .iter()
            .map(|(path, text)| (path.to_string(), text.to_string()))
            .collect::<Vec<_>>()
    };
    let now: Vec<_> = COMMITTED
        .iter()
        .map(|&(path, text)| {
            if path == "src/main.txt" {
                (path, MAIN_NOW)
            } else {
                (path, text)
            }
        })
        .chain([UNTRACKED])
        .collect();
    (dir, owned(&COMMITTED), owned(&now))
}

fn scan(files: &[(String, String)], keep: impl Fn(&str, &str) -> Vec<usize>) -> Vec<Row> {
    let mut rows = Vec::new();
    for (path, text) in files {
        let lines: Vec<&str> = text.lines().collect();
        for no in keep(path, text) {
            rows.push((path.clone(), no as u32 + 1, lines[no].to_string()));
        }
    }
    rows.sort_unstable();
    rows
}

fn needles() -> Vec<String> {
    let filler = |range: std::ops::Range<u32>| range.map(|i| format!("needle_{i}"));
    PREFIXES
        .map(String::from)
        .into_iter()
        .chain(filler(0..60))
        .chain(AWKWARD.map(String::from))
        .chain(filler(60..120))
        .collect()
}

fn holds_whole_word(line: &str, word: &str) -> bool {
    let (line, word) = (line.as_bytes(), word.as_bytes());
    let in_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    (0..line.len()).any(|at| {
        line[at..].starts_with(word)
            && (at == 0 || !in_word(line[at - 1]))
            && !line.get(at + word.len()).is_some_and(|&b| in_word(b))
    })
}

fn lines_where(text: &str, hit: impl Fn(&str) -> bool) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| hit(line))
        .map(|(no, _)| no)
        .collect()
}

fn assert_rows_are_the_scan(repo: &Path, committed: &[(String, String)], now: &[(String, String)]) {
    let needles = needles();
    let holds = |line: &str| {
        needles
            .iter()
            .any(|n| !n.is_empty() && line.contains(n.as_str()))
    };
    for (rev, files) in [(Some("HEAD"), committed), (None, now)] {
        let expected = scan(files, |_, text| lines_where(text, holds));
        assert!(
            expected.len() > 10,
            "the fixture spells the needles: {expected:?}"
        );
        let found = needle_lines(repo, &needles, rev, &|| true).expect("search");
        assert_eq!(found, expected, "needle rows at {rev:?}");
    }

    let words = needles;
    let whole = |line: &str| {
        words
            .iter()
            .any(|w| !w.is_empty() && holds_whole_word(line, w))
    };
    for ((rev, files), per_file) in [(Some("HEAD"), committed), (None, now)]
        .into_iter()
        .flat_map(|side| [(side, 2), (side, 1000)])
    {
        let expected = scan(files, |path, text| {
            if Path::new(path).extension().is_none_or(|ext| ext != "txt") {
                return Vec::new();
            }
            let mut hits = lines_where(text, whole);
            hits.truncate(per_file);
            hits
        });
        assert!(
            expected.iter().any(|(path, ..)| path == "filler.txt")
                && expected.iter().any(|(path, ..)| path == "src/main.txt"),
            "the fixture spells whole words: {expected:?}"
        );
        let pairs = expected.iter().filter(|(path, ..)| path == "pairs.txt");
        assert_eq!(
            pairs.count(),
            2,
            "foobar and foo.bar each hold a needle as a word"
        );
        let found = word_lines(repo, &words, rev, per_file, &["*.txt".to_string()], &|| {
            true
        })
        .expect("search");
        assert_eq!(
            found, expected,
            "whole-word rows at {rev:?}, {per_file} a file"
        );
    }
}

#[test]
fn needle_rows_are_the_lines_that_hold_them() {
    let (dir, committed, now) = repository();
    assert_rows_are_the_scan(dir.path(), &committed, &now);
}

/// The search runs again in a child whose `git` refuses `-P` as one built
/// without PCRE does: the process cannot change its own `PATH` safely.
#[cfg(unix)]
#[test]
fn a_git_without_pcre_gives_the_same_rows() {
    use std::os::unix::fs::PermissionsExt;

    if std::env::var_os("GIT_SHIM_LOG").is_some() {
        let (dir, committed, now) = repository();
        return assert_rows_are_the_scan(dir.path(), &committed, &now);
    }
    let shim = TempDir::new().expect("tempdir");
    let log = shim.path().join("refused");
    let script = shim.path().join("git");
    std::fs::write(
        &script,
        "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = -P ]; then\n    echo refused >> \"$GIT_SHIM_LOG\"\n    \
echo 'fatal: cannot use Perl-compatible regexes when not compiled with USE_LIBPCRE' >&2\n    exit 128\n  fi\n\
done\nPATH=\"$GIT_SHIM_REAL_PATH\"\nexec git \"$@\"\n",
    )
    .expect("shim");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let real_path = std::env::var("PATH").expect("PATH");
    let out = Command::new(std::env::current_exe().expect("this test binary"))
        .args(["--exact", "a_git_without_pcre_gives_the_same_rows"])
        .env("PATH", format!("{}:{real_path}", shim.path().display()))
        .env("GIT_SHIM_REAL_PATH", real_path)
        .env("GIT_SHIM_LOG", &log)
        .output()
        .expect("child runs");
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(log.exists(), "the shim was asked for -P");
}
