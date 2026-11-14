// What an impact answer says of the tests of a changed definition has three
// values: the tests named, "no static test link" (the search up the uses ended
// and every definition on it is accounted for), and "tests not established" (a
// test is further up than the hop bound names, or a definition on the way holds
// a use no binding explains, so a test may be there). An absence is claimed from
// a closed search only, whichever the hop bound is.
//
// The seeds share one shape: `leaf` changes, `run` or `m1` calls it, and the
// test file sits at a different height above it.

use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::{NO_WALL_DEADLINE, diffctx, fill_steps};

struct Seed {
    name: &'static str,
    files: &'static [(&'static str, &'static str)],
    changed: (&'static str, &'static str),
    /// `guard.state` of the definition `leaf` ("" when it states none), and of
    /// the row of each caller named.
    definition: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

const PACKAGE: (&str, &str) = ("pkg/__init__.py", "# package\n");

const SEEDS: &[Seed] = &[
    Seed {
        name: "a test calls the caller",
        files: &[
            PACKAGE,
            (
                "pkg/direct.py",
                "def leaf(x):\n    return x + 1\n\n\ndef run(x):\n    return leaf(x)\n",
            ),
            (
                "tests/test_direct.py",
                "from pkg.direct import run\n\n\ndef test_run():\n    assert run(1) == 2\n",
            ),
        ],
        changed: (
            "pkg/direct.py",
            "def leaf(x):\n    return x + 2\n\n\ndef run(x):\n    return leaf(x)\n",
        ),
        definition: "",
        rows: &[("run", "linked")],
    },
    Seed {
        name: "the search ended and no test is on it",
        files: &[
            PACKAGE,
            (
                "pkg/lonely.py",
                "def leaf(x):\n    return x + 1\n\n\ndef run(x):\n    return leaf(x)\n",
            ),
            ("pkg/other.py", "def other(x):\n    return x * 2\n"),
            (
                "tests/test_other.py",
                "from pkg.other import other\n\n\ndef test_other():\n    assert other(2) == 4\n",
            ),
        ],
        changed: (
            "pkg/lonely.py",
            "def leaf(x):\n    return x + 2\n\n\ndef run(x):\n    return leaf(x)\n",
        ),
        definition: "none",
        rows: &[("run", "none")],
    },
    Seed {
        name: "a test is further up than the hop bound reaches",
        files: &[
            PACKAGE,
            (
                "pkg/chain.py",
                "def leaf(x):\n    return x + 1\n\n\ndef m1(x):\n    return leaf(x)\n\n\ndef m2(x):\n    return m1(x)\n\n\ndef m3(x):\n    return m2(x)\n\n\ndef m4(x):\n    return m3(x)\n",
            ),
            (
                "tests/test_chain.py",
                "from pkg.chain import m4\n\n\ndef test_m4():\n    assert m4(1) == 2\n",
            ),
        ],
        changed: (
            "pkg/chain.py",
            "def leaf(x):\n    return x + 2\n\n\ndef m1(x):\n    return leaf(x)\n\n\ndef m2(x):\n    return m1(x)\n\n\ndef m3(x):\n    return m2(x)\n\n\ndef m4(x):\n    return m3(x)\n",
        ),
        definition: "not_established",
        rows: &[("m1", "not_established")],
    },
    Seed {
        name: "a use no binding explains is above the caller",
        files: &[
            PACKAGE,
            (
                "pkg/service.py",
                "def leaf(x):\n    return x + 1\n\n\ndef run(x):\n    return leaf(x)\n",
            ),
            (
                "tests/test_service.py",
                "def test_run(client):\n    assert client.run(1) == 2\n",
            ),
        ],
        changed: (
            "pkg/service.py",
            "def leaf(x):\n    return x + 2\n\n\ndef run(x):\n    return leaf(x)\n",
        ),
        definition: "not_established",
        rows: &[("run", "not_established")],
    },
];

fn answer(seed: &Seed) -> (Value, String) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    let changed = [seed.changed];
    fill_steps(root, &[seed.files, changed.as_slice()]);
    let run = |format: &str| {
        let out = diffctx()
            .current_dir(root)
            .args(["--diff", "HEAD~1..HEAD", "--mode", "impact", "-f", format])
            .args(["-q", "--timeout", NO_WALL_DEADLINE])
            .output()
            .expect("run diffctx");
        assert!(
            out.status.success(),
            "{}: {}",
            seed.name,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8")
    };
    let json = serde_json::from_str(&run("json")).expect("a JSON answer");
    (json, run("md"))
}

fn state(guard: &Value) -> &str {
    guard["state"].as_str().unwrap_or("")
}

#[test]
fn the_tests_claim_has_three_values_from_the_search_that_closed_or_did_not() {
    let mut failures = Vec::new();
    for seed in SEEDS {
        let (doc, md) = answer(seed);
        let Some(leaf) = doc["definitions"]
            .as_array()
            .and_then(|defs| defs.iter().find(|d| d["symbol"] == "leaf"))
        else {
            failures.push(format!("{}: `leaf` has no entry\n{md}", seed.name));
            continue;
        };
        if state(&leaf["guard"]) != seed.definition {
            failures.push(format!(
                "{}: `leaf` states `{}`, expected `{}`\n{md}",
                seed.name,
                state(&leaf["guard"]),
                seed.definition
            ));
        }
        for (caller, expected) in seed.rows {
            let found = leaf["rows"]
                .as_array()
                .and_then(|rows| rows.iter().find(|r| r["symbol"] == *caller))
                .map(|row| state(&row["guard"]).to_string());
            if found.as_deref() != Some(expected) {
                failures.push(format!(
                    "{}: the row of `{caller}` states {found:?}, expected `{expected}`\n{md}",
                    seed.name
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn an_unclosed_search_says_so_in_the_text_and_claims_no_absence() {
    let seed = &SEEDS[2];
    let (_, md) = answer(seed);
    assert!(md.contains("tests not established"), "{md}");
    assert!(!md.contains("\u{2014} no static test link"), "{md}");
    assert!(!md.contains("no static test link found"), "{md}");
}
