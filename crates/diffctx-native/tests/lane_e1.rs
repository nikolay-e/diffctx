// The native door of the one pack contract (#507): its JSON and YAML are
// rendered and fitted by the same `artifact::fit` the Python CLI and the MCP
// server call, measured on the text returned. The Python half of the gate,
// the doors compared, is tests/test_fit_contract.py.

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

/// A repository whose last commit applies `change` on top of `base`.
fn repo(base: &[(&str, String)], change: &[(&str, String)]) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    git(root, &["init", "-q", "."]);
    git(root, &["config", "user.email", "test@example.com"]);
    git(root, &["config", "user.name", "diffctx tests"]);
    for (step, files) in [base, change].into_iter().enumerate() {
        for (rel, content) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", &format!("step {step}")]);
    }
    tmp
}

fn pack(repo: &Path, budget: u32, format: &str) -> String {
    let out = Command::new(&*BIN)
        .current_dir(repo)
        .args(["--diff", "HEAD~1..HEAD", "-q", "-f", format])
        .args(["--budget", &budget.to_string()])
        .output()
        .expect("run diffctx");
    assert!(
        out.status.success(),
        "-f {format} --budget {budget}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8")
}

/// `(path, first line, changed)` per fragment.
fn fragments(text: &str, format: &str) -> Vec<(String, String, bool)> {
    let doc: serde_json::Value = if format == "json" {
        serde_json::from_str(text).unwrap()
    } else {
        serde_yaml::from_str(text).unwrap()
    };
    doc["fragments"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .map(|f| {
            let first = f["lines"].as_str().unwrap().split('-').next().unwrap();
            (
                f["path"].as_str().unwrap().to_string(),
                first.to_string(),
                f["role"] == "changed",
            )
        })
        .collect()
}

fn tier_repo() -> TempDir {
    let body: String = (0..40).map(|i| format!("    total += {i} * 0\n")).collect();
    let asserts: String = (0..25)
        .map(|i| format!("    assert compute({i}, 2) == {i}+2\n"))
        .collect();
    let docs: String = (0..20)
        .map(|i| format!("Line {i} of docs about compute.\n"))
        .collect();
    let test_head = "from zsrc.x import compute\n\n\ndef test_compute():\n";
    repo(
        &[
            ("zsrc/x.py", "def compute(a, b):\n    return a + b\n".into()),
            (
                "tests/test_x.py",
                format!("{test_head}    assert compute(1, 2) == 3\n"),
            ),
            ("README.md", "# Project\n\nCompute adds numbers.\n".into()),
        ],
        &[
            (
                "zsrc/x.py",
                format!("def compute(a, b):\n    total = a + b\n{body}    return total\n"),
            ),
            ("tests/test_x.py", format!("{test_head}{asserts}")),
            (
                "README.md",
                format!("# Project\n\nCompute adds numbers.\n\n{docs}"),
            ),
        ],
    )
}

/// #438's tier fixture: `zsrc/` sorts after `README.md` and `tests/`, and a
/// cut by position took the production file first. Every format fits its
/// own text, and production goes last, whatever the format.
#[test]
fn every_format_fits_its_own_text_and_production_goes_last() {
    let repo = tier_repo();
    for format in ["json", "yaml"] {
        for budget in [600, 700, 900, 1100, 1300] {
            let text = pack(repo.path(), budget, format);
            let tokens = _diffctx::tokenizer::count_tokens(&text);
            assert!(tokens <= budget, "-f {format} at {budget}: {tokens}");
            let shown: Vec<String> = fragments(&text, format)
                .into_iter()
                .map(|(path, _, _)| path)
                .collect();
            let has = |p: &str| shown.iter().any(|s| s == p);
            assert!(has("zsrc/x.py"), "-f {format} at {budget}: {shown:?}");
            assert!(
                !has("README.md") || has("tests/test_x.py"),
                "-f {format} at {budget}: docs outlasted tests: {shown:?}"
            );
        }
    }
}

/// The control: a budget nothing binds at delivers every changed hunk whole
/// and the same fragments in every format.
#[test]
fn an_unbound_budget_delivers_every_hunk_in_every_format() {
    let repo = tier_repo();
    let expected = vec![
        ("README.md".to_string(), "1".to_string(), true),
        ("tests/test_x.py".to_string(), "4".to_string(), true),
        ("zsrc/x.py".to_string(), "1".to_string(), true),
    ];
    for format in ["json", "yaml"] {
        let text = pack(repo.path(), 20_000, format);
        let changed: Vec<_> = fragments(&text, format)
            .into_iter()
            .filter(|(_, _, changed)| *changed)
            .collect();
        assert_eq!(changed, expected, "-f {format}");
        assert!(!text.contains("more lines of this change"), "-f {format}");
    }
}

/// The other half of #438: five one-line bumps, then the rewrite of `plan`.
/// The bumps are the decoys — small, cheap and first by position — and the
/// cut took the whole rewrite at the budget where a smaller one showed its
/// signature. A larger budget must never hide a hunk a smaller one showed.
#[test]
fn a_larger_budget_never_hides_a_changed_hunk() {
    let engine = |version: u32| {
        let helpers: String = (0..5)
            .map(|i| format!("def helper_{i}(x):\n    return x + {}\n\n\n", i * version))
            .collect();
        let steps: String = (0..60)
            .map(|i| format!("    step_{i} = helper_{}(rows[{i}]) * {version}\n", i % 5))
            .collect();
        format!("{helpers}def plan(rows):\n{steps}    return step_0\n")
    };
    let repo = repo(
        &[("app/engine.py", engine(1))],
        &[("app/engine.py", engine(2))],
    );
    for format in ["json", "yaml"] {
        let mut previous: Vec<String> = Vec::new();
        for budget in [900, 1100, 1300, 1500, 1700, 2000] {
            let heads: Vec<String> = fragments(&pack(repo.path(), budget, format), format)
                .into_iter()
                .filter(|(_, _, changed)| *changed)
                .map(|(_, first, _)| first)
                .collect();
            let lost: Vec<&String> = previous.iter().filter(|h| !heads.contains(h)).collect();
            assert!(lost.is_empty(), "-f {format} at {budget} lost {lost:?}");
            previous = heads;
        }
        assert!(
            previous.iter().any(|h| h == "21"),
            "-f {format}: plan's rewrite is shown: {previous:?}"
        );
    }
}

/// YAML that a 1.1 reader (PyYAML) reads as it was meant: a symbol named
/// `off` or `y` is a string, not a boolean. The control, a plain name,
/// stays plain.
#[test]
fn yaml_keeps_a_look_alike_symbol_a_string() {
    let repo = repo(
        &[(
            "lib.py",
            "def off(x):\n    return x\n\n\ndef y(x):\n    return x\n\n\ndef plain(x):\n    return x\n".into(),
        )],
        &[(
            "lib.py",
            "def off(x):\n    return x + 1\n\n\ndef y(x):\n    return x + 1\n\n\ndef plain(x):\n    return x + 1\n".into(),
        )],
    );
    let text = pack(repo.path(), 20_000, "yaml");
    assert!(text.contains("symbol: \"off\"\n"), "{text}");
    assert!(text.contains("symbol: \"y\"\n"), "{text}");
    assert!(text.contains("symbol: plain\n"), "{text}");
}
