// Boundedness by construction (#510): every loop over the repository ends at
// the run's deadline because it iterates `RunContext::bounded`, not because
// someone remembered to poll.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

static BIN: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    std::env::var("DIFFCTX_NATIVE_BIN")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_diffctx").to_string())
});

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

const CALLERS: usize = 12;

/// `CALLERS` modules importing `pkg/core.py`, whose last commit changes it,
/// beside files no walk may admit: data, an ignored module, a module
/// `.diffctx/ignore` withholds.
fn repo_with_a_universe() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    write(root, "pkg/core.py", "def compute(x):\n    return x + 1\n");
    for i in 0..CALLERS {
        write(
            root,
            &format!("pkg/mod_{i}.py"),
            &format!("from pkg.core import compute\n\n\ndef use_{i}():\n    return compute({i})\n"),
        );
    }
    write(root, "data/table.bin", "compute compute compute\n");
    write(root, ".git/info/exclude", "build/\n");
    write(
        root,
        "build/gen.py",
        "from pkg.core import compute\n\n\ndef generated():\n    return compute(0)\n",
    );
    write(root, ".diffctx/ignore", "vendor/\n");
    write(
        root,
        "vendor/lib.py",
        "from pkg.core import compute\n\n\ndef vendored():\n    return compute(1)\n",
    );
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    write(root, "pkg/core.py", "def compute(x):\n    return x + 2\n");
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "change"]);
    tmp
}

fn run(repo: &Path, args: &[&str], polls: Option<u64>) -> serde_json::Value {
    let mut cmd = Command::new(&*BIN);
    cmd.current_dir(repo)
        .args(args)
        .args(["-q", "--timeout", "600"]);
    if let Some(n) = polls {
        cmd.env("DIFFCTX_TEST_DEADLINE_AFTER_POLLS", n.to_string());
    }
    let out = cmd.output().expect("run diffctx");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("json")
}

