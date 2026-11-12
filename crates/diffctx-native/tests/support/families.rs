// Seed repositories for the fourteen families of the engine's registry: one
// changed definition, one known caller and a control with no caller anywhere,
// built from a shape so a transformation is a difference between two shapes.
// The expected link comes from reading the generated files, never from
// running diffctx.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::Value;
use tempfile::TempDir;

use super::diffctx;
use super::seeds::run_git;

pub(crate) type Files = BTreeMap<String, String>;

// The size cap of a changed file, `max_changed_file_size`.
const SIZE_CAP: usize = 5_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Family {
    Python,
    TypeScript,
    Rust,
    Kotlin,
    Java,
    Go,
    Php,
    C,
    Config,
    Terraform,
    Dockerfile,
    Shell,
    Swift,
    Sql,
}

impl Family {
    pub(crate) const ALL: [Self; 14] = [
        Self::Python,
        Self::TypeScript,
        Self::Rust,
        Self::Kotlin,
        Self::Java,
        Self::Go,
        Self::Php,
        Self::C,
        Self::Config,
        Self::Terraform,
        Self::Dockerfile,
        Self::Shell,
        Self::Swift,
        Self::Sql,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Kotlin => "kotlin",
            Self::Java => "java",
            Self::Go => "go",
            Self::Php => "php",
            Self::C => "c",
            Self::Config => "config",
            Self::Terraform => "terraform",
            Self::Dockerfile => "dockerfile",
            Self::Shell => "shell",
            Self::Swift => "swift",
            Self::Sql => "sql",
        }
    }

    // The families whose definitions `--symbol PATH:NAME` names.
    pub(crate) fn answers_symbol(self) -> bool {
        !matches!(self, Self::Config | Self::Terraform | Self::Dockerfile)
    }

    // The families that state a clean absence as `empty: true`; the others
    // state it through their own sections, so only the absence of a gap is
    // read there.
    //
    // A SQL file is a schema contract by its extension, so a change to one is
    // never `empty`: its absence is read through the gap sections alone.
    pub(crate) fn proves_empty(self) -> bool {
        self.answers_symbol() && self != Self::Sql
    }

    // The file suffixes of the family's interaction group, as the repository
    // holds them.
    pub(crate) fn group(self) -> &'static [&'static str] {
        match self {
            Self::Python => &[".py"],
            Self::TypeScript => &[
                ".ts", ".tsx", ".js", ".jsx", ".html", ".vue", ".svelte", ".astro",
            ],
            Self::Rust => &[".rs"],
            Self::Kotlin | Self::Java => &[".kt", ".kts", ".java", ".scala", ".groovy", ".gradle"],
            Self::Go => &[".go"],
            Self::Php => &[".php"],
            Self::C => &[".c", ".h", ".cpp", ".hpp", ".cc", ".m"],
            Self::Config => &[".yaml", ".yml", ".json", ".toml", ".ini"],
            Self::Terraform => &[".tf"],
            Self::Dockerfile => &["Dockerfile"],
            Self::Shell => &[".sh"],
            Self::Swift => &[".swift"],
            Self::Sql => &[".sql"],
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Break {
    Declaration,
    Call,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Neighbour {
    Java,
    Scala,
    Kotlin,
    Groovy,
    Cpp,
    CppDefinition,
    Js,
    Vue,
    Svelte,
    Html,
    Astro,
}

impl Neighbour {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Java => "java",
            Self::Scala => "scala",
            Self::Kotlin => "kotlin",
            Self::Groovy => "groovy",
            Self::Cpp => "cpp",
            Self::CppDefinition => "cpp_definition",
            Self::Js => "js",
            Self::Vue => "vue",
            Self::Svelte => "svelte",
            Self::Html => "html",
            Self::Astro => "astro",
        }
    }
}

// What a transformation may vary. The default is the plain seed.
#[derive(Clone, Debug, Default)]
pub(crate) struct Shape {
    // Left-nested terms after the call: the call is the leaf `depth` levels
    // down the left spine.
    pub depth: usize,
    pub broken: Option<Break>,
    pub oversize: bool,
    pub neighbour: Option<Neighbour>,
    pub one_letter: bool,
}

// A seed as a repository: the first commit, and the files the second commit
// writes (the change under review).
#[derive(Clone, Debug)]
pub(crate) struct Repo {
    pub family: Family,
    pub base: Files,
    pub change: Files,
    // The file that calls the definition; none for the control.
    pub caller: Option<String>,
    pub definition: String,
    pub symbol: String,
}

pub(crate) const RANGE: &str = "HEAD~1..HEAD";

impl Repo {
    pub(crate) fn commit_into(&self, dir: &Path) {
        run_git(dir, &["init", "-q", "-b", "main", "."]);
        run_git(dir, &["config", "user.email", "test@example.com"]);
        run_git(dir, &["config", "user.name", "diffctx tests"]);
        run_git(dir, &["config", "commit.gpgsign", "false"]);
        for (index, files) in [&self.base, &self.change].into_iter().enumerate() {
            for (rel, content) in files {
                let path = dir.join(rel);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("mkdir");
                }
                std::fs::write(path, content).expect("write");
            }
            run_git(dir, &["add", "-A"]);
            run_git(dir, &["commit", "-q", "-m", &format!("step {index}")]);
        }
    }

    pub(crate) fn symbol_arg(&self) -> String {
        format!("{}:{}", self.definition, self.symbol)
    }

    // Every file of the repository at the head that belongs to the family's
    // interaction group, by path.
    pub(crate) fn group_files(&self) -> Vec<(String, String)> {
        let mut head = self.base.clone();
        head.extend(self.change.clone());
        head.into_iter()
            .filter(|(path, _)| {
                self.family
                    .group()
                    .iter()
                    .any(|suffix| path.ends_with(suffix))
            })
            .collect()
    }
}

