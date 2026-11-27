// The class "a test lives in the file it tests". A test context is a property
// of the scope that holds an occurrence, never of the file's path: the
// framework decides which scope is a test, and where a source file carries
// one, the answer names that test or names a gap. "No test files in view" and
// "no static test link" over a definition that its own file tests are false
// absences (AC5), whatever the file is called.
//
// One seed per family where the convention exists, each with the framework's
// rule. A seed changes two functions: `total`, which a test in the same file
// exercises, and `tax`, which nothing does, so a file-wide test context
// (everything in a file with a test is tested) fails as surely as a path-only
// one (nothing in a source file is).
//
//   rust        The Rust Book, "Test Organization": unit tests sit in the file
//               they test, in `#[cfg(test)] mod tests`, each marked `#[test]`.
//   typescript  Vitest, "In-source testing": tests in the source file under
//               `if (import.meta.vitest)`, files opted in by `includeSource`.
//   python      unittest: a `TestCase` subclass in the module, run by
//               `unittest.main()`. doctest: `>>>` examples in a docstring, run
//               by `python -m doctest` or pytest's `--doctest-modules`.
//   cpp         doctest: `TEST_CASE` in the implementation file with
//               `DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN`.
//
// Families with no such convention, so that the file's path decides and no seed
// exists: Go (`go test` reads `_test.go` files only, `testing` package), Java
// and Kotlin (JUnit and kotlin.test sit in the `test` source set, Maven and
// Gradle layout), Swift (XCTest targets under `Tests/`), PHP (PHPUnit
// `*Test.php`), C (Unity and CMocka link a separate test executable), shell
// (bats `*.bats`), SQL (pgTAP scripts), and the configuration families.

use serde_json::Value;
use tempfile::TempDir;

mod support;
use support::{NO_WALL_DEADLINE, diffctx, fill_steps};

struct Family {
    name: &'static str,
    /// The file holding `total`, `tax` and the test.
    path: &'static str,
    before: &'static str,
    after: &'static str,
    /// Files both commits carry that the framework needs (a manifest, a config).
    extra: &'static [(&'static str, &'static str)],
    /// A file the parser reads only in part states a gap of its own, so the
    /// claim about the untested neighbour is withheld, whatever its test context.
    claims_untested: bool,
}

const FAMILIES: &[Family] = &[
    Family {
        name: "rust cfg(test) module",
        path: "src/lib.rs",
        before: "pub fn total(items: &[i32]) -> i32 {\n    items.iter().sum()\n}\n\npub fn tax(amount: i32) -> i32 {\n    amount * 2\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn sums() {\n        assert_eq!(total(&[1, 2]), 3);\n    }\n}\n",
        after: "pub fn total(items: &[i32]) -> i32 {\n    items.iter().sum::<i32>() + 0\n}\n\npub fn tax(amount: i32) -> i32 {\n    amount * 3\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n\n    #[test]\n    fn sums() {\n        assert_eq!(total(&[1, 2]), 3);\n    }\n}\n",
        extra: &[(
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )],
        claims_untested: true,
    },
    Family {
        name: "typescript vitest in-source block",
        path: "src/price.ts",
        before: "export function total(items: number[]): number {\n  return items.reduce((a, b) => a + b, 0);\n}\n\nexport function tax(amount: number): number {\n  return amount * 2;\n}\n\nif (import.meta.vitest) {\n  const {it, expect} = import.meta.vitest;\n  it('sums the items', () => {\n    expect(total([1, 2])).toBe(3);\n  });\n}\n",
        after: "export function total(items: number[]): number {\n  return items.reduce((a, b) => a + b, 0) + 0;\n}\n\nexport function tax(amount: number): number {\n  return amount * 3;\n}\n\nif (import.meta.vitest) {\n  const {it, expect} = import.meta.vitest;\n  it('sums the items', () => {\n    expect(total([1, 2])).toBe(3);\n  });\n}\n",
        extra: &[(
            "vitest.config.ts",
            "import {defineConfig} from 'vitest/config';\n\nexport default defineConfig({test: {includeSource: ['src/**/*.ts']}});\n",
        )],
        claims_untested: true,
    },
    Family {
        name: "python unittest case in the module",
        path: "shop/price.py",
        before: "import unittest\n\n\ndef total(items):\n    return sum(items)\n\n\ndef tax(amount):\n    return amount * 2\n\n\nclass TotalTest(unittest.TestCase):\n    def test_total(self):\n        self.assertEqual(total([1, 2]), 3)\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n",
        after: "import unittest\n\n\ndef total(items):\n    return sum(items) + 0\n\n\ndef tax(amount):\n    return amount * 3\n\n\nclass TotalTest(unittest.TestCase):\n    def test_total(self):\n        self.assertEqual(total([1, 2]), 3)\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n",
        extra: &[],
        claims_untested: true,
    },
    Family {
        name: "python doctest in a docstring",
        path: "shop/price.py",
        before: "def total(items):\n    \"\"\"Sum the items.\n\n    >>> total([1, 2])\n    3\n    \"\"\"\n    return sum(items)\n\n\ndef tax(amount):\n    return amount * 2\n",
        after: "def total(items):\n    \"\"\"Sum the items.\n\n    >>> total([1, 2])\n    3\n    \"\"\"\n    return sum(items) + 0\n\n\ndef tax(amount):\n    return amount * 3\n",
        extra: &[(
            "pyproject.toml",
            "[tool.pytest.ini_options]\naddopts = \"--doctest-modules\"\n",
        )],
        claims_untested: true,
    },
    Family {
        name: "cpp doctest case in the implementation file",
        path: "src/price.cpp",
        before: "#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN\n#include \"doctest.h\"\n\nint total(int a, int b) {\n  return a + b;\n}\n\nint tax(int amount) {\n  return amount * 2;\n}\n\nTEST_CASE(\"total adds\") {\n  CHECK(total(1, 2) == 3);\n}\n",
        after: "#define DOCTEST_CONFIG_IMPLEMENT_WITH_MAIN\n#include \"doctest.h\"\n\nint total(int a, int b) {\n  return a + b + 0;\n}\n\nint tax(int amount) {\n  return amount * 3;\n}\n\nTEST_CASE(\"total adds\") {\n  CHECK(total(1, 2) == 3);\n}\n",
        extra: &[],
        claims_untested: false,
    },
];