fn limits(doc: &serde_json::Value) -> Vec<String> {
    doc["coverage"]["limit_reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_string))
        .collect()
}

fn paths(doc: &serde_json::Value, key: &str) -> Vec<String> {
    let mut out: Vec<String> = doc[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| f["path"].as_str().map(str::to_string))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// #510: stopped at its `n`th deadline poll, a run has looked at no more
/// candidate files than it had polls, and a universe it did not finish is
/// named `discovery_truncated`. The walk, the file cache and every discovery
/// strategy iterate `bounded`; before, they ran to the end of the
/// repository whatever the deadline said, and a run stopped at its second
/// poll had still listed, stat'ed and read every file. Swept for every `n`
/// until the run answers in full, which must then be the whole answer: the
/// 12 callers and the change, none of the data, ignored or withheld files.
#[test]
fn a_pack_run_stopped_at_any_poll_has_read_no_candidate_past_it() {
    let tmp = repo_with_a_universe();
    let repo = tmp.path();
    let args = ["--diff", "HEAD~1..HEAD", "-f", "json"];
    let whole = run(repo, &args, None);
    let mut expected: Vec<String> = (0..CALLERS).map(|i| format!("pkg/mod_{i}.py")).collect();
    expected.push("pkg/core.py".to_string());
    expected.sort();
    assert_eq!(paths(&whole, "fragments"), expected, "{whole}");
    assert!(limits(&whole).is_empty(), "{whole}");
    let mut n: u64 = 0;
    loop {
        let doc = run(repo, &args, Some(n));
        let reasons = limits(&doc);
        if !reasons.iter().any(|r| r == "deadline") {
            assert_eq!(
                paths(&doc, "fragments"),
                expected,
                "poll {n}: a smaller answer that looks complete"
            );
            break;
        }
        let read = doc["coverage"]["resources"]["candidate_files"]
            .as_u64()
            .unwrap_or(0);
        assert!(
            read <= n,
            "poll {n}: {read} candidate files admitted after the deadline fired"
        );
        if read < CALLERS as u64 {
            assert!(
                reasons.iter().any(|r| r == "discovery_truncated"),
                "poll {n}: {read} of {CALLERS} candidates, and the universe reads as whole: {reasons:?}"
            );
        }
        assert!(
            paths(&doc, "fragments")
                .iter()
                .all(|p| expected.contains(p)),
            "poll {n}: a file outside the universe: {doc}"
        );
        assert!(n < 5_000, "the run never finished");
        n += 1;
    }
}

/// #510: the same sweep over locate, the other selection answer: stopped at
/// any poll it names the deadline, and unstopped it is the whole answer.
#[test]
fn a_locate_run_stopped_at_any_poll_names_the_deadline_or_answers_in_full() {
    let tmp = repo_with_a_universe();
    let repo = tmp.path();
    let args = ["--diff", "HEAD~1..HEAD", "--mode", "locate"];
    let whole = paths(&run(repo, &args, None), "items");
    assert_eq!(whole.len(), CALLERS + 1, "{whole:?}");
    let mut n: u64 = 0;
    loop {
        let doc = run(repo, &args, Some(n));
        let got = paths(&doc, "items");
        assert!(
            got.iter().all(|p| whole.contains(p)),
            "poll {n}: a file outside the universe: {got:?}"
        );
        if !limits(&doc).iter().any(|r| r == "deadline") {
            assert_eq!(got, whole, "poll {n}: a smaller answer that looks complete");
            break;
        }
        assert!(n < 5_000, "the run never finished");
        n += 1;
    }
}

/// The regions whose loops walk the repository: a whole module up to its
/// tests, or one function of a larger one.
const BOUNDED_REGIONS: &[(&str, Option<&str>)] = &[
    ("src/candidate_files.rs", None),
    ("src/discovery.rs", None),
    ("src/token_corpus.rs", Some("build")),
    ("src/pipeline.rs", Some("build_file_cache")),
    ("src/impact.rs", Some("unanalysed_files")),
    ("src/impact.rs", Some("missed_namers")),
    ("src/impact.rs", Some("stale_references")),
    ("src/impact.rs", Some("fit_with")),
];

/// Loops in those regions over something smaller than the repository, each
/// with what bounds it. An entry no loop matches any more fails, so the list
/// cannot outlive the code it excuses.
const NOT_OVER_THE_REPOSITORY: &[(&str, &str, &str)] = &[
    (
        "src/candidate_files.rs",
        "for walk_root in walk_roots",
        "the scope's roots",
    ),
    (
        "src/candidate_files.rs",
        "files .into_par_iter()",
        "a path check per file the bounded walk admitted; it reads nothing",
    ),
    (
        "src/discovery.rs",
        "for f in rare_files",
        "rare_identifier_threshold files per identifier of the diff",
    ),
    (
        "src/discovery.rs",
        "for ident in &ctx.expansion_concepts",
        "the diff's identifiers",
    ),
    (
        "src/discovery.rs",
        "for (_ident, files) in &ident_to_files",
        "the diff's identifiers",
    ),
    (
        "src/discovery.rs",
        "for f in files",
        "rare_identifier_threshold files",
    ),
    (
        "src/discovery.rs",
        "for path in &ctx.changed_files",
        "the changed files",
    ),
    (
        "src/discovery.rs",
        "for f in &ctx.changed_files",
        "the changed files",
    ),
    (
        "src/discovery.rs",
        "for (stem, names) in &per_file",
        "the changed files",
    ),
    (
        "src/discovery.rs",
        "for name in names",
        "the names a changed file defines",
    ),
    (
        "src/discovery.rs",
        "for name in &named",
        "the changed files' names one document holds",
    ),
    (
        "src/discovery.rs",
        "for prefix in TEST_PREFIXES",
        "a constant",
    ),
    (
        "src/discovery.rs",
        "for suffix in TEST_SUFFIXES",
        "a constant",
    ),
    (
        "src/discovery.rs",
        "for suffix in crate::edges::structural::testing::JVM_TEST_SUFFIXES",
        "a constant",
    ),
    (
        "src/discovery.rs",
        "for t in query_set",
        "the diff's query terms",
    ),
    (
        "src/discovery.rs",
        "for term in &query_set",
        "the diff's query terms",
    ),
    (
        "src/discovery.rs",
        "for term in doc.term_counts.keys()",
        "one document's terms, inside the bounded document loop",
    ),
    (
        "src/discovery.rs",
        ".strategies .par_iter()",
        "the strategies",
    ),
    (
        "src/discovery.rs",
        "for (source, paths) in per_strategy",
        "the strategies",
    ),
    (
        "src/impact.rs",
        "for (target, _) in &targets",
        "the deleted and renamed paths",
    ),
    (
        "src/impact.rs",
        "while !fits(output)",
        "the answer, a stride a turn; past the deadline it must still fit",
    ),
    (
        "src/impact.rs",
        "while hi - lo > 1",
        "halves one stride of the answer",
    ),
];

/// The loop headers of `text`'s code: `for`, `while` and `loop` up to their
/// brace, and every rayon loop with the statement it starts.
fn loop_headers(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with("//"))
        .collect();
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let opens = line.starts_with("for ") || line.starts_with("while ") || *line == "loop {";
        if opens {
            let header: Vec<&str> = lines[i..]
                .iter()
                .copied()
                .take_while(|l| !l.ends_with('{'))
                .chain(lines[i..].iter().copied().find(|l| l.ends_with('{')))
                .collect();
            out.push(header.join(" "));
        } else if line.contains("par_iter") || line.contains("par_chunks") {
            let start = (0..=i)
                .rev()
                .find(|&j| j == 0 || lines[j - 1].ends_with(';') || lines[j - 1].ends_with('{'))
                .unwrap_or(i);
            out.push(lines[start..=i].join(" "));
        }
    }
    out
}