const LEAF_PAD: usize = 98;

fn padding(comment: &str) -> String {
    let line = format!("{comment} {}\n", "x".repeat(LEAF_PAD));
    line.repeat(SIZE_CAP / line.len() + 2)
}

// `expr` followed by `depth` left-nested terms: `expr + 1 + 1 ...`.
fn chain(expr: &str, depth: usize, joiner: &str) -> String {
    format!("{expr}{}", joiner.repeat(depth))
}

// The pieces one family's seed is made of.
struct Parts {
    manifests: Vec<(&'static str, &'static str)>,
    unrelated: Vec<(String, String)>,
    definition: String,
    base: String,
    head: String,
    symbol: &'static str,
    caller: String,
    caller_text: String,
    comment: &'static str,
    // Joins the nested terms of a call.
    // A line no grammar of the family reads, set on its own line.
    garbage: &'static str,
    // Where the garbage goes in the caller: the line number counted from the
    // top of the text before which it is set (a statement of the call's own
    // body), and in the definition's file (before the declaration).
    declaration_marker: &'static str,
}

fn pair(path: &str, text: &str) -> (String, String) {
    (path.to_string(), text.to_string())
}

fn python(s: &Shape) -> Parts {
    let (dir, file) = if s.one_letter {
        ("a", "a")
    } else {
        ("shop", "pricing")
    };
    let definition = format!("{dir}/{file}.py");
    let module = format!("{dir}.{file}");
    let call = chain("total(cart)", s.depth, " + 1");
    Parts {
        manifests: vec![(
            "pyproject.toml",
            "[project]\nname = \"shop\"\nversion = \"0.1.0\"\n",
        )],
        unrelated: vec![pair(
            &format!("{dir}/util.py"),
            "def helper(x):\n    return x\n",
        )],
        definition,
        base: "def total(items):\n    return sum(items)\n".into(),
        head: "def total(items):\n    return sum(items) * 2\n".into(),
        symbol: "total",
        caller: format!("{dir}/checkout.py"),
        caller_text: format!(
            "from {module} import total\n\n\ndef charge(cart):\n@CALL@\n    return {call}\n"
        ),
        comment: "#",
        garbage: "    ))) ((( @@@\n",
        declaration_marker: "def total",
    }
}

fn typescript(s: &Shape) -> Parts {
    let (dir, file) = if s.one_letter {
        ("a", "a")
    } else {
        ("src", "pricing")
    };
    let call = chain("total(cart)", s.depth, " + 1");
    Parts {
        manifests: vec![
            ("package.json", "{\n  \"name\": \"shop\",\n  \"version\": \"1.0.0\"\n}\n"),
            ("tsconfig.json", "{\n  \"compilerOptions\": {}\n}\n"),
        ],
        unrelated: vec![pair(
            &format!("{dir}/util.ts"),
            "export function helper(x: number) {\n  return x;\n}\n",
        )],
        definition: format!("{dir}/{file}.ts"),
        base: "export function total(items: number[]) {\n  return items.reduce((a, b) => a + b, 0);\n}\n".into(),
        head: "export function total(items: number[]) {\n  return items.reduce((a, b) => a + b, 0) * 2;\n}\n".into(),
        symbol: "total",
        caller: format!("{dir}/checkout.ts"),
        caller_text: format!(
            "import {{ total }} from './{file}';\n\nexport function charge(cart: number[]) {{\n@CALL@\n  const result = {call};\n  return result;\n}}\n"
        ),
        comment: "//",
        garbage: "  ))) ((( @@@\n",
        declaration_marker: "export function total",
    }
}