fn answer(family: &Family) -> (Value, String) {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    let mut first: Vec<(&str, &str)> = family.extra.to_vec();
    first.push((family.path, family.before));
    let second = [(family.path, family.after)];
    fill_steps(root, &[first.as_slice(), second.as_slice()]);
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
            family.name,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).expect("utf8")
    };
    let json = serde_json::from_str(&run("json")).expect("a JSON answer");
    (json, run("md"))
}

fn definition<'a>(doc: &'a Value, name: &str) -> Option<&'a Value> {
    doc["definitions"]
        .as_array()?
        .iter()
        .find(|d| d["symbol"] == name)
}

/// The answer says the definition has no static test link: a listed definition
/// whose guard is `none`, or a row of the lists a callerless definition goes in.
fn claimed_untested(doc: &Value, name: &str) -> bool {
    let listed = definition(doc, name).is_some_and(|d| d["guard"]["state"] == "none");
    let unlisted = ["no_caller", "called_in_diff"].iter().any(|key| {
        doc["unlisted"][key]["items"]
            .as_array()
            .is_some_and(|rows| rows.iter().any(|d| d["symbol"] == name))
    });
    listed || unlisted
}

/// The test is named or a gap is named: tests are in view, and the changed
/// definition has its own row, which carries no "no static test link" guard. A
/// definition that appears in no row is a silent loss, not a tested one.
fn names_its_test(doc: &Value, name: &str) -> bool {
    doc["tests"]["known"] == true
        && definition(doc, name).is_some_and(|d| d["guard"]["state"] != "none")
}

#[test]
fn a_test_in_the_same_file_is_in_view() {
    let mut failures = Vec::new();
    for family in FAMILIES {
        let (doc, md) = answer(family);
        if doc["tests"]["known"] != true || md.contains("no test files in view") {
            failures.push(format!(
                "{}: tests are not in view although the file holds one\n{md}",
                family.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_definition_a_test_in_its_file_exercises_names_that_test() {
    let mut failures = Vec::new();
    for family in FAMILIES {
        let (doc, md) = answer(family);
        if !names_its_test(&doc, "total") {
            failures.push(format!(
                "{}: `total` is called untested although its file tests it\n{md}",
                family.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn the_neighbour_no_test_exercises_stays_untested() {
    let mut failures = Vec::new();
    for family in FAMILIES.iter().filter(|f| f.claims_untested) {
        let (doc, md) = answer(family);
        if !claimed_untested(&doc, "tax") {
            failures.push(format!(
                "{}: `tax` has no test and the answer does not say so\n{md}",
                family.name
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