fn region(text: &str, function: Option<&str>) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let Some(name) = function else {
        let end = lines
            .iter()
            .position(|l| l.trim() == "#[cfg(test)]")
            .unwrap_or(lines.len());
        return lines[..end].join("\n");
    };
    let start = lines
        .iter()
        .position(|l| l.contains(&format!("fn {name}(")) || l.contains(&format!("fn {name}<")))
        .unwrap_or_else(|| panic!("fn {name} is gone"));
    let indent = lines[start].len() - lines[start].trim_start().len();
    let close = format!("{}}}", " ".repeat(indent));
    let end = (start..lines.len())
        .find(|&i| lines[i] == close)
        .unwrap_or_else(|| panic!("fn {name} has no end"));
    lines[start..=end].join("\n")
}

/// #510: no loop over the repository in the bounded regions iterates
/// outside `RunContext::bounded`: a new one is either bounded or listed
/// with what bounds it instead. A loop that needed a hand-placed poll ran
/// until it bit; this is the mechanical half of making that impossible.
#[test]
fn every_loop_over_the_repository_iterates_bounded() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut unbounded = Vec::new();
    let mut used = vec![false; NOT_OVER_THE_REPOSITORY.len()];
    for (file, function) in BOUNDED_REGIONS {
        let text = std::fs::read_to_string(root.join(file)).expect(file);
        for header in loop_headers(&region(&text, *function)) {
            if header.contains("bounded(") || header.contains("bounded_par(") {
                continue;
            }
            let excused = NOT_OVER_THE_REPOSITORY
                .iter()
                .position(|(f, loop_, _)| f == file && header.contains(loop_));
            match excused {
                Some(i) => used[i] = true,
                None => unbounded.push(format!("{file}: {header}")),
            }
        }
    }
    assert!(
        unbounded.is_empty(),
        "loops outside `bounded`: {unbounded:#?}"
    );
    let stale: Vec<_> = NOT_OVER_THE_REPOSITORY
        .iter()
        .zip(&used)
        .filter(|(_, used)| !**used)
        .map(|(entry, _)| entry)
        .collect();
    assert!(
        stale.is_empty(),
        "excused loops that no longer exist: {stale:#?}"
    );
}