fn rust(s: &Shape) -> Parts {
    let module = if s.one_letter { "a" } else { "pricing" };
    let call = chain("total(cart)", s.depth, " + 1");
    Parts {
        manifests: vec![(
            "Cargo.toml",
            "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )],
        unrelated: vec![
            pair("src/util.rs", "pub fn helper(x: i64) -> i64 {\n    x\n}\n"),
            pair(
                "src/lib.rs",
                &format!("pub mod {module};\npub mod checkout;\npub mod util;\n"),
            ),
        ],
        definition: format!("src/{module}.rs"),
        base: "pub fn total(items: &[i64]) -> i64 {\n    items.iter().sum()\n}\n".into(),
        head: "pub fn total(items: &[i64]) -> i64 {\n    items.iter().sum::<i64>() * 2\n}\n".into(),
        symbol: "total",
        caller: "src/checkout.rs".into(),
        caller_text: format!(
            "use crate::{module}::total;\n\npub fn charge(cart: &[i64]) -> i64 {{\n@CALL@\n    let result = {call};\n    result\n}}\n"
        ),
        comment: "//",
        garbage: "    ))) ((( @@@\n",
        declaration_marker: "pub fn total",
    }
}

fn kotlin(s: &Shape) -> Parts {
    let package = if s.one_letter { "a" } else { "shop" };
    let import = if s.one_letter {
        "import a.total\n\n"
    } else {
        ""
    };
    let call = chain("total(cart)", s.depth, " + 1");
    Parts {
        manifests: vec![
            ("settings.gradle.kts", "rootProject.name = \"shop\"\n"),
            (
                "build.gradle.kts",
                "plugins {\n    kotlin(\"jvm\") version \"2.0.0\"\n}\n",
            ),
        ],
        unrelated: vec![pair(
            "src/main/kotlin/shop/Util.kt",
            "package shop\n\nfun helper(x: Int): Int {\n    return x\n}\n",
        )],
        definition: format!("src/main/kotlin/{package}/Pricing.kt"),
        base: format!(
            "package {package}\n\nfun total(items: List<Int>): Int {{\n    return items.sum()\n}}\n"
        ),
        head: format!(
            "package {package}\n\nfun total(items: List<Int>): Int {{\n    return items.sum() * 2\n}}\n"
        ),
        symbol: "total",
        caller: "src/main/kotlin/shop/Checkout.kt".into(),
        caller_text: format!(
            "package shop\n\n{import}fun charge(cart: List<Int>): Any {{\n@CALL@\n    val result = {call}\n    return result\n}}\n"
        ),
        comment: "//",
        garbage: "    ))) ((( @@@\n",
        declaration_marker: "fun total",
    }
}

fn swift(s: &Shape) -> Parts {
    let target = if s.one_letter { "A" } else { "Shop" };
    let call = chain("Pricing.total(items)", s.depth, " + 1");
    Parts {
        manifests: vec![(
            "Package.swift",
            "// swift-tools-version:5.9\nimport PackageDescription\n\nlet package = Package(\n    name: \"Fixture\",\n    targets: [.target(name: \"Shop\")]\n)\n",
        )],
        unrelated: vec![pair(
            "Sources/Shop/Util.swift",
            "func helper(_ x: Int) -> Int {\n    return x\n}\n",
        )],
        definition: format!("Sources/{target}/Pricing.swift"),
        base: "enum Pricing {\n    static func total(_ items: [Int]) -> Int {\n        return items.reduce(0, +)\n    }\n}\n".into(),
        head: "enum Pricing {\n    static func total(_ items: [Int]) -> Int {\n        return items.reduce(0, +) * 2\n    }\n}\n".into(),
        symbol: "total",
        caller: format!("Sources/{target}/Checkout.swift"),
        caller_text: format!(
            "func charge(_ items: [Int]) -> Int {{\n@CALL@\n    let result = {call}\n    return result\n}}\n"
        ),
        comment: "//",
        garbage: "    ))) ((( @@@\n",
        declaration_marker: "static func total",
    }
}

fn sql(s: &Shape) -> Parts {
    let dir = if s.one_letter { "a" } else { "db" };
    let call = chain("refresh_book(cart)", s.depth, " + 1");
    Parts {
        manifests: vec![],
        unrelated: vec![pair(
            &format!("{dir}/V9__util.sql"),
            "CREATE FUNCTION helper(x int) RETURNS int\nLANGUAGE sql AS $$\n  SELECT x\n$$;\n",
        )],
        definition: format!("{dir}/V1__fn.sql"),
        base: "CREATE FUNCTION refresh_book(items int) RETURNS int\nLANGUAGE sql AS $$\n  SELECT items\n$$;\n".into(),
        head: "CREATE FUNCTION refresh_book(items int) RETURNS int\nLANGUAGE sql AS $$\n  SELECT items * 2\n$$;\n".into(),
        symbol: "refresh_book",
        caller: format!("{dir}/V2__use.sql"),
        caller_text: format!("SELECT 1;\n@CALL@\nSELECT {call};\n"),
        comment: "--",
        garbage: "))) ((( @@@\n",
        declaration_marker: "CREATE FUNCTION",
    }
}

fn java(s: &Shape) -> Parts {
    let package = if s.one_letter { "a" } else { "shop" };
    let import = if s.one_letter {
        "import a.Pricing;\n\n"
    } else {
        ""
    };
    let call = chain("Pricing.total(items)", s.depth, " + 1");
    let body = |factor: &str| {
        format!(
            "package {package};\n\npublic class Pricing {{\n    public static int total(int[] items) {{\n        int sum = 0;\n        for (int n : items) sum += n;\n        return sum{factor};\n    }}\n}}\n"
        )
    };
    Parts {
        manifests: vec![],
        unrelated: vec![pair(
            "src/main/java/shop/Util.java",
            "package shop;\n\npublic class Util {\n    public static int helper(int x) {\n        return x;\n    }\n}\n",
        )],
        definition: format!("src/main/java/{package}/Pricing.java"),
        base: body(""),
        head: body(" * 2"),
        symbol: "total",
        caller: "src/main/java/shop/Checkout.java".into(),
        caller_text: format!(
            "package shop;\n\n{import}public class Checkout {{\n    public int charge(int[] items) {{\n@CALL@\n        return {call};\n    }}\n}}\n"
        ),
        comment: "//",
        garbage: "        ))) ((( @@@\n",
        declaration_marker: "    public static int total",
    }
}

fn go(s: &Shape) -> Parts {
    let (dir, package) = if s.one_letter {
        ("a", "a")
    } else {
        ("pricing", "pricing")
    };
    let call = chain(&format!("{package}.Total(items)"), s.depth, " + 1");
    let body = |factor: &str| {
        format!(
            "package {package}\n\nfunc Total(items []int) int {{\n\tsum := 0\n\tfor _, n := range items {{\n\t\tsum += n\n\t}}\n\treturn sum{factor}\n}}\n"
        )
    };
    Parts {
        manifests: vec![("go.mod", "module example.com/app\n\ngo 1.22\n")],
        unrelated: vec![pair(
            "util/util.go",
            "package util\n\nfunc Helper(x int) int {\n\treturn x\n}\n",
        )],
        definition: format!("{dir}/{package}.go"),
        base: body(""),
        head: body(" * 2"),
        symbol: "Total",
        caller: "checkout/checkout.go".into(),
        caller_text: format!(
            "package checkout\n\nimport \"example.com/app/{dir}\"\n\nfunc Charge(items []int) int {{\n@CALL@\n\treturn {call}\n}}\n"
        ),
        comment: "//",
        garbage: "\t))) ((( @@@\n",
        declaration_marker: "func Total",
    }
}

fn php(s: &Shape) -> Parts {
    let namespace = if s.one_letter { "A" } else { "Shop" };
    let import = if s.one_letter {
        "use A\\Pricing;\n\n"
    } else {
        ""
    };
    let call = chain("Pricing::total($items)", s.depth, " + 1");
    let body = |factor: &str| {
        format!(
            "<?php\n\nnamespace {namespace};\n\nclass Pricing\n{{\n    public static function total(array $items): int\n    {{\n        return array_sum($items){factor};\n    }}\n}}\n"
        )
    };
    Parts {
        manifests: vec![],
        unrelated: vec![pair(
            "src/Util.php",
            "<?php\n\nnamespace Shop;\n\nclass Util\n{\n    public static function helper(int $x): int\n    {\n        return $x;\n    }\n}\n",
        )],
        definition: format!("src/{namespace}/Pricing.php"),
        base: body(""),
        head: body(" * 2"),
        symbol: "total",
        caller: "src/Shop/Checkout.php".into(),
        caller_text: format!(
            "<?php\n\nnamespace Shop;\n\n{import}class Checkout\n{{\n    public function charge(array $items): int\n    {{\n@CALL@\n        return {call};\n    }}\n}}\n"
        ),
        comment: "//",
        garbage: "        ))) ((( @@@\n",
        declaration_marker: "    public static function total",
    }
}

fn c(s: &Shape) -> Parts {
    let dir = if s.one_letter { "a" } else { "src" };
    let call = chain("total(items, n)", s.depth, " + 1");
    let body = |factor: &str| {
        format!(
            "#include \"pricing.h\"\n\nint total(const int *items, int n)\n{{\n    int sum = 0;\n    for (int i = 0; i < n; i++) {{\n        sum += items[i];\n    }}\n    return sum{factor};\n}}\n"
        )
    };
    Parts {
        manifests: vec![],
        unrelated: vec![
            pair(
                &format!("{dir}/pricing.h"),
                "#ifndef PRICING_H\n#define PRICING_H\n\nint total(const int *items, int n);\n\n#endif\n",
            ),
            pair(
                &format!("{dir}/util.c"),
                "int helper(int x)\n{\n    return x;\n}\n",
            ),
        ],
        definition: format!("{dir}/pricing.c"),
        base: body(""),
        head: body(" * 2"),
        symbol: "total",
        caller: format!("{dir}/checkout.c"),
        caller_text: format!(
            "#include \"pricing.h\"\n\nint charge(const int *items, int n)\n{{\n@CALL@\n    return {call};\n}}\n"
        ),
        comment: "//",
        garbage: "    ))) ((( @@@\n",
        declaration_marker: "int total",
    }
}

fn config(s: &Shape) -> Parts {
    let dir = if s.one_letter { "a" } else { "rules" };
    let nest = "{a: ".repeat(s.depth);
    let close = "}".repeat(s.depth);
    let query = "query: 'up{job=\"demo\"}'";
    let reader = if s.depth == 0 {
        format!("    {query}\n")
    } else {
        format!("    nest: {nest}{{{query}}}{close}\n")
    };
    let alerts = |expr: &str| {
        format!(
            "groups:\n  - name: demo\n    rules:\n      - alert: DemoDown\n        expr: '{expr}'\n"
        )
    };
    let dashboard_dir = if s.one_letter { "a" } else { "app" };
    Parts {
        manifests: vec![],
        unrelated: vec![
            pair(
                &format!("{dashboard_dir}/other.yaml"),
                "panels:\n  - title: other\n    query: 'up{job=\"other\"}'\n",
            ),
            pair(
                "scripts/lint.sh",
                &format!("#!/bin/sh\npromtool check rules {dir}/alerts.yaml\n"),
            ),
        ],
        definition: format!("{dir}/alerts.yaml"),
        base: alerts("up{job=\"demo\"} == 0"),
        head: alerts("up{job=\"demo\", pod=\"x\"} == 0"),
        symbol: "DemoDown",
        caller: format!("{dashboard_dir}/dashboard.yaml"),
        caller_text: format!("panels:\n  - title: demo\n@CALL@\n{reader}"),
        comment: "#",
        garbage: "  : : [ {\n",
        declaration_marker: "groups:",
    }
}

fn terraform(s: &Shape) -> Parts {
    let module = if s.one_letter { "m" } else { "net" };
    let dir = if s.one_letter {
        "m".to_string()
    } else {
        "modules/net".to_string()
    };
    let value = chain("\"10.0.0.0/16\"", s.depth, " + \"x\"");
    Parts {
        manifests: vec![],
        unrelated: vec![pair(
            &format!("{dir}/other.tf"),
            "variable \"region\" {\n  type = string\n}\n",
        )],
        definition: format!("{dir}/main.tf"),
        base: "variable \"cidr\" {\n  type = string\n}\n".into(),
        head: "variable \"cidr\" {\n  type = list(string)\n}\n".into(),
        symbol: "cidr",
        caller: "main.tf".into(),
        caller_text: format!(
            "module \"{module}\" {{\n  source = \"./{dir}\"\n@CALL@\n  cidr   = {value}\n}}\n"
        ),
        comment: "#",
        garbage: "  }}} @@@\n",
        declaration_marker: "variable \"cidr\"",
    }
}

fn dockerfile(s: &Shape) -> Parts {
    let stage = if s.one_letter { "b" } else { "base" };
    let dir = if s.one_letter { "a/" } else { "" };
    let text = |install: &str| {
        format!(
            "FROM alpine:3.20 AS {stage}\nRUN {install}\n\nFROM {stage} AS app\nCOPY app.sh /app.sh\n"
        )
    };
    Parts {
        manifests: vec![],
        unrelated: vec![
            pair(&format!("{dir}app.sh"), "echo app\n"),
            pair(
                ".github/workflows/build.yml",
                &format!(
                    "name: build\non: push\njobs:\n  image:\n    runs-on: ubuntu-latest\n    steps:\n      - run: docker build -f {dir}Dockerfile .\n"
                ),
            ),
        ],
        definition: format!("{dir}Dockerfile"),
        base: text("apk add curl"),
        head: text("apk add curl git"),
        symbol: if s.one_letter { "b" } else { "base" },
        caller: format!("{dir}Dockerfile"),
        caller_text: String::new(),
        comment: "#",
        garbage: "@@@ ((( )))\n",
        declaration_marker: "FROM alpine:3.20 AS",
    }
}

fn shell(s: &Shape) -> Parts {
    let (dir, file) = if s.one_letter {
        ("a", "a")
    } else {
        ("scripts", "pricing")
    };
    let call = chain("total \"$@\"", s.depth, " && true");
    let body = |print: &str| {
        format!(
            "total() {{\n  local sum=0\n  for n in \"$@\"; do sum=$((sum + n)); done\n  {print}\n}}\n"
        )
    };
    Parts {
        manifests: vec![],
        unrelated: vec![pair(&format!("{dir}/notes.sh"), "echo notes\n")],
        definition: format!("{dir}/{file}.sh"),
        base: body("echo \"$sum\""),
        head: body("printf '%s\\n' \"$sum\""),
        symbol: "total",
        caller: format!("{dir}/checkout.sh"),
        caller_text: format!(
            ". ./{file}.sh\n\ncharge() {{\n  local t\n@CALL@\n  t=$({call})\n  echo \"$((t + 1))\"\n}}\n"
        ),
        comment: "#",
        garbage: "  fi ) @@@\n",
        declaration_marker: "total()",
    }
}

fn parts(family: Family, shape: &Shape) -> Parts {
    match family {
        Family::Python => python(shape),
        Family::TypeScript => typescript(shape),
        Family::Rust => rust(shape),
        Family::Kotlin => kotlin(shape),
        Family::Java => java(shape),
        Family::Go => go(shape),
        Family::Php => php(shape),
        Family::C => c(shape),
        Family::Config => config(shape),
        Family::Terraform => terraform(shape),
        Family::Dockerfile => dockerfile(shape),
        Family::Shell => shell(shape),
        Family::Swift => swift(shape),
        Family::Sql => sql(shape),
    }
}

// The garbage line set before the first line that starts with `marker`.
fn before(text: &str, marker: &str, garbage: &str) -> String {
    let at = text
        .find(marker)
        .unwrap_or_else(|| panic!("the marker {marker:?} is not in {text:?}"));
    let line = text[..at].rfind('\n').map_or(0, |i| i + 1);
    format!("{}{garbage}{}", &text[..line], &text[line..])
}

// The caller written by a neighbour language of the group: the same call,
// another syntax.
fn neighbour(family: Family, which: Neighbour, definition: &str) -> (String, String) {
    let _ = definition;
    match (family, which) {
        (Family::Kotlin, Neighbour::Java) => pair(
            "src/main/java/shop/Checkout.java",
            "package shop;\n\nimport java.util.List;\n\npublic class Checkout {\n    public int charge(List<Integer> items) {\n        return PricingKt.total(items);\n    }\n}\n",
        ),
        (Family::Kotlin, Neighbour::Scala) => pair(
            "src/main/scala/shop/Checkout.scala",
            "package shop\n\nobject Checkout {\n  def charge(items: java.util.List[Integer]): Int = PricingKt.total(items)\n}\n",
        ),
        (Family::Java, Neighbour::Kotlin) => pair(
            "src/main/kotlin/shop/Checkout.kt",
            "package shop\n\nfun charge(items: IntArray): Int {\n    return Pricing.total(items)\n}\n",
        ),
        (Family::Java, Neighbour::Groovy) => pair(
            "src/main/groovy/shop/Checkout.groovy",
            "package shop\n\nclass Checkout {\n    int charge(int[] items) {\n        return Pricing.total(items)\n    }\n}\n",
        ),
        (Family::C, Neighbour::Cpp) => pair(
            "src/checkout.cpp",
            "#include \"pricing.h\"\n\nint charge(const int *items, int n)\n{\n    return total(items, n);\n}\n",
        ),
        (Family::C, Neighbour::CppDefinition) => pair(
            "src/checkout.c",
            "#include \"pricing.h\"\n\nint charge(const int *items, int n)\n{\n    return total(items, n);\n}\n",
        ),
        (Family::TypeScript, Neighbour::Js) => pair(
            "src/checkout.js",
            "import { total } from './pricing';\n\nexport function charge(cart) {\n  const result = total(cart);\n  return result;\n}\n",
        ),
        (Family::TypeScript, Neighbour::Vue) => pair(
            "src/checkout.vue",
            "<script setup lang=\"ts\">\nimport { total } from './pricing';\n\nconst result = total([1]);\n</script>\n\n<template>\n  <p>{{ result }}</p>\n</template>\n",
        ),
        (Family::TypeScript, Neighbour::Svelte) => pair(
            "src/checkout.svelte",
            "<script lang=\"ts\">\n  import { total } from './pricing';\n  const result = total([1]);\n</script>\n\n<p>{result}</p>\n",
        ),
        (Family::TypeScript, Neighbour::Html) => pair(
            "src/checkout.html",
            "<script type=\"module\">\n  import { total } from './pricing.js';\n  const result = total([1]);\n</script>\n",
        ),
        (Family::TypeScript, Neighbour::Astro) => pair(
            "src/checkout.astro",
            "---\nimport { total } from './pricing';\nconst result = total([1]);\n---\n<p>{result}</p>\n",
        ),
        (family, which) => panic!(
            "no {} caller of a {} definition",
            which.label(),
            family.label()
        ),
    }
}

fn into_files(items: impl IntoIterator<Item = (String, String)>) -> Files {
    items.into_iter().collect()
}

// A seed with the caller, and the change that makes its definition's body
// different. A Dockerfile and a configuration are consumed outside the code
// the engine reads, so each seed carries a file that names the changed file's
// path (a workflow, a script): without one the answer is a `content_untraced`
// gap by design, never a clean absence.
pub(crate) fn with_caller(family: Family, shape: &Shape) -> Repo {
    let p = parts(family, shape);
    let mut definition_text = p.base.clone();
    let mut head_text = p.head.clone();
    let mut caller_text = p.caller_text.clone();
    let mut caller_path = p.caller.clone();
    let mut definition_path = p.definition.clone();
    if shape.broken == Some(Break::Declaration) {
        definition_text = before(&definition_text, p.declaration_marker, p.garbage);
        head_text = before(&head_text, p.declaration_marker, p.garbage);
    }
    if family == Family::Dockerfile {
        // The stages are read in the file that defines them.
        if shape.broken == Some(Break::Call) {
            let reader = format!("FROM {} AS app", p.symbol);
            definition_text = before(&definition_text, &reader, p.garbage);
            head_text = before(&head_text, &reader, p.garbage);
        }
        if shape.oversize {
            let pad = padding(p.comment);
            definition_text.push_str(&pad);
            head_text.push_str(&pad);
        }
    } else {
        let at = if shape.broken == Some(Break::Call) {
            p.garbage
        } else {
            ""
        };
        caller_text = caller_text.replace("@CALL@\n", at);
        if shape.oversize {
            caller_text.push_str(&padding(p.comment));
        }
    }
    let mut base = into_files(p.unrelated.clone());
    for (path, text) in &p.manifests {
        base.insert((*path).to_string(), (*text).to_string());
    }
    if let Some(which) = shape.neighbour {
        let (path, mut text) = neighbour(family, which, &definition_path);
        if shape.oversize {
            text.push_str(&padding(p.comment));
        }
        caller_path = path;
        caller_text = text;
        if which == Neighbour::CppDefinition {
            // The definition is C++, the header and the caller are C.
            let cpp = definition_path.replace("pricing.c", "pricing.cpp");
            definition_text = format!(
                "extern \"C\" {}",
                definition_text.replacen("#include \"pricing.h\"\n\n", "", 1)
            );
            head_text = format!(
                "extern \"C\" {}",
                head_text.replacen("#include \"pricing.h\"\n\n", "", 1)
            );
            definition_path = cpp;
        }
    }
    base.insert(definition_path.clone(), definition_text);
    if family != Family::Dockerfile {
        base.insert(caller_path.clone(), caller_text);
    }
    let mut change = Files::new();
    change.insert(definition_path.clone(), head_text);
    Repo {
        family,
        base,
        change,
        caller: Some(caller_path),
        definition: definition_path,
        symbol: p.symbol.to_string(),
    }
}

// The same seed with no caller anywhere: the definition changes and nothing,
// in any file, calls or names it. `broken_unrelated` adds a file of the group
// that does not parse and does not spell the name.
pub(crate) fn control(family: Family, broken_unrelated: bool) -> Repo {
    let shape = Shape::default();
    let p = parts(family, &shape);
    let mut base = into_files(p.unrelated.clone());
    for (path, text) in &p.manifests {
        base.insert((*path).to_string(), (*text).to_string());
    }
    let mut definition_base = p.base.clone();
    let mut definition_head = p.head.clone();
    if family == Family::Dockerfile {
        // A second stage nothing reads, so no stage reads the changed one.
        let alone = |text: &str| {
            text.replace(
                "\n\nFROM base AS app\nCOPY app.sh /app.sh\n",
                "\n\nFROM alpine:3.20 AS final\nCMD [\"true\"]\n",
            )
        };
        definition_base = alone(&definition_base);
        definition_head = alone(&definition_head);
    }
    if family == Family::Rust {
        base.insert(
            "src/lib.rs".into(),
            "pub mod pricing;\npub mod util;\n".into(),
        );
    }
    base.insert(p.definition.clone(), definition_base);
    if broken_unrelated {
        let stem = match family {
            Family::Dockerfile => "broken/Dockerfile.broken".to_string(),
            _ => {
                let suffix = p.definition.rsplit_once('.').map_or("", |(_, s)| s);
                format!("broken/unrelated.{suffix}")
            }
        };
        base.insert(stem, format!("{}{}", p.garbage, p.garbage));
    }
    let mut change = Files::new();
    change.insert(p.definition.clone(), definition_head);
    Repo {
        family,
        base,
        change,
        caller: None,
        definition: p.definition,
        symbol: p.symbol.to_string(),
    }
}

// ---- reading an answer ----

// Sections that list what changed, which name the definition's own file and
// so are no evidence of a link.
const LISTINGS: [&str; 5] = ["inventory", "range", "name", "schema", "provenance"];

// What a row of `definitions` says about the definition itself, which is no
// evidence of a caller even where the caller is the definition's own file.
const OWN: [&str; 5] = ["path", "symbol", "lines", "kind", "change"];

fn names(value: &Value, path: &str) -> bool {
    match value {
        Value::String(text) => text == path || text.starts_with(&format!("{path}::")),
        Value::Array(items) => items.iter().any(|item| names(item, path)),
        Value::Object(map) => map.iter().any(|(key, item)| match key.as_str() {
            listed if LISTINGS.contains(&listed) => false,
            "definitions" => item.as_array().into_iter().flatten().any(|row| {
                row.as_object().is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|(field, held)| !OWN.contains(&field.as_str()) && names(held, path))
                })
            }),
            _ => names(item, path),
        }),
        _ => false,
    }
}

// The answer names `path` somewhere other than the list of changed files.
pub(crate) fn names_link(doc: &Value, path: &str) -> bool {
    names(doc, path)
}

// What the answer owns up to not having read: a limit, an unanalysed or
// withheld file, a caller search that says it was partial.
pub(crate) fn fingerprint(doc: &Value) -> String {
    let mut out = format!(
        "limits={} unanalysed={} withheld={}",
        doc["coverage"]["limits"], doc["coverage"]["unanalysed"], doc["inventory"]["withheld"]
    );
    for row in doc["definitions"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n{}::{} missed_in={} gaps={}",
            row["path"], row["symbol"], row["missed_in"], row["gaps"]
        ));
    }
    out
}

pub(crate) fn discloses(doc: &Value) -> bool {
    let listed = |value: &Value| value.as_array().is_some_and(|rows| !rows.is_empty());
    listed(&doc["coverage"]["limits"])
        || doc["coverage"]["unanalysed"]["total"].as_u64() > Some(0)
        || doc["inventory"]["withheld"].as_u64() > Some(0)
        || listed(&doc["spelled"]["rows"])
        || doc["definitions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| listed(&row["missed_in"]) || listed(&row["gaps"]))
}

// The answer says the change reaches nothing.
pub(crate) fn says_empty(doc: &Value) -> bool {
    doc["empty"] == true
}

// ---- asking the binary ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Query {
    Impact,
    Symbol,
}

// The questions asked of a seed: the change, and the definition by name for
// the families `--symbol` names.
pub(crate) fn queries(family: Family) -> Vec<Query> {
    let mut all = vec![Query::Impact];
    if family.answers_symbol() {
        all.push(Query::Symbol);
    }
    all
}

// A seed as a repository on disk, with a facts cache of its own so that no
// run reads what another binary or another seed stored.
pub(crate) struct Planted {
    pub repo: Repo,
    tmp: TempDir,
}

impl Planted {
    pub(crate) fn new(repo: Repo) -> Self {
        let tmp = TempDir::new().expect("tempdir");
        std::fs::create_dir_all(tmp.path().join("repo")).expect("mkdir");
        repo.commit_into(&tmp.path().join("repo"));
        Self { repo, tmp }
    }

    pub(crate) fn work(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    pub(crate) fn run(&self, query: Query, env: &[(&str, &str)]) -> Output {
        let mut command = diffctx();
        command.current_dir(self.work()).arg(".");
        match query {
            Query::Impact => command.args(["--diff", RANGE, "--mode", "impact"]),
            Query::Symbol => command.args(["--symbol", self.repo.symbol_arg().as_str()]),
        };
        command
            .args(["-f", "json", "-q", "--unfold"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("DIFFCTX_NO_MARKER", "1")
            .env("DIFFCTX_CACHE_DIR", self.tmp.path().join("cache"));
        for (key, value) in env {
            command.env(key, value);
        }
        command.output().expect("spawn diffctx")
    }

    // The JSON answer of a run that succeeded.
    pub(crate) fn ask(&self, query: Query, env: &[(&str, &str)]) -> Result<Value, String> {
        let out = self.run(query, env);
        if !out.status.success() {
            return Err(format!(
                "{query:?}: exit {:?}: {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| {
            format!(
                "{query:?}: not JSON ({e}): {}",
                String::from_utf8_lossy(&out.stdout)
            )
        })
    }
}
